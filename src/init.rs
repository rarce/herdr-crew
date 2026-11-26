//! Initial crew setup: choose a software workflow, preview it, then create a validated config.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Write};
use std::path::Path;

use serde::Serialize;

use crate::agent::{AgentOptions, AgyOptions, ClaudeOptions, CodexOptions, Kind, PiOptions};
use crate::models::{self, Duty, Profile};
use crate::{Fail, config, git};

pub(crate) const MAX_PREFIX: usize = 21; // Leave room for "-research-a" within a 32-byte agent name.
/// Each generated-file rule with the paths it must ignore, to ask Git whether the repository
/// already covers them with rules of its own. A directory rule needs both the directory, so
/// that a rule for some of its files such as `*.txt` is not enough, and a file of the shape
/// setup writes there, so that a whitelist's `*` does not hide a re-included `!*.txt`.
const IGNORE_RULES: &[(&str, &[&str])] = &[
    ("/.herdr/status.json", &[".herdr/status.json"]),
    ("/.herdr/status.json.tmp", &[".herdr/status.json.tmp"]),
    ("/.herdr/.herdr-crew-*.tmp", &[".herdr/.herdr-crew-1-0.tmp"]),
    ("/.herdr/status.schema.json", &[".herdr/status.schema.json"]),
    (
        "/.herdr/prompts/",
        &[".herdr/prompts/", ".herdr/prompts/role.txt"],
    ),
    (
        "/.herdr/codex/",
        &[".herdr/codex/", ".herdr/codex/bindings/role.json"],
    ),
];
const WORKTREES_RULE: (&str, &[&str]) = ("/.worktrees/", &[".worktrees/", ".worktrees/role/file"]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Solo,
    Review,
    Parallel,
    Research,
}

impl Preset {
    pub(crate) const ALL: [Self; 4] = [Self::Solo, Self::Review, Self::Parallel, Self::Research];

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "solo" => Some(Self::Solo),
            "review" => Some(Self::Review),
            "parallel" => Some(Self::Parallel),
            "research" => Some(Self::Research),
            _ => None,
        }
    }

    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Solo => "solo",
            Self::Review => "review",
            Self::Parallel => "parallel",
            Self::Research => "research",
        }
    }

    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::Solo => {
                "Solo developer (1 session): focused fixes and sequential work; start here if unsure"
            }
            Self::Review => {
                "Reviewed delivery (3 sessions): lead, developer, reviewer; implement -> review -> integrate"
            }
            Self::Parallel => {
                "Parallel implementation (4 sessions): lead, two developers, reviewer; separate components in worktrees"
            }
            Self::Research => {
                "Investigation (3 sessions): lead and two researchers; independent hypotheses -> evidence -> one decision"
            }
        }
    }

    pub(crate) fn roles(self) -> &'static [(&'static str, Duty)] {
        match self {
            Self::Solo => &[("dev", Duty::Solo)],
            Self::Review => &[
                ("lead", Duty::Lead),
                ("dev", Duty::Developer),
                ("reviewer", Duty::Reviewer),
            ],
            Self::Parallel => &[
                ("lead", Duty::Lead),
                ("dev-a", Duty::Developer),
                ("dev-b", Duty::Developer),
                ("reviewer", Duty::Reviewer),
            ],
            Self::Research => &[
                ("lead", Duty::Lead),
                ("research-a", Duty::Researcher),
                ("research-b", Duty::Researcher),
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Topology {
    Tabs,
    Tree,
}

impl Topology {
    pub(crate) const ALL: [Self; 2] = [Self::Tabs, Self::Tree];

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "tabs" => Some(Self::Tabs),
            "tree" => Some(Self::Tree),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Tabs => "tabs",
            Self::Tree => "tree",
        }
    }

    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::Tabs => "independent Herdr tabs per role with optional worktrees (default)",
            Self::Tree => {
                "1 coordinator + native subagents in shared checkout (needs features.multi_agent_v2 = true)"
            }
        }
    }
}

pub(crate) const AGY_TREE_BASE_ERROR: &str =
    "--base does not apply to multi-role agy crews (agy uses native tree mode without [worktrees])";
pub(crate) const AGY_TABS_ERROR: &str =
    "--topology tabs does not apply to multi-role agy crews (agy uses native tree mode)";
pub(crate) const TREE_PRESET_ERROR: &str =
    "--topology tree applies only to multi-role presets (review, parallel, research)";
pub(crate) const TREE_AGENT_ERROR: &str =
    "--topology tree applies only to multi-role codex or agy crews";
pub(crate) const TREE_BASE_ERROR: &str =
    "--base does not apply with --topology tree (tree mode does not use [worktrees])";
pub(crate) const TREE_BASE_INTERACTIVE_ERROR: &str =
    "--base does not apply with --topology tree; choose tabs or omit --base";

pub(crate) fn uses_agy_tree(agent: Kind, preset: Preset) -> bool {
    agent == Kind::Agy && preset != Preset::Solo
}

pub(crate) fn supports_codex_tree(agent: Kind, preset: Preset) -> bool {
    agent == Kind::Codex && preset != Preset::Solo
}

pub(crate) fn effective_topology(agent: Kind, preset: Preset, chosen: Topology) -> Topology {
    if uses_agy_tree(agent, preset) {
        Topology::Tree
    } else if supports_codex_tree(agent, preset) {
        chosen
    } else {
        Topology::Tabs
    }
}

pub(crate) fn is_codex_tree(agent: Kind, preset: Preset, topology: Topology) -> bool {
    supports_codex_tree(agent, preset) && topology == Topology::Tree
}

pub(crate) fn configured_roles(
    agent: Kind,
    preset: Preset,
    topology: Topology,
) -> &'static [(&'static str, Duty)] {
    if effective_topology(agent, preset, topology) == Topology::Tree {
        &preset.roles()[..1]
    } else {
        preset.roles()
    }
}

#[derive(Default)]
pub struct Options {
    pub agent: Option<crate::agent::Kind>,
    pub preset: Option<Preset>,
    pub topology: Option<Topology>,
    pub models: Option<Profile>,
    pub name: Option<String>,
    pub base: Option<String>,
    pub shared_checkout: bool,
    pub yes: bool,
    pub no_ignore: bool,
    /// Use the line-by-line wizard even in a terminal.
    pub no_tui: bool,
}

/// Explicit model and/or effort override for one role during setup.
/// `Some(None)` forces the agent's own default (`inherit`), `Some(Some(value))` sets an explicit
/// value, and `None` keeps what the selected `Profile` picks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoleModelOverride {
    pub model: Option<Option<String>>,
    pub effort: Option<Option<String>>,
}

pub struct Selection {
    pub agent: crate::agent::Kind,
    pub name: String,
    pub preset: Preset,
    pub topology: Topology,
    pub profile: Profile,
    pub overrides: BTreeMap<String, RoleModelOverride>,
    pub base: Option<String>,
    /// In `agy_tree` mode, whether implementer workers use isolated native worktrees (`Workspace: "share"`)
    /// rather than the shared checkout (`Workspace: "inherit"`).
    pub tree_isolated: bool,
    pub ignore: bool,
}

impl Selection {
    pub(crate) fn is_agy_tree(&self) -> bool {
        uses_agy_tree(self.agent, self.preset)
    }

    pub(crate) fn is_codex_tree(&self) -> bool {
        is_codex_tree(self.agent, self.preset, self.topology)
    }

    pub(crate) fn is_tree(&self) -> bool {
        self.is_agy_tree() || self.is_codex_tree()
    }

    /// Whether any configured role's effective model or effort differs from the selected profile.
    pub fn is_customized(&self) -> bool {
        configured_roles(self.agent, self.preset, self.topology)
            .iter()
            .any(|(suffix, duty)| {
                role_options(self.agent, self.profile, *duty, self.overrides.get(*suffix))
                    != self.profile.options(self.agent, *duty)
            })
    }
}

/// Extracts `(model, effort)` from an `AgentOptions`.
pub(crate) fn options_model_effort(options: &AgentOptions) -> (Option<String>, Option<String>) {
    match options {
        AgentOptions::Claude(o) => (o.model.clone(), o.effort.clone()),
        AgentOptions::Codex(o) => (o.model.clone(), o.reasoning_effort.clone()),
        AgentOptions::Pi(o) => (o.model.clone(), o.thinking.clone()),
        AgentOptions::Agy(o) => (o.model.clone(), o.effort.clone()),
    }
}

/// Computes the effective `AgentOptions` for one role given the selected agent, profile, duty,
/// and any explicit per-role override.
pub(crate) fn role_options(
    agent: Kind,
    profile: Profile,
    duty: Duty,
    override_: Option<&RoleModelOverride>,
) -> AgentOptions {
    let (base_model, base_effort) = options_model_effort(&profile.options(agent, duty));
    let model = match override_.and_then(|o| o.model.as_ref()) {
        Some(explicit) => explicit.clone(),
        None => base_model,
    };
    let effort = match override_.and_then(|o| o.effort.as_ref()) {
        Some(explicit) => explicit.clone(),
        None => base_effort,
    };
    match agent {
        Kind::Claude => AgentOptions::Claude(ClaudeOptions {
            model,
            effort,
            ..Default::default()
        }),
        Kind::Codex => AgentOptions::Codex(CodexOptions {
            model,
            reasoning_effort: effort,
            ..Default::default()
        }),
        Kind::Pi => AgentOptions::Pi(PiOptions {
            model,
            thinking: effort,
            ..Default::default()
        }),
        Kind::Agy => AgentOptions::Agy(AgyOptions {
            model,
            effort,
            ..Default::default()
        }),
    }
}

/// Validates a role's `AgentOptions` against the agent's own rules and launch constraints.
pub(crate) fn validate_role_options(options: &AgentOptions) -> Result<(), String> {
    let mut problems = match options {
        AgentOptions::Claude(o) => o.problems(),
        AgentOptions::Codex(o) => o.problems(),
        AgentOptions::Pi(o) => {
            let mut p = o.problems();
            if let Some(lp) = o.launch_problem() {
                p.push(lp.into());
            }
            p
        }
        AgentOptions::Agy(o) => {
            let mut p = o.problems();
            if let Some(lp) = o.launch_problem() {
                p.push(lp.into());
            }
            p
        }
    };
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.remove(0))
    }
}

#[derive(Serialize)]
struct Template {
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<crate::agent::Kind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    codex_tree: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agy_tree: Option<bool>,
    version: i64,
    common_prompt: String,
    start_message: &'static str,
    workspace: Workspace,
    board: Board,
    #[serde(skip_serializing_if = "Option::is_none")]
    worktrees: Option<Worktrees>,
    roles: Vec<Role>,
}

#[derive(Serialize)]
struct Workspace {
    label: String,
}

#[derive(Serialize)]
struct Board {
    tab: String,
    writer: String,
}

#[derive(Serialize)]
struct Worktrees {
    dir: &'static str,
    base: String,
}

#[derive(Serialize)]
struct Role {
    name: String,
    prompt: String,
    worktree: bool,
    extra: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    claude: Option<ClaudeOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    codex: Option<CodexOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pi: Option<PiOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agy: Option<AgyOptions>,
}

pub(crate) fn valid_name(name: &str) -> bool {
    name.len() <= MAX_PREFIX && config::is_agent_name(name)
}

pub(crate) fn suggested_name(root: &Path) -> String {
    let folder = root.file_name().unwrap_or_default().to_string_lossy();
    let mut name = String::new();
    for ch in folder.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            name.push(ch);
        } else if !name.ends_with('-') {
            name.push('-');
        }
    }
    let mut name = name.trim_matches('-').to_string();
    if !name.starts_with(|c: char| c.is_ascii_lowercase()) {
        name = format!("crew-{name}");
    }
    name.truncate(MAX_PREFIX);
    name.trim_end_matches('-').to_string()
}

fn ask(
    input: &mut impl BufRead,
    output: &mut impl Write,
    label: &str,
    default: Option<&str>,
) -> Result<String, Fail> {
    if let Some(default) = default {
        write!(output, "{label} [{default}]: ")
    } else {
        write!(output, "{label}: ")
    }
    .map_err(io_fail)?;
    output.flush().map_err(io_fail)?;
    let mut line = String::new();
    if input.read_line(&mut line).map_err(io_fail)? == 0 {
        return Err(Fail::run(
            "input ended; no configuration created. Use init --yes for non-interactive setup",
        ));
    }
    let value = line.trim();
    Ok(if value.is_empty() {
        default.unwrap_or_default()
    } else {
        value
    }
    .to_string())
}

fn yes_no(
    input: &mut impl BufRead,
    output: &mut impl Write,
    label: &str,
    default: bool,
) -> Result<bool, Fail> {
    loop {
        let answer = ask(
            input,
            output,
            label,
            Some(if default { "Y/n" } else { "y/N" }),
        )?;
        match answer.to_ascii_lowercase().as_str() {
            "y/n" => return Ok(default),
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => writeln!(output, "Enter y or n.").map_err(io_fail)?,
        }
    }
}

fn io_fail(error: io::Error) -> Fail {
    Fail::run(error.to_string())
}

fn validate_options(root: &Path, options: &Options) -> Result<(), Fail> {
    if let Some(name) = &options.name
        && !valid_name(name)
    {
        return Err(Fail::usage(format!(
            "--name must match [a-z][a-z0-9_-]{{0,{}}}",
            MAX_PREFIX - 1
        )));
    }
    let effective_preset = options
        .preset
        .or_else(|| options.yes.then_some(Preset::Solo));
    let effective_agent = options
        .agent
        .or_else(|| options.yes.then_some(Kind::default()));
    if options.shared_checkout && effective_preset.is_some_and(|p| p != Preset::Review) {
        return Err(Fail::usage(
            "--shared-checkout applies only to the review preset; parallel requires worktrees",
        ));
    }
    if options.topology == Some(Topology::Tree) {
        if effective_agent.is_some_and(|a| !matches!(a, Kind::Codex | Kind::Agy)) {
            return Err(Fail::usage(TREE_AGENT_ERROR));
        }
        if effective_preset == Some(Preset::Solo) {
            return Err(Fail::usage(TREE_PRESET_ERROR));
        }
        if options.base.is_some() {
            return Err(Fail::usage(TREE_BASE_ERROR));
        }
    }
    if options.topology == Some(Topology::Tabs)
        && effective_agent == Some(Kind::Agy)
        && effective_preset.is_some_and(|p| p != Preset::Solo)
    {
        return Err(Fail::usage(AGY_TABS_ERROR));
    }
    if options.base.is_some() {
        if options.shared_checkout
            || effective_preset.is_some_and(|p| !matches!(p, Preset::Review | Preset::Parallel))
        {
            return Err(Fail::usage(
                "--base requires a review or parallel preset using worktrees",
            ));
        }
        if effective_agent == Some(Kind::Agy) {
            return Err(Fail::usage(AGY_TREE_BASE_ERROR));
        }
        if !git::has_remote(root).map_err(Fail::run)? {
            return Err(no_remote());
        }
    }
    Ok(())
}

fn choose(
    root: &Path,
    options: &Options,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<Selection, Fail> {
    validate_options(root, options)?;
    let explicit_tree = options.topology == Some(Topology::Tree);
    let explicit_tabs = options.topology == Some(Topology::Tabs);
    let default_name = suggested_name(root);
    let name = if let Some(name) = &options.name {
        name.clone()
    } else if options.yes {
        default_name
    } else {
        loop {
            let name = ask(
                input,
                output,
                "Project name / role prefix",
                Some(&default_name),
            )?;
            if valid_name(&name) {
                break name;
            }
            writeln!(
                output,
                "Use 1-{MAX_PREFIX} characters: start with a-z, then a-z, 0-9, _ or -."
            )
            .map_err(io_fail)?;
        }
    };
    if !valid_name(&name) {
        return Err(Fail::usage(format!(
            "--name must match [a-z][a-z0-9_-]{{0,{}}}",
            MAX_PREFIX - 1
        )));
    }
    let preset = if let Some(preset) = options.preset {
        preset
    } else if options.yes {
        Preset::Solo
    } else {
        writeln!(
            output,
            "\nChoose a workflow. Add sessions when the task benefits from them:"
        )
        .map_err(io_fail)?;
        for (i, preset) in Preset::ALL.iter().enumerate() {
            writeln!(output, "  {}. {}", i + 1, preset.description()).map_err(io_fail)?;
        }
        let default_workflow = if explicit_tree { "2" } else { "1" };
        loop {
            let choice = ask(input, output, "Workflow", Some(default_workflow))?;
            if let Some(preset) = Preset::parse(&choice).or_else(|| {
                choice
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| i.checked_sub(1))
                    .and_then(|i| Preset::ALL.get(i).copied())
            }) {
                if explicit_tree && preset == Preset::Solo {
                    writeln!(output, "{TREE_PRESET_ERROR}.").map_err(io_fail)?;
                    continue;
                }
                if explicit_tabs && options.agent == Some(Kind::Agy) && preset != Preset::Solo {
                    writeln!(output, "{AGY_TABS_ERROR}.").map_err(io_fail)?;
                    continue;
                }
                break preset;
            }
            writeln!(output, "Enter 1-4 or solo, review, parallel, research.").map_err(io_fail)?;
        }
    };
    if options.shared_checkout && preset != Preset::Review {
        return Err(Fail::usage(
            "--shared-checkout applies only to the review preset; parallel requires worktrees",
        ));
    }
    if explicit_tree && preset == Preset::Solo {
        return Err(Fail::usage(TREE_PRESET_ERROR));
    }
    if explicit_tabs && options.agent == Some(Kind::Agy) && preset != Preset::Solo {
        return Err(Fail::usage(AGY_TABS_ERROR));
    }
    if options.base.is_some()
        && (options.shared_checkout || !matches!(preset, Preset::Review | Preset::Parallel))
    {
        return Err(Fail::usage(
            "--base requires a review or parallel preset using worktrees",
        ));
    }
    // Check remote before asking for the agent when flags already require git worktrees.
    let known_tabs = !explicit_tree
        && (options
            .agent
            .is_some_and(|a| matches!(a, Kind::Claude | Kind::Pi))
            || (options.agent == Some(Kind::Codex) && (explicit_tabs || options.yes))
            || (options.yes && options.agent.is_none())
            || options.base.is_some());
    let flags_need_worktrees = known_tabs
        && (preset == Preset::Parallel
            || (preset == Preset::Review
                && !options.shared_checkout
                && (options.base.is_some() || options.yes)));
    if flags_need_worktrees && !git::has_remote(root).map_err(Fail::run)? {
        return Err(no_remote());
    }
    let agent = if let Some(agent) = options.agent {
        agent
    } else if options.yes {
        crate::agent::Kind::default()
    } else {
        let names = crate::agent::Kind::ALL.map(crate::agent::Kind::as_str);
        let label = format!("Agent ({})", names.join(", "));
        let default_agent = if explicit_tree {
            Kind::Codex.as_str()
        } else {
            crate::agent::Kind::default().as_str()
        };
        loop {
            let choice = ask(input, output, &label, Some(default_agent))?;
            if let Some(agent) = crate::agent::Kind::parse(&choice.to_ascii_lowercase()) {
                if explicit_tree && !matches!(agent, Kind::Codex | Kind::Agy) {
                    writeln!(output, "{TREE_AGENT_ERROR}.").map_err(io_fail)?;
                    continue;
                }
                if explicit_tabs && uses_agy_tree(agent, preset) {
                    writeln!(output, "{AGY_TABS_ERROR}.").map_err(io_fail)?;
                    continue;
                }
                if options.base.is_some() && uses_agy_tree(agent, preset) {
                    writeln!(output, "{AGY_TREE_BASE_ERROR}.").map_err(io_fail)?;
                    continue;
                }
                break agent;
            }
            writeln!(output, "Enter one of: {}.", names.join(", ")).map_err(io_fail)?;
        }
    };
    if explicit_tree && !matches!(agent, Kind::Codex | Kind::Agy) {
        return Err(Fail::usage(TREE_AGENT_ERROR));
    }
    if explicit_tabs && uses_agy_tree(agent, preset) {
        return Err(Fail::usage(AGY_TABS_ERROR));
    }
    if options.base.is_some() && uses_agy_tree(agent, preset) {
        return Err(Fail::usage(AGY_TREE_BASE_ERROR));
    }
    let topology = if uses_agy_tree(agent, preset) {
        Topology::Tree
    } else if supports_codex_tree(agent, preset) {
        if let Some(topology) = options.topology {
            topology
        } else if options.yes {
            Topology::Tabs
        } else {
            writeln!(output, "\nChoose a topology for Codex roles:").map_err(io_fail)?;
            for (i, topology) in Topology::ALL.iter().enumerate() {
                writeln!(
                    output,
                    "  {}. {}: {}",
                    i + 1,
                    topology.as_str(),
                    topology.description()
                )
                .map_err(io_fail)?;
            }
            loop {
                let choice = ask(input, output, "Topology", Some("1"))?;
                if let Some(topology) =
                    Topology::parse(&choice.to_ascii_lowercase()).or_else(|| {
                        choice
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| i.checked_sub(1))
                            .and_then(|i| Topology::ALL.get(i).copied())
                    })
                {
                    if topology == Topology::Tree && options.base.is_some() {
                        writeln!(output, "{TREE_BASE_INTERACTIVE_ERROR}.").map_err(io_fail)?;
                        continue;
                    }
                    break topology;
                }
                writeln!(output, "Enter 1-2 or tabs, tree.").map_err(io_fail)?;
            }
        }
    } else {
        Topology::Tabs
    };
    if topology == Topology::Tree && options.base.is_some() {
        return Err(Fail::usage(TREE_BASE_ERROR));
    }
    if topology == Topology::Tabs {
        let tabs_need_worktrees = preset == Preset::Parallel
            || (preset == Preset::Review
                && !options.shared_checkout
                && (options.base.is_some() || options.yes));
        if tabs_need_worktrees && !git::has_remote(root).map_err(Fail::run)? {
            return Err(no_remote());
        }
    }
    let profile = if let Some(profile) = options.models {
        profile
    } else if options.yes {
        Profile::default()
    } else {
        writeln!(output, "\nChoose models and effort per role:").map_err(io_fail)?;
        for (i, profile) in Profile::ALL.iter().enumerate() {
            writeln!(
                output,
                "  {}. {}: {}",
                i + 1,
                profile.as_str(),
                profile.description()
            )
            .map_err(io_fail)?;
        }
        let default = Profile::ALL
            .iter()
            .position(|p| *p == Profile::default())
            .unwrap()
            + 1;
        loop {
            let choice = ask(input, output, "Models", Some(&default.to_string()))?;
            if let Some(profile) = Profile::parse(&choice.to_ascii_lowercase()).or_else(|| {
                choice
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| i.checked_sub(1))
                    .and_then(|i| Profile::ALL.get(i).copied())
            }) {
                break profile;
            }
            writeln!(output, "Enter 1-4 or inherit, economy, balanced, quality.")
                .map_err(io_fail)?;
        }
    };
    let mut overrides = BTreeMap::new();
    if !options.yes
        && options.models.is_none()
        && yes_no(
            input,
            output,
            "Customize model or effort for any role?",
            false,
        )?
    {
        let efforts_hint = models::efforts(agent).join(", ");
        let effort_key = models::effort_key(agent);
        for (suffix, duty) in configured_roles(agent, preset, topology) {
            let (default_model, default_effort) =
                options_model_effort(&profile.options(agent, *duty));
            writeln!(output, "\nRole {name}-{suffix} ({}):", duty.as_str()).map_err(io_fail)?;
            let catalog = models::catalog(agent);
            if !catalog.is_empty() {
                let names: Vec<&str> = catalog.iter().map(|(m, _)| *m).collect();
                writeln!(output, "  Suggested models: {}", names.join(", ")).map_err(io_fail)?;
            }
            loop {
                let model_answer = ask(
                    input,
                    output,
                    "  Model (name or 'inherit' for agent default)",
                    Some(default_model.as_deref().unwrap_or("inherit")),
                )?;
                let model = (!model_answer.eq_ignore_ascii_case("inherit")
                    && !model_answer.is_empty())
                .then_some(model_answer);
                let effort_label = format!("  {effort_key} ({efforts_hint}, or 'inherit')");
                let effort_answer = ask(
                    input,
                    output,
                    &effort_label,
                    Some(default_effort.as_deref().unwrap_or("inherit")),
                )?;
                let effort = (!effort_answer.eq_ignore_ascii_case("inherit")
                    && !effort_answer.is_empty())
                .then_some(effort_answer);
                let candidate = RoleModelOverride {
                    model: (model != default_model).then_some(model),
                    effort: (effort != default_effort).then_some(effort),
                };
                let effective = role_options(agent, profile, *duty, Some(&candidate));
                match validate_role_options(&effective) {
                    Ok(()) => {
                        if candidate.model.is_some() || candidate.effort.is_some() {
                            overrides.insert((*suffix).to_string(), candidate);
                        }
                        break;
                    }
                    Err(problem) => {
                        writeln!(output, "  Invalid setting: {problem}").map_err(io_fail)?;
                    }
                }
            }
        }
    }
    let (base, tree_isolated) = if uses_agy_tree(agent, preset) {
        let tree_isolated = match preset {
            Preset::Parallel => true,
            Preset::Review if options.shared_checkout => false,
            Preset::Review if options.yes => true,
            Preset::Review => yes_no(
                input,
                output,
                "Use isolated native worktrees (Workspace: \"share\") for implementation workers?",
                true,
            )?,
            _ => false,
        };
        (None, tree_isolated)
    } else if is_codex_tree(agent, preset, topology) {
        (None, false)
    } else {
        let suggested_base = git::default_base(root);
        let isolated = match preset {
            Preset::Parallel => true,
            Preset::Review if options.shared_checkout => false,
            Preset::Review if options.base.is_some() || options.yes => true,
            Preset::Review => yes_no(
                input,
                output,
                "Use separate worktrees for development and review?",
                suggested_base.is_some(),
            )?,
            _ => false,
        };
        let base = if isolated {
            // Also reached after the interactive review answers yes to worktrees.
            if !git::has_remote(root).map_err(Fail::run)? {
                return Err(no_remote());
            }
            writeln!(output, "Worktrees start from a remote branch, not uncommitted local changes. No fetch runs during setup.").map_err(io_fail)?;
            // An explicit base, from --base or the --yes default, is used once; the prompt
            // follows when it is declined. Without --yes, an unfetched base needs confirmation.
            let mut given = options.base.clone();
            if given.is_none() && options.yes {
                given = Some(suggested_base.clone().ok_or_else(|| Fail::usage("no remote branch found; set --base REMOTE/BRANCH, or use review --shared-checkout"))?);
            }
            loop {
                let explicit = given.take();
                let base = match &explicit {
                    Some(base) => base.clone(),
                    None => ask(
                        input,
                        output,
                        "Worktree base (configured remote/branch)",
                        suggested_base.as_deref(),
                    )?,
                };
                if let Err(error) = git::validate_base(root, &base) {
                    if explicit.is_some() {
                        return Err(Fail::usage(error));
                    }
                    writeln!(output, "{error}").map_err(io_fail)?;
                    continue;
                }
                if git::knows_base(root, &base) {
                    break Some(base);
                }
                writeln!(output, "{}", unfetched(&base)).map_err(io_fail)?;
                if options.yes || yes_no(input, output, &format!("Use {base} anyway?"), false)? {
                    break Some(base);
                }
            }
        } else {
            None
        };
        (base, false)
    };
    let ignore = !options.no_ignore
        && (options.yes
            || yes_no(
                input,
                output,
                "Ignore generated crew files in .gitignore?",
                true,
            )?);
    Ok(Selection {
        agent,
        name,
        preset,
        topology,
        profile,
        overrides,
        base,
        tree_isolated,
        ignore,
    })
}

pub(crate) fn no_remote() -> Fail {
    Fail::usage(
        "worktrees require a configured Git remote; configure one first, or use solo, research, or review --shared-checkout",
    )
}

pub(crate) fn unfetched(base: &str) -> String {
    format!(
        "Warning: {base} is not among the fetched remote branches. Check its name; `up` fetches it and stops if the remote lacks it."
    )
}

fn duty_responsibilities(preset: Preset, kind: Duty) -> &'static str {
    match kind {
        Duty::Solo => {
            "Own the full task: clarify acceptance, reproduce or understand the problem, implement a focused change, and validate it using the repository's actual commands. Keep investigation, editing and test feedback in one loop. Report the result and remaining limitations to the user."
        }
        Duty::Lead if preset == Preset::Research => {
            "Define a common reproduction and independent hypotheses for research-a and research-b. Assign a bounded question, expected evidence and stopping condition to each researcher. Compare their reports, resolve contradictions with observations, and select one implementation decision. Perform or assign the chosen fix after consolidation, and validate the result before reporting it to the user."
        }
        Duty::Lead => {
            "Coordinate the user's task. Define acceptance, dependencies, base commit, file ownership and a bounded assignment for each worker. Agree interfaces before parallel edits. Collect evidence and exact commits, request corrections, and integrate only reviewed deliveries in dependency order. Validate the combined result before accepting it. Handle requirements and architecture decisions yourself unless a separate investigation is useful."
        }
        Duty::Developer => {
            "Implement only your assigned component. Reproduce relevant failures, preserve interfaces and file ownership, make focused changes, and run relevant reproduction and regression checks. Deliver the exact commit, affected files, commands and results, and limitations to the coordinator. Address reviewer findings; do not integrate another worker's changes yourself."
        }
        Duty::Reviewer => {
            "Review the exact delivered commit against its acceptance criteria. Check correctness, regressions, interfaces and missing validation. Return actionable findings with file locations, consequences and reproduction steps. Request changes or explicitly accept with evidence. Do not silently fix the implementation; the developer owns corrections and the coordinator owns integration."
        }
        Duty::Researcher => {
            "Investigate only the assigned hypothesis or question. Gather commands, observations, relevant code locations and primary references. Explain evidence for and against the hypothesis, uncertainty and a recommended next step. Report to the coordinator before any implementation. Do not edit source files, change Git state, or start shared services."
        }
    }
}

fn render_agy_tree_manifest(selection: &Selection, coordinator: &str) -> String {
    let mut lines = vec![
        "Manifest workers (spawn only on explicit delegation via invoke_subagent with these full role instructions):".to_string(),
    ];
    for (suffix, duty) in &selection.preset.roles()[1..] {
        let type_name = match duty {
            Duty::Developer => "self",
            Duty::Reviewer | Duty::Researcher => "research",
            Duty::Solo | Duty::Lead => unreachable!("manifest workers are non-lead roles"),
        };
        let workspace = match duty {
            Duty::Developer if selection.tree_isolated => "share",
            _ => "inherit",
        };
        let responsibilities = duty_responsibilities(selection.preset, *duty);
        let isolation = match (duty, workspace) {
            (Duty::Developer, "share") => {
                "Works in an isolated native worktree (Workspace: \"share\"). Create or use a focused task branch before committing, preserve interfaces and file ownership, and never reset or delete another session's branch."
            }
            (Duty::Developer, _) => {
                "Shares the main checkout (Workspace: \"inherit\"). Preserve the user's changes and do not switch or reset branches while other sessions use it. Only the assigned implementer edits source files; coordinate validation commands that generate files with the coordinator."
            }
            _ => {
                "Read-only in the shared checkout (Workspace: \"inherit\"). Do not edit source files, change Git state, or start shared services."
            }
        };
        lines.push(format!(
            "- {suffix} (TypeName: \"{type_name}\", Workspace: \"{workspace}\"):\n  \
             Responsibilities: {responsibilities}\n  \
             Ownership and isolation: {isolation}\n  \
             Reporting: Report progress, blockers, evidence, exact commits, validation commands/results and limitations to {coordinator} via your native subagent reply; never edit {{{{STATUS}}}} directly."
        ));
    }
    lines.join("\n")
}

fn render_codex_tree_manifest(selection: &Selection, coordinator: &str) -> String {
    let mut lines = vec![
        "Manifest workers (spawn only on explicit delegation via spawn_agent with fork_turns = \"none\" and these full role instructions; reuse returned canonical paths via followup_task and inspect loaded threads with list_agents):".to_string(),
    ];
    for (suffix, duty) in &selection.preset.roles()[1..] {
        let task_name = suffix.replace('-', "_");
        let responsibilities = duty_responsibilities(selection.preset, *duty);
        let isolation = match duty {
            Duty::Developer if selection.preset == Preset::Parallel => {
                "Shares the main checkout with other workers. Preserve the user's changes, edit only your agreed disjoint files, and never switch or reset branches while other sessions use the checkout. Coordinate build or test commands that write shared artifacts with the coordinator."
            }
            Duty::Developer => {
                "Shares the main checkout. Preserve the user's changes and do not switch or reset branches while other sessions use it. Only the assigned implementer edits source files; coordinate validation commands that generate files with the coordinator."
            }
            _ => {
                "Read-only in the shared checkout. Do not edit source files, change Git state, or start shared services."
            }
        };
        lines.push(format!(
            "- {suffix} (task_name: \"{task_name}\"):\n  \
             Responsibilities: {responsibilities}\n  \
             Ownership and isolation: {isolation}\n  \
             Reporting: Report progress, blockers, evidence, exact commits, validation commands/results and limitations to {coordinator} in your native final response; never edit {{{{STATUS}}}} directly."
        ));
    }
    lines.join("\n")
}

pub(crate) fn render(selection: &Selection, root: &Path) -> Result<String, Fail> {
    let coordinator = format!("{}-{}", selection.name, selection.preset.roles()[0].0);
    let is_agy_tree = selection.is_agy_tree();
    let is_codex_tree = selection.is_codex_tree();
    let is_tree = selection.is_tree();
    let mut roles = Vec::new();
    if is_tree {
        let (suffix, kind) = selection.preset.roles()[0];
        let responsibilities = duty_responsibilities(selection.preset, kind);
        let manifest = if is_agy_tree {
            render_agy_tree_manifest(selection, &coordinator)
        } else {
            render_codex_tree_manifest(selection, &coordinator)
        };
        let (mut codex, mut agy) = (None, None);
        match role_options(
            selection.agent,
            selection.profile,
            kind,
            selection.overrides.get(suffix),
        ) {
            AgentOptions::Codex(o) if o != CodexOptions::default() => codex = Some(o),
            AgentOptions::Agy(o) if o != AgyOptions::default() => agy = Some(o),
            _ => {}
        }
        let delegation = if is_codex_tree {
            "Delegate to manifest workers via spawn_agent (fork_turns = \"none\"), reuse returned paths via followup_task, and check loaded threads with list_agents.\n"
        } else {
            ""
        };
        roles.push(Role {
            name: format!("{}-{suffix}", selection.name),
            prompt: format!(
                "You are {{{{NAME}}}} in {{{{REPO}}}}.\n\
                 {responsibilities}\n\
                 Report progress, blockers and accepted results to the user.\n\
                 You coordinate in the main checkout and integrate reviewed deliveries. Preserve the user's changes and do not switch or reset branches unexpectedly.\n\
                 {delegation}\n\
                 {manifest}\n"
            ),
            worktree: false,
            extra: false,
            claude: None,
            codex,
            pi: None,
            agy,
        });
    } else {
        for (suffix, kind) in selection.preset.roles() {
            let worktree =
                selection.base.is_some() && matches!(kind, Duty::Developer | Duty::Reviewer);
            let responsibilities = duty_responsibilities(selection.preset, *kind);
            let reporting = if matches!(kind, Duty::Solo | Duty::Lead) {
                "Report progress, blockers and accepted results to the user.".to_string()
            } else {
                format!(
                    "Report progress, blockers and deliveries to {coordinator}; ask the user to relay them if no direct channel is available."
                )
            };
            let isolation = if worktree {
                "You work in an isolated worktree. New worktrees start with detached HEAD from the configured remote base and do not inherit the lead's local edits. Check the assigned base and existing changes, then create a task branch before committing. For review, inspect the delivered commit and validate that exact version in your worktree without discarding local work. Git references are shared: never reset or delete another session's branch."
            } else {
                "You share the main checkout. Preserve the user's changes and do not switch or reset branches while other sessions use it. Only the assigned implementer edits source files; review and research are read-only. Coordinate validation commands that generate files with the implementer."
            };
            let (mut claude, mut codex, mut pi, mut agy) = (None, None, None, None);
            match role_options(
                selection.agent,
                selection.profile,
                *kind,
                selection.overrides.get(*suffix),
            ) {
                AgentOptions::Claude(o) if o != ClaudeOptions::default() => claude = Some(o),
                AgentOptions::Codex(o) if o != CodexOptions::default() => codex = Some(o),
                AgentOptions::Pi(o) if o != PiOptions::default() => pi = Some(o),
                AgentOptions::Agy(o) if o != AgyOptions::default() => agy = Some(o),
                _ => {}
            }
            roles.push(Role {
                name: format!("{}-{suffix}", selection.name),
                prompt: format!("You are {{{{NAME}}}} in {{{{REPO}}}}.\n{responsibilities}\n{reporting}\n{isolation}\n"),
                worktree,
                extra: worktree && matches!(kind, Duty::Developer),
                claude,
                codex,
                pi,
                agy,
            });
        }
    }
    let workflow = match selection.preset {
        Preset::Solo => {
            "Understand -> implement -> validate -> deliver. Use a separate review only when it answers a concrete question."
        }
        Preset::Review => {
            "Acceptance -> implementation -> review -> corrections -> integration -> combined validation. The reviewer waits for a specific commit; sessions need not work simultaneously."
        }
        Preset::Parallel => {
            "Acceptance and common base -> agreed interfaces -> independent components -> commit review -> ordered integration -> combined validation. Assign dev-a and dev-b distinct files; give shared models, schemas and lockfiles one owner."
        }
        Preset::Research => {
            "Common reproduction -> independent hypotheses -> evidence reports -> comparison by the lead -> one implementation decision. Researchers do not produce competing patches by default; the lead assigns or performs the chosen fix afterwards."
        }
    };
    let expansion = if is_tree {
        "Spawn manifest subagents only on explicit delegation when the task benefits from them."
    } else if roles.iter().any(|role| role.extra) {
        "Add extra developers with {{LAUNCHER}} add <role> only when the user requests or approves it and the task has independent ownership."
    } else {
        "No extra instances are enabled in this configuration. Reconsider the workflow and role ownership before expanding the crew."
    };
    let template = Template {
        kind: (selection.agent != crate::agent::Kind::Claude).then_some(selection.agent),
        codex_tree: is_codex_tree.then_some(true),
        agy_tree: is_agy_tree.then_some(true),
        version: config::SUPPORTED_VERSION,
        common_prompt: format!(
            "Workflow: {workflow}\nFollow the repository's contributor instructions and discover its actual build, lint and test commands. Wait for an explicit task before changing files. A handoff includes objective, acceptance criteria, exact base and delivery commits, validation results and limitations. A 'done' report is not acceptance of the integrated result.\nOnly {coordinator} writes {{{{STATUS}}}}, following {{{{SCHEMA}}}}; other roles report status without editing the board. Write the whole board atomically through a .tmp file and rename.\nAdditional sessions cost time and tokens. {expansion}\n{{{{PEERS}}}}\n"
        ),
        start_message: if is_tree {
            "Confirm your coordinator role in one line and wait for an explicit task before changing files."
        } else {
            "Confirm your role in one line and wait for an explicit task before changing files."
        },
        workspace: Workspace {
            label: selection.name.clone(),
        },
        board: Board {
            tab: format!("{}-status", selection.name),
            writer: coordinator,
        },
        worktrees: (!is_tree)
            .then(|| {
                selection.base.as_ref().map(|base| Worktrees {
                    dir: ".worktrees",
                    base: base.clone(),
                })
            })
            .flatten(),
        roles,
    };
    let models_note = if selection.is_customized() {
        format!("{} customized", selection.profile.as_str())
    } else {
        selection.profile.as_str().to_string()
    };
    let mode_note = if is_agy_tree {
        ", agy tree topology"
    } else if is_codex_tree {
        ", codex tree topology"
    } else {
        ""
    };
    let text = format!(
        "# Generated by herdr-crew init ({} workflow{mode_note}, {} models). Edit prompts and roles for your project.\n# Workflow guidance: https://github.com/rarce/herdr-crew/blob/main/docs/workflows.md\n# Models and effort: https://github.com/rarce/herdr-crew/blob/main/docs/models.md; change them with herdr-crew config\n{}",
        selection.preset.id(),
        models_note,
        toml::to_string_pretty(&template)
            .map_err(|e| Fail::run(format!("cannot render crew configuration: {e}")))?
    );
    config::Config::parse(&text, root).map_err(|errors| Fail {
        code: 2,
        lines: errors.iter().map(ToString::to_string).collect(),
    })?;
    Ok(text)
}

pub(crate) fn ensure_new_config(root: &Path) -> Result<(), Fail> {
    let path = root.join(config::CONFIG_FILE);
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            return Err(Fail::run(format!(
                "{} already exists; edit it directly. init never overwrites it",
                path.display()
            )));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_fail(error)),
    }
    if let Ok(metadata) = fs::symlink_metadata(root.join(".herdr"))
        && (!metadata.is_dir() || metadata.file_type().is_symlink())
    {
        return Err(Fail::run(
            ".herdr must be a directory inside the project, not a file or symlink",
        ));
    }
    Ok(())
}

/// What setup would append to `.gitignore`, and a warning when existing rules ignore
/// `crew.toml`.
pub(crate) struct IgnoreCheck {
    pub addition: String,
    pub warning: Option<String>,
}

/// One `git check-ignore` query for the generated-file rules (when `ignore` is set) and for
/// `crew.toml`. Without Git's answer, setup falls back to exact `.gitignore` lines and gives
/// no warning: an advisory check must not block setup.
pub(crate) fn ignore_check(
    root: &Path,
    worktrees: bool,
    ignore: bool,
) -> Result<IgnoreCheck, Fail> {
    let mut rules = Vec::new();
    let mut contents = String::new();
    if ignore {
        let path = root.join(".gitignore");
        contents = match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => fs::read_to_string(&path).map_err(io_fail)?,
            Ok(_) => {
                return Err(Fail::run(
                    ".gitignore is not a regular file; use --no-ignore and add the rules manually",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(io_fail(error)),
        };
        rules = IGNORE_RULES.to_vec();
        if worktrees {
            rules.push(WORKTREES_RULE);
        }
    }
    let mut samples: Vec<&str> = rules
        .iter()
        .flat_map(|(_, samples)| *samples)
        .copied()
        .collect();
    samples.push(config::CONFIG_FILE);
    let answer = git::ignore_rules(root, &samples).ok();
    let warning = answer
        .as_ref()
        .and_then(|decided| decided.last()?.as_ref())
        .filter(|rule| rule.ignores())
        .map(|rule| {
            format!(
                "Warning: {}:{} ignores {} with {:?}. Git will not version the crew until that rule changes.",
                rule.source,
                rule.line,
                config::CONFIG_FILE,
                rule.pattern
            )
        });
    let shared = answer
        .as_ref()
        .map(|decided| git::shared_rules(root, decided));
    // A shared .gitignore rule that ignores the path, such as `.herdr/`, already covers it.
    // A negation counts only when it names this rule's path: the team chose to version that
    // file on purpose, while a broad `!*.json` in a whitelist was never meant for the board.
    // Git does not report negated directories, so a `!` line naming the rule counts too.
    // Otherwise Git's answer decides, even over an exact line that a later negation undoes.
    // Local and global excludes do not count: other clones lack them. Without an answer from
    // Git, setup falls back to exact lines.
    let mut next = 0;
    let missing: Vec<&str> = rules
        .iter()
        .filter(|(rule, rule_samples)| {
            let range = next..next + rule_samples.len();
            next = range.end;
            let covered = match (&answer, &shared) {
                (Some(decided), Some(shared)) => {
                    range.clone().all(|i| {
                        shared[i]
                            && decided[i].as_ref().is_some_and(|decided| {
                                decided.ignores() || same_path(&decided.pattern, rule)
                            })
                    }) || contents
                        .lines()
                        .any(|line| line.trim().starts_with('!') && same_path(line, rule))
                }
                _ => contents.lines().any(|line| same_path(line, rule)),
            };
            !covered
        })
        .map(|(rule, _)| *rule)
        .collect();
    let addition = if missing.is_empty() {
        String::new()
    } else {
        format!(
            "{}\n# Generated crew files\n{}\n",
            if contents.is_empty() || contents.ends_with('\n') {
                ""
            } else {
                "\n"
            },
            missing.join("\n")
        )
    };
    Ok(IgnoreCheck { addition, warning })
}

/// Whether an ignore line names the same path as a root-anchored rule, ignoring negation
/// and the optional leading and trailing slashes: `!.herdr/prompts` matches `/.herdr/prompts/`.
fn same_path(line: &str, rule: &str) -> bool {
    let normalize = |text: &str| {
        let text = text.trim();
        let text = text.strip_prefix('!').unwrap_or(text);
        text.trim_matches('/').to_string()
    };
    normalize(line) == normalize(rule)
}

/// Who runs the wizard: `init` only creates the configuration; `up` creates it and then starts
/// the sessions, so its intro and closing lines say so.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Init,
    Up,
}

/// The summary shown before saving: workflow, project, agent, models and board writer.
pub(crate) fn summary(selection: &Selection) -> String {
    let models_summary = if selection.is_customized() {
        format!(
            "{} (customized; {})",
            selection.profile.as_str(),
            selection.profile.description()
        )
    } else {
        format!(
            "{} ({})",
            selection.profile.as_str(),
            selection.profile.description()
        )
    };
    if selection.is_agy_tree() {
        let isolation = match selection.preset {
            Preset::Research => "read-only in main checkout (Workspace: \"inherit\")",
            _ if selection.tree_isolated => {
                "isolated native worktrees (Workspace: \"share\") for implementers"
            }
            _ => "shared main checkout (Workspace: \"inherit\")",
        };
        return format!(
            "{}\nWorkspace: {}\nAgent: {}\nMode: native tree (1 agy coordinator + native subagents)\nModels: {}\nWorker isolation: {}\nBoard writer: {}-{}",
            selection.preset.description(),
            selection.name,
            selection.agent.as_str(),
            models_summary,
            isolation,
            selection.name,
            selection.preset.roles()[0].0
        );
    }
    if selection.is_codex_tree() {
        return format!(
            "{}\nWorkspace: {}\nAgent: {}\nTopology: tree (1 Codex coordinator + native subagents; requires features.multi_agent_v2 = true)\nModels: {}\nWorker isolation: shared main checkout (no [worktrees])\nBoard writer: {}-{}",
            selection.preset.description(),
            selection.name,
            selection.agent.as_str(),
            models_summary,
            selection.name,
            selection.preset.roles()[0].0
        );
    }
    let topology_line = if supports_codex_tree(selection.agent, selection.preset) {
        "Topology: tabs (independent Herdr tabs)\n"
    } else {
        ""
    };
    format!(
        "{}\nWorkspace: {}\nAgent: {}\n{topology_line}Models: {}\nWorktree base: {}\nBoard writer: {}-{}",
        selection.preset.description(),
        selection.name,
        selection.agent.as_str(),
        models_summary,
        selection
            .base
            .as_deref()
            .unwrap_or("none (main checkout only)"),
        selection.name,
        selection.preset.roles()[0].0
    )
}

/// The line-by-line setup wizard. Returns whether it created `.herdr/crew.toml` (not on a dry
/// run or a cancellation).
fn wizard(
    root: &Path,
    options: &Options,
    dry_run: bool,
    purpose: Purpose,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<bool, Fail> {
    ensure_new_config(root)?;
    let intro = match purpose {
        Purpose::Init => format!(
            "Crew setup for {}\nCreates configuration only; assign tasks after starting your sessions.",
            root.display()
        ),
        Purpose::Up if options.yes => format!(
            "No crew is configured in {} yet ({} is missing).\n\
             Setting one up with the defaults and the given options, then starting its sessions.",
            root.display(),
            config::CONFIG_FILE
        ),
        Purpose::Up => format!(
            "No crew is configured in {} yet ({} is missing).\n\
             Let's create one: choose a workflow and an agent, review the preview, and herdr-crew \
             starts the sessions right after saving it.\n\
             (Ctrl-C or end of input cancels without changing anything; `herdr-crew init` only \
             creates the configuration.)",
            root.display(),
            config::CONFIG_FILE
        ),
    };
    writeln!(output, "{intro}").map_err(io_fail)?;
    let selection = choose(root, options, input, output)?;
    let text = render(&selection, root)?;
    let IgnoreCheck { addition, warning } =
        ignore_check(root, selection.base.is_some(), selection.ignore)?;
    writeln!(output, "\n{}\n", summary(&selection)).map_err(io_fail)?;
    if !options.yes || dry_run {
        writeln!(output, "{text}").map_err(io_fail)?;
    }
    if !addition.is_empty() {
        writeln!(output, ".gitignore additions:{addition}").map_err(io_fail)?;
    }
    if let Some(warning) = warning {
        writeln!(output, "{warning}\n").map_err(io_fail)?;
    }
    if dry_run {
        dry_run_note(purpose, output)?;
        return Ok(false);
    }
    let question = if addition.is_empty() {
        "Create .herdr/crew.toml?"
    } else {
        "Create .herdr/crew.toml and update .gitignore?"
    };
    if !options.yes && !yes_no(input, output, question, true)? {
        writeln!(output, "Cancelled: no files created or changed.").map_err(io_fail)?;
        return Ok(false);
    }
    create(root, &selection, &text, purpose, output)?;
    Ok(true)
}

fn dry_run_note(purpose: Purpose, output: &mut impl Write) -> Result<(), Fail> {
    writeln!(output, "Dry run: no files created or changed.").map_err(io_fail)?;
    if purpose == Purpose::Up {
        writeln!(
            output,
            "Without --dry-run, up would save this configuration and then start the sessions above."
        )
        .map_err(io_fail)?;
    }
    Ok(())
}

/// Saves a confirmed configuration and the `.gitignore` rules, then prints the next steps.
pub(crate) fn create(
    root: &Path,
    selection: &Selection,
    text: &str,
    purpose: Purpose,
    output: &mut impl Write,
) -> Result<(), Fail> {
    ensure_new_config(root)?;
    fs::create_dir_all(root.join(".herdr")).map_err(io_fail)?;
    let path = root.join(config::CONFIG_FILE);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| Fail::run(format!("cannot create {}: {e}", path.display())))?;
    if let Err(error) = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
    {
        let _ = fs::remove_file(&path);
        return Err(io_fail(error));
    }
    // Re-read before appending so changes made while the wizard was open are preserved.
    if selection.ignore {
        let update = (|| -> Result<(), Fail> {
            let addition = ignore_check(root, selection.base.is_some(), true)?.addition;
            if !addition.is_empty() {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(root.join(".gitignore"))
                    .and_then(|mut file| file.write_all(addition.as_bytes()))
                    .map_err(io_fail)?;
            }
            Ok(())
        })();
        if let Err(error) = update {
            return Err(Fail::run(format!(
                "created {}, but could not update .gitignore: {}. Add generated-file rules manually",
                path.display(),
                error.lines.join(" ")
            )));
        }
    }
    match purpose {
        Purpose::Init => {
            writeln!(output, "Created {}\nNext: review the prompts, run herdr-crew check, then herdr-crew up --dry-run.\nChange models and effort per role with herdr-crew config.\nStart sessions with herdr or herdr-crew up --no-attach when ready.", path.display()).map_err(io_fail)?;
            if selection.agent == crate::agent::Kind::Codex {
                writeln!(output, "Codex setup: run herdr-crew codex-install, then review/trust the crew hook in Codex /hooks before launching roles.").map_err(io_fail)?;
                if selection.is_codex_tree() {
                    writeln!(output, "Codex tree mode: ensure features.multi_agent_v2 = true in ~/.codex/config.toml (Codex 0.163.0+).").map_err(io_fail)?;
                }
            }
            if selection.agent == crate::agent::Kind::Pi {
                writeln!(output, "pi setup: run herdr-crew pi-install once; its extension keeps each role's prompt across herdr restores.").map_err(io_fail)?;
            }
            if selection.agent == crate::agent::Kind::Agy {
                writeln!(output, "agy setup: run herdr-crew agy-install once (and herdr integration install antigravity-cli for session restore).").map_err(io_fail)?;
            }
        }
        Purpose::Up => {
            writeln!(
                output,
                "Created {}; edit it any time and run herdr-crew up again.",
                path.display()
            )
            .map_err(io_fail)?;
        }
    }
    Ok(())
}

pub fn run(root: &Path, options: &Options, dry_run: bool) -> Result<(), Fail> {
    setup(root, options, dry_run, Purpose::Init).map(|_| ())
}

/// The wizard on the terminal: full screen when a person can answer it, otherwise line by
/// line. Returns whether it created the configuration.
pub fn setup(
    root: &Path,
    options: &Options,
    dry_run: bool,
    purpose: Purpose,
) -> Result<bool, Fail> {
    if !options.yes && !options.no_tui && crate::tui::interactive() {
        ensure_new_config(root)?;
        validate_options(root, options)?;
        let Some((selection, text)) = crate::tui::wizard::run(root, options).map_err(io_fail)?
        else {
            println!("Cancelled: no files created or changed.");
            return Ok(false);
        };
        let mut out = io::stdout().lock();
        if dry_run {
            writeln!(out, "{}\n\n{text}", summary(&selection)).map_err(io_fail)?;
            dry_run_note(purpose, &mut out)?;
            return Ok(false);
        }
        create(root, &selection, &text, purpose, &mut out)?;
        return Ok(true);
    }
    wizard(
        root,
        options,
        dry_run,
        purpose,
        &mut io::stdin().lock(),
        &mut io::stdout().lock(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_workflows_generate_valid_prompts_and_worktree_policies() {
        for preset in Preset::ALL {
            let base =
                matches!(preset, Preset::Review | Preset::Parallel).then(|| "origin/main".into());
            let selection = Selection {
                agent: crate::agent::Kind::Claude,
                name: "a".repeat(MAX_PREFIX),
                preset,
                topology: Topology::Tabs,
                profile: Profile::Balanced,
                overrides: BTreeMap::new(),
                base,
                tree_isolated: false,
                ignore: true,
            };
            let text =
                render(&selection, Path::new("/repo")).unwrap_or_else(|f| panic!("{:?}", f.lines));
            let config = config::Config::parse(&text, Path::new("/repo")).unwrap();
            for role in &config.roles {
                let prompt = crate::prompt::render(&config, &role.name, Path::new("/bin/crew"));
                assert!(!prompt.contains("{{"));
                assert!(prompt.contains(&config.board.writer));
                if role.extra {
                    assert!(role.worktree);
                }
            }
            assert_eq!(
                config.roles.len(),
                match preset {
                    Preset::Solo => 1,
                    Preset::Review | Preset::Research => 3,
                    Preset::Parallel => 4,
                }
            );
            assert!(!config.roles[0].worktree);
            if preset == Preset::Research {
                assert!(config.worktrees.is_none());
            }
        }
    }

    #[test]
    fn folder_names_produce_valid_defaults() {
        for folder in [
            "My Project",
            "123-project",
            "___",
            "工具",
            "áéí",
            "long".repeat(30).as_str(),
        ] {
            assert!(valid_name(&suggested_name(Path::new(folder))), "{folder}");
        }
    }

    fn ok<T>(result: Result<T, Fail>) -> T {
        result.unwrap_or_else(|f| panic!("{:?}", f.lines))
    }

    fn choose_with(root: &Path, options: &Options, answers: &str) -> (Selection, String) {
        let mut output = Vec::new();
        let selection = choose(root, options, &mut answers.as_bytes(), &mut output)
            .unwrap_or_else(|f| panic!("{:?}", f.lines));
        (selection, String::from_utf8(output).unwrap())
    }

    #[test]
    fn the_wizard_asks_for_the_agent_and_retries_unknown_ones() {
        let root = git::test_repo("init-agent");
        let (selection, output) = choose_with(&root, &Options::default(), "\n1\ngpt\nPi\n\n\n\n");
        assert_eq!(selection.agent, crate::agent::Kind::Pi);
        assert!(
            output.contains("Enter one of: claude, codex, pi, agy."),
            "{output}"
        );
        let yes = Options {
            yes: true,
            ..Options::default()
        };
        let (selection, _) = choose_with(&root, &yes, "");
        assert_eq!(selection.agent, crate::agent::Kind::Claude);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_unfetched_base_needs_confirmation_or_warns_when_given_as_a_flag() {
        let root = git::test_repo("init-base");
        git::test_remote_branch(&root, "origin/main");
        // A typo is refused at the confirmation, then the suggested default is accepted.
        let (selection, output) =
            choose_with(&root, &Options::default(), "\n3\n\n\n\norigin/mian\n\n\n\n");
        assert_eq!(selection.base.as_deref(), Some("origin/main"));
        assert!(
            output.contains("origin/mian is not among the fetched"),
            "{output}"
        );
        assert!(output.contains("Use origin/mian anyway? [y/N]"), "{output}");
        // An explicit answer keeps it, since `up` fetches before creating worktrees.
        let (selection, _) =
            choose_with(&root, &Options::default(), "\n3\n\n\n\norigin/next\ny\n\n");
        assert_eq!(selection.base.as_deref(), Some("origin/next"));

        let flag = Options {
            preset: Some(Preset::Parallel),
            base: Some("origin/mian".into()),
            yes: true,
            ..Options::default()
        };
        let (selection, output) = choose_with(&root, &flag, "");
        assert_eq!(selection.base.as_deref(), Some("origin/mian"));
        assert!(output.contains("Warning: origin/mian"), "{output}");

        // Without --yes, the flag's value needs the same confirmation as a typed one, and
        // declining it falls back to the prompt.
        let interactive = Options { yes: false, ..flag };
        let (selection, output) = choose_with(&root, &interactive, "\n\n\n\n\n\n\n");
        assert_eq!(selection.base.as_deref(), Some("origin/main"));
        assert!(output.contains("Use origin/mian anyway? [y/N]"), "{output}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn worktree_presets_without_a_remote_fail_before_the_agent_question() {
        let root = git::test_repo("init-no-remote");
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["remote", "remove", "origin"])
            .status()
            .unwrap();
        assert!(status.success());
        for options in [
            Options {
                preset: Some(Preset::Parallel),
                agent: Some(Kind::Claude),
                ..Options::default()
            },
            Options {
                preset: Some(Preset::Parallel),
                agent: Some(Kind::Codex),
                topology: Some(Topology::Tabs),
                ..Options::default()
            },
            Options {
                preset: Some(Preset::Parallel),
                agent: Some(Kind::Codex),
                yes: true,
                ..Options::default()
            },
            Options {
                preset: Some(Preset::Review),
                yes: true,
                ..Options::default()
            },
        ] {
            let mut output = Vec::new();
            let result = choose(&root, &options, &mut &b"\n"[..], &mut output);
            let output = String::from_utf8(output).unwrap();
            assert!(
                matches!(&result, Err(Fail { code: 2, lines }) if lines[0].contains("configured Git remote"))
            );
            assert!(!output.contains("Agent ("), "{output}");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn flag_combinations_fail_before_the_agent_question() {
        let root = git::test_repo("init-flags");
        let options = Options {
            preset: Some(Preset::Solo),
            base: Some("origin/main".into()),
            ..Options::default()
        };
        let mut output = Vec::new();
        let result = choose(&root, &options, &mut &b"\n"[..], &mut output);
        let output = String::from_utf8(output).unwrap();
        assert!(matches!(result, Err(Fail { code: 2, .. })));
        assert!(!output.contains("Agent ("), "{output}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn addition(root: &Path, worktrees: bool) -> String {
        ok(ignore_check(root, worktrees, true)).addition
    }

    fn warning(root: &Path) -> Option<String> {
        ok(ignore_check(root, false, false)).warning
    }

    #[test]
    fn ignore_rules_already_decided_by_the_repository_are_not_appended() {
        let root = git::test_repo("init-ignore");
        let gitignore = root.join(".gitignore");
        assert!(warning(&root).is_none());

        std::fs::write(&gitignore, ".herdr/\n").unwrap();
        assert_eq!(addition(&root, false), "");
        assert_eq!(
            addition(&root, true),
            "\n# Generated crew files\n/.worktrees/\n"
        );
        let text = warning(&root).unwrap();
        assert!(
            text.contains(".gitignore:1 ignores .herdr/crew.toml"),
            "{text}"
        );

        // A deliberate negation keeps its effect, however its path is spelled.
        for negation in ["!/.herdr/prompts/", "!.herdr/prompts/", "!/.herdr/prompts"] {
            std::fs::write(
                &gitignore,
                format!("/.herdr/*\n!/.herdr/crew.toml\n{negation}\n"),
            )
            .unwrap();
            assert!(warning(&root).is_none());
            assert_eq!(addition(&root, false), "", "{negation}");
        }

        // A pattern for some files covers those files, not the directories holding others.
        std::fs::write(&gitignore, "*.json").unwrap();
        assert_eq!(
            addition(&root, false),
            "\n\n# Generated crew files\n/.herdr/status.json.tmp\n/.herdr/.herdr-crew-*.tmp\n/.herdr/prompts/\n/.herdr/codex/\n"
        );

        // A broad negation in a whitelist was not meant for the board, which must stay out.
        std::fs::write(&gitignore, "*\n!*/\n!*.json\n!*.toml\n").unwrap();
        assert_eq!(
            addition(&root, false),
            "\n# Generated crew files\n/.herdr/status.json\n/.herdr/status.schema.json\n/.herdr/codex/\n"
        );
        assert!(warning(&root).is_none());

        // A whitelist's `*` reports the directory as ignored while `!*.txt` re-includes
        // the prompt files inside it.
        std::fs::write(&gitignore, "*\n!*/\n!*.txt\n").unwrap();
        assert_eq!(
            addition(&root, false),
            "\n# Generated crew files\n/.herdr/prompts/\n"
        );

        // An exact line does not count when a later negation re-includes the file.
        std::fs::write(&gitignore, "/.herdr/status.json\n!*.json\n").unwrap();
        assert!(addition(&root, false).contains("\n/.herdr/status.json\n"));

        // Local excludes do not count: other clones lack them.
        std::fs::remove_file(&gitignore).unwrap();
        std::fs::write(root.join(".git/info/exclude"), "/.herdr/status.json\n").unwrap();
        assert!(addition(&root, false).contains("/.herdr/status.json\n"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_failing_ignore_query_falls_back_to_exact_lines_without_blocking_setup() {
        let root = git::test_repo("init-ignore-broken");
        // Git refuses a directory as an excludes file and check-ignore exits with 128.
        let excludes = root.join("excludes");
        std::fs::create_dir(&excludes).unwrap();
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["config", "core.excludesFile"])
            .arg(&excludes)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(git::ignore_rules(&root, &[config::CONFIG_FILE]).is_err());

        std::fs::write(root.join(".gitignore"), "/.herdr/status.json\n").unwrap();
        let check = ok(ignore_check(&root, false, true));
        assert!(check.warning.is_none());
        assert!(!check.addition.contains("/.herdr/status.json\n"));
        assert!(check.addition.contains("/.herdr/prompts/\n"));
        assert!(ok(ignore_check(&root, false, false)).addition.is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn answers_retry_invalid_choices_and_respect_negative_default() {
        let mut output = Vec::new();
        assert!(
            !yes_no(&mut &b"wrong\n\n"[..], &mut output, "Test", false)
                .unwrap_or_else(|f| panic!("{:?}", f.lines))
        );
        assert!(String::from_utf8(output).unwrap().contains("Enter y or n"));
    }

    #[test]
    fn render_applies_role_overrides_across_agents() {
        let root = Path::new("/repo");
        for (agent, custom_model, custom_effort) in [
            (Kind::Claude, "fable", "xhigh"),
            (Kind::Codex, "gpt-6.1-terra", "ultra"),
            (Kind::Pi, "my-provider-model", "max"),
            (Kind::Agy, "claude-opus-5-5", "medium"),
        ] {
            let mut overrides = BTreeMap::new();
            overrides.insert(
                "lead".to_string(),
                RoleModelOverride {
                    model: Some(Some(custom_model.into())),
                    effort: Some(Some(custom_effort.into())),
                },
            );
            overrides.insert(
                "reviewer".to_string(),
                RoleModelOverride {
                    model: Some(None),
                    effort: Some(None),
                },
            );
            let selection = Selection {
                agent,
                name: "demo".into(),
                preset: Preset::Review,
                topology: if agent == Kind::Agy {
                    Topology::Tree
                } else {
                    Topology::Tabs
                },
                profile: Profile::Balanced,
                overrides,
                base: None,
                tree_isolated: agent == Kind::Agy,
                ignore: false,
            };
            assert!(selection.is_customized());
            assert!(summary(&selection).contains("balanced (customized;"));
            let text = ok(render(&selection, root));
            assert!(text.contains("balanced customized models"), "{text}");
            let config = config::Config::parse(&text, root).unwrap();
            let lead = config.role("demo-lead").unwrap();
            let (lead_model, lead_effort) = options_model_effort(&lead.agent());
            assert_eq!(lead_model.as_deref(), Some(custom_model));
            assert_eq!(lead_effort.as_deref(), Some(custom_effort));
            if agent == Kind::Agy {
                assert!(config.agy_tree);
                assert_eq!(config.roles.len(), 1);
            } else {
                let reviewer = config.role("demo-reviewer").unwrap();
                let (rev_model, rev_effort) = options_model_effort(&reviewer.agent());
                assert_eq!(rev_model, None);
                assert_eq!(rev_effort, None);
            }
        }
    }

    #[test]
    fn line_by_line_wizard_customizes_role_models_and_retries_invalid_settings() {
        let root = git::test_repo("init-custom-models");
        // Multi-role customization on Claude review:
        // name=demo, workflow=2 (review), agent=claude, profile=3 (balanced), customize=y:
        // lead: invalid effort "ultra" -> retry with fable + xhigh
        // dev: keep default model, inherit effort
        // reviewer: inherit model, keep default effort
        // worktrees=n, ignore=n
        let answers =
            "demo\n2\nclaude\n3\ny\nfable\nultra\nfable\nxhigh\n\ninherit\ninherit\n\nn\nn\n";
        let (selection, output) = choose_with(&root, &Options::default(), answers);
        assert!(output.contains("Invalid setting:"), "{output}");
        assert!(selection.is_customized());
        let text = ok(render(&selection, &root));
        let config = config::Config::parse(&text, &root).unwrap();
        let lead = config.role("demo-lead").unwrap();
        assert_eq!(lead.claude.model.as_deref(), Some("fable"));
        assert_eq!(lead.claude.effort.as_deref(), Some("xhigh"));
        let dev = config.role("demo-dev").unwrap();
        assert_eq!(dev.claude.model.as_deref(), Some("sonnet"));
        assert_eq!(dev.claude.effort, None);
        let reviewer = config.role("demo-reviewer").unwrap();
        assert_eq!(reviewer.claude.model, None);
        assert_eq!(reviewer.claude.effort.as_deref(), Some("high"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn agy_tree_presets_render_valid_single_coordinator_manifest_without_remote() {
        let root = git::test_repo("init-agy-tree");
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["remote", "remove", "origin"])
            .status()
            .unwrap();
        assert!(status.success());

        for (preset, shared, expected_workers) in [
            (
                Preset::Review,
                false,
                vec![
                    ("- dev (TypeName: \"self\", Workspace: \"share\")", true),
                    (
                        "- reviewer (TypeName: \"research\", Workspace: \"inherit\")",
                        true,
                    ),
                ],
            ),
            (
                Preset::Review,
                true,
                vec![
                    ("- dev (TypeName: \"self\", Workspace: \"inherit\")", true),
                    (
                        "- reviewer (TypeName: \"research\", Workspace: \"inherit\")",
                        true,
                    ),
                ],
            ),
            (
                Preset::Parallel,
                false,
                vec![
                    ("- dev-a (TypeName: \"self\", Workspace: \"share\")", true),
                    ("- dev-b (TypeName: \"self\", Workspace: \"share\")", true),
                    (
                        "- reviewer (TypeName: \"research\", Workspace: \"inherit\")",
                        true,
                    ),
                ],
            ),
            (
                Preset::Research,
                false,
                vec![
                    (
                        "- research-a (TypeName: \"research\", Workspace: \"inherit\")",
                        true,
                    ),
                    (
                        "- research-b (TypeName: \"research\", Workspace: \"inherit\")",
                        true,
                    ),
                ],
            ),
        ] {
            let options = Options {
                agent: Some(Kind::Agy),
                preset: Some(preset),
                name: Some("acme".into()),
                shared_checkout: shared,
                yes: true,
                ..Options::default()
            };
            let (selection, _) = choose_with(&root, &options, "");
            assert!(selection.is_tree());
            assert!(selection.base.is_none());
            let text = ok(render(&selection, &root));
            assert!(text.contains("agy_tree = true"), "{text}");
            assert!(!text.contains("[worktrees]"), "{text}");
            let config = config::Config::parse(&text, &root).unwrap();
            assert!(config.agy_tree);
            assert_eq!(config.kind, Kind::Agy);
            assert!(config.worktrees.is_none());
            assert_eq!(config.roles.len(), 1);
            assert_eq!(config.roles[0].name, "acme-lead");
            assert!(!config.roles[0].worktree);
            assert!(!config.roles[0].extra);
            assert_eq!(config.board.writer, "acme-lead");
            let full_prompt =
                crate::prompt::render(&config, "acme-lead", Path::new("/bin/herdr-crew"));
            assert!(!full_prompt.contains("{{"));
            assert!(full_prompt.contains("Crew runtime communication (agy native tree):"));
            for (needle, present) in expected_workers {
                assert_eq!(
                    full_prompt.contains(needle),
                    present,
                    "{needle}\n{full_prompt}"
                );
            }
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn line_by_line_agy_tree_wizard_skips_topology_and_only_customizes_coordinator() {
        let root = git::test_repo("init-agy-tree-wizard");
        // name=demo, workflow=review, agent=agy (no topology prompt), profile=balanced,
        // customize=y -> only lead (invalid suffixed model + high effort -> retry with claude-opus-5-5 + xhigh),
        // isolation=y (share), ignore=n
        let answers =
            "demo\n2\nagy\n3\ny\ngemini-4-neon-high\nhigh\nclaude-opus-5-5\nxhigh\ny\nn\n";
        let (selection, output) = choose_with(&root, &Options::default(), answers);
        assert!(selection.is_tree());
        assert!(selection.tree_isolated);
        assert!(!output.contains("Topology"), "{output}");
        assert!(output.contains("Invalid setting:"), "{output}");
        assert!(output.contains("Role demo-lead (lead):"), "{output}");
        assert!(!output.contains("Role demo-dev"), "{output}");
        assert!(!output.contains("Role demo-reviewer"), "{output}");
        assert!(output.contains("Use isolated native worktrees"), "{output}");
        let text = ok(render(&selection, &root));
        let config = config::Config::parse(&text, &root).unwrap();
        assert!(config.agy_tree);
        assert_eq!(config.roles.len(), 1);
        assert_eq!(
            config.roles[0].agy.model.as_deref(),
            Some("claude-opus-5-5")
        );
        assert_eq!(config.roles[0].agy.effort.as_deref(), Some("xhigh"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn explicit_base_rejects_multi_role_agy_and_reprompts_in_choose() {
        let root = git::test_repo("init-explicit-base");
        git::test_remote_branch(&root, "origin/main");
        // Flag combination fails immediately before asking.
        for preset in [Some(Preset::Review), Some(Preset::Parallel), None] {
            let options = Options {
                agent: Some(Kind::Agy),
                preset,
                base: Some("origin/main".into()),
                ..Options::default()
            };
            let mut output = Vec::new();
            let result = choose(&root, &options, &mut &b""[..], &mut output);
            assert!(
                matches!(&result, Err(Fail { code: 2, lines }) if lines[0].contains(AGY_TREE_BASE_ERROR)),
                "{preset:?}"
            );
            assert!(output.is_empty());
        }

        // Interactive --base without --agent reprompts when agy is chosen on review.
        let options = Options {
            base: Some("origin/main".into()),
            ..Options::default()
        };
        // name=demo, workflow=review, agent=agy (rejected) -> claude, profile=default, customize=N, ignore=N
        let (selection, output) =
            choose_with(&root, &options, "demo\nreview\nagy\nclaude\n\n\nn\n");
        assert!(!selection.is_tree());
        assert_eq!(selection.agent, Kind::Claude);
        assert_eq!(selection.base.as_deref(), Some("origin/main"));
        assert!(output.contains(AGY_TREE_BASE_ERROR), "{output}");
        let text = ok(render(&selection, &root));
        let config = config::Config::parse(&text, &root).unwrap();
        assert!(!config.agy_tree);
        assert_eq!(config.worktrees.as_ref().unwrap().branch, "main");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn codex_tree_presets_render_valid_single_coordinator_manifest_without_remote() {
        let root = git::test_repo("init-codex-tree");
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["remote", "remove", "origin"])
            .status()
            .unwrap();
        assert!(status.success());

        for (preset, shared, expected_workers) in [
            (
                Preset::Review,
                false,
                vec![
                    "- dev (task_name: \"dev\"):",
                    "- reviewer (task_name: \"reviewer\"):",
                ],
            ),
            (
                Preset::Review,
                true,
                vec![
                    "- dev (task_name: \"dev\"):",
                    "- reviewer (task_name: \"reviewer\"):",
                ],
            ),
            (
                Preset::Parallel,
                false,
                vec![
                    "- dev-a (task_name: \"dev_a\"):",
                    "- dev-b (task_name: \"dev_b\"):",
                    "- reviewer (task_name: \"reviewer\"):",
                    "agreed disjoint files",
                ],
            ),
            (
                Preset::Research,
                false,
                vec![
                    "- research-a (task_name: \"research_a\"):",
                    "- research-b (task_name: \"research_b\"):",
                ],
            ),
        ] {
            let options = Options {
                agent: Some(Kind::Codex),
                preset: Some(preset),
                topology: Some(Topology::Tree),
                name: Some("acme".into()),
                shared_checkout: shared,
                yes: true,
                ..Options::default()
            };
            let (selection, _) = choose_with(&root, &options, "");
            assert!(selection.is_codex_tree());
            assert!(selection.base.is_none());
            let text = ok(render(&selection, &root));
            assert!(text.contains("codex_tree = true"), "{text}");
            assert!(text.contains("codex tree topology"), "{text}");
            assert!(!text.contains("[worktrees]"), "{text}");
            let config = config::Config::parse(&text, &root).unwrap();
            assert!(config.codex_tree);
            assert!(!config.agy_tree);
            assert_eq!(config.kind, Kind::Codex);
            assert!(config.worktrees.is_none());
            assert_eq!(config.roles.len(), 1);
            assert_eq!(config.roles[0].name, "acme-lead");
            assert!(!config.roles[0].worktree);
            assert!(!config.roles[0].extra);
            assert_eq!(config.board.writer, "acme-lead");
            let full_prompt =
                crate::prompt::render(&config, "acme-lead", Path::new("/bin/herdr-crew"));
            assert!(!full_prompt.contains("{{"));
            assert!(full_prompt.contains("Crew runtime communication (Codex native tree):"));
            assert!(full_prompt.contains("fork_turns = \"none\""));
            assert!(full_prompt.contains("followup_task"));
            assert!(full_prompt.contains("list_agents"));
            for needle in expected_workers {
                assert!(
                    full_prompt.contains(needle),
                    "missing {needle}\n{full_prompt}"
                );
            }
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn line_by_line_codex_wizard_offers_topology_defaulting_to_tabs_and_skips_worktrees_in_tree() {
        let root = git::test_repo("init-codex-topology-wizard");
        git::test_remote_branch(&root, "origin/main");

        // Multi-role Codex defaults to tabs (empty Enter at Topology).
        // name=demo, workflow=review(2), agent=codex, topology=default(1=tabs), profile=balanced(3),
        // customize=n, worktrees=n, ignore=n
        let (tabs_sel, tabs_out) =
            choose_with(&root, &Options::default(), "demo\n2\ncodex\n\n3\nn\nn\nn\n");
        assert_eq!(tabs_sel.topology, Topology::Tabs);
        assert!(!tabs_sel.is_tree());
        assert!(
            tabs_out.contains("Choose a topology for Codex roles:"),
            "{tabs_out}"
        );
        assert!(tabs_out.contains("Use separate worktrees"), "{tabs_out}");

        // Choosing tree (2) skips Worktrees and Base, and only customizes coordinator.
        // name=demo, workflow=review(2), agent=codex, topology=tree(2), profile=balanced(3),
        // customize=y -> only lead (gpt-6.2-astra, xhigh), ignore=n
        let (tree_sel, tree_out) = choose_with(
            &root,
            &Options::default(),
            "demo\n2\ncodex\n2\n3\ny\ngpt-6.2-astra\nxhigh\nn\n",
        );
        assert!(tree_sel.is_codex_tree());
        assert!(tree_sel.base.is_none());
        assert!(tree_out.contains("Role demo-lead (lead):"), "{tree_out}");
        assert!(!tree_out.contains("Role demo-dev"), "{tree_out}");
        assert!(!tree_out.contains("Use separate worktrees"), "{tree_out}");
        let text = ok(render(&tree_sel, &root));
        let config = config::Config::parse(&text, &root).unwrap();
        assert!(config.codex_tree);
        assert_eq!(config.roles.len(), 1);
        assert_eq!(
            config.roles[0].codex.model.as_deref(),
            Some("gpt-6.2-astra")
        );
        assert_eq!(
            config.roles[0].codex.reasoning_effort.as_deref(),
            Some("xhigh")
        );

        // With --base on Codex review, selecting tree at Topology reprompts and accepts tabs.
        let base_opts = Options {
            agent: Some(Kind::Codex),
            preset: Some(Preset::Review),
            base: Some("origin/main".into()),
            ..Options::default()
        };
        // name=demo, topology=tree(rejected)->tabs(1), profile=default, customize=n, ignore=n
        let (base_sel, base_out) = choose_with(&root, &base_opts, "demo\ntree\n1\n\n\nn\n");
        assert_eq!(base_sel.topology, Topology::Tabs);
        assert_eq!(base_sel.base.as_deref(), Some("origin/main"));
        assert!(base_out.contains(TREE_BASE_INTERACTIVE_ERROR), "{base_out}");

        // With --topology tree without --agent: interactive defaults to review + codex, reprompting solo/claude.
        let tree_opts = Options {
            topology: Some(Topology::Tree),
            ..Options::default()
        };
        let (sel, out) = choose_with(&root, &tree_opts, "demo\nsolo\n\nclaude\n\n\n\nn\n");
        assert!(sel.is_codex_tree());
        assert_eq!(sel.preset, Preset::Review);
        assert_eq!(sel.agent, Kind::Codex);
        assert!(out.contains(TREE_PRESET_ERROR), "{out}");
        assert!(out.contains(TREE_AGENT_ERROR), "{out}");

        std::fs::remove_dir_all(&root).unwrap();
    }
}
