//! Initial crew setup: choose a software workflow, preview it, then create a validated config.

use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Write};
use std::path::Path;

use serde::Serialize;

use crate::{Fail, config, git};

const MAX_PREFIX: usize = 21; // Leave room for "-research-a" within a 32-byte agent name.
const IGNORE_RULES: &[&str] = &[
    "/.herdr/status.json",
    "/.herdr/status.json.tmp",
    "/.herdr/.herdr-crew-*.tmp",
    "/.herdr/status.schema.json",
    "/.herdr/prompts/",
    "/.herdr/codex/",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Solo,
    Review,
    Parallel,
    Research,
}

impl Preset {
    const ALL: [Self; 4] = [Self::Solo, Self::Review, Self::Parallel, Self::Research];

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "solo" => Some(Self::Solo),
            "review" => Some(Self::Review),
            "parallel" => Some(Self::Parallel),
            "research" => Some(Self::Research),
            _ => None,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Solo => "solo",
            Self::Review => "review",
            Self::Parallel => "parallel",
            Self::Research => "research",
        }
    }

    fn description(self) -> &'static str {
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

    fn roles(self) -> &'static [(&'static str, Kind)] {
        match self {
            Self::Solo => &[("dev", Kind::Solo)],
            Self::Review => &[
                ("lead", Kind::Lead),
                ("dev", Kind::Developer),
                ("reviewer", Kind::Reviewer),
            ],
            Self::Parallel => &[
                ("lead", Kind::Lead),
                ("dev-a", Kind::Developer),
                ("dev-b", Kind::Developer),
                ("reviewer", Kind::Reviewer),
            ],
            Self::Research => &[
                ("lead", Kind::Lead),
                ("research-a", Kind::Researcher),
                ("research-b", Kind::Researcher),
            ],
        }
    }
}

#[derive(Default)]
pub struct Options {
    pub agent: Option<crate::agent::Kind>,
    pub preset: Option<Preset>,
    pub name: Option<String>,
    pub base: Option<String>,
    pub shared_checkout: bool,
    pub yes: bool,
    pub no_ignore: bool,
}

#[derive(Clone, Copy)]
enum Kind {
    Solo,
    Lead,
    Developer,
    Reviewer,
    Researcher,
}

struct Selection {
    agent: crate::agent::Kind,
    name: String,
    preset: Preset,
    base: Option<String>,
    ignore: bool,
}

#[derive(Serialize)]
struct Template {
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<crate::agent::Kind>,
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
}

fn valid_name(name: &str) -> bool {
    name.len() <= MAX_PREFIX && config::is_agent_name(name)
}

fn suggested_name(root: &Path) -> String {
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

fn choose(
    root: &Path,
    options: &Options,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<Selection, Fail> {
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
        loop {
            let choice = ask(input, output, "Workflow", Some("1"))?;
            if let Some(preset) = Preset::parse(&choice).or_else(|| {
                choice
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| i.checked_sub(1))
                    .and_then(|i| Preset::ALL.get(i).copied())
            }) {
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
    if options.base.is_some()
        && (options.shared_checkout || !matches!(preset, Preset::Review | Preset::Parallel))
    {
        return Err(Fail::usage(
            "--base requires a review or parallel preset using worktrees",
        ));
    }
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
        if !git::has_remote(root).map_err(Fail::run)? {
            return Err(Fail::usage(
                "worktrees require a configured Git remote; configure one first, or use solo, research, or review --shared-checkout",
            ));
        }
        writeln!(output, "Worktrees start from a remote branch, not uncommitted local changes. No fetch runs during setup.").map_err(io_fail)?;
        if let Some(base) = &options.base {
            git::validate_base(root, base).map_err(Fail::usage)?;
            Some(base.clone())
        } else if options.yes {
            let base = suggested_base.ok_or_else(|| Fail::usage("no remote branch found; set --base REMOTE/BRANCH, or use review --shared-checkout"))?;
            git::validate_base(root, &base).map_err(Fail::usage)?;
            Some(base)
        } else {
            loop {
                let base = ask(
                    input,
                    output,
                    "Worktree base (configured remote/branch)",
                    suggested_base.as_deref(),
                )?;
                match git::validate_base(root, &base) {
                    Ok(()) => break Some(base),
                    Err(error) => writeln!(output, "{error}").map_err(io_fail)?,
                }
            }
        }
    } else {
        None
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
        agent: options.agent.unwrap_or_default(),
        name,
        preset,
        base,
        ignore,
    })
}

fn render(selection: &Selection, root: &Path) -> Result<String, Fail> {
    let coordinator = format!("{}-{}", selection.name, selection.preset.roles()[0].0);
    let mut roles = Vec::new();
    for (suffix, kind) in selection.preset.roles() {
        let worktree = selection.base.is_some() && matches!(kind, Kind::Developer | Kind::Reviewer);
        let responsibilities = match kind {
            Kind::Solo => {
                "Own the full task: clarify acceptance, reproduce or understand the problem, implement a focused change, and validate it using the repository's actual commands. Keep investigation, editing and test feedback in one loop. Report the result and remaining limitations to the user."
            }
            Kind::Lead if selection.preset == Preset::Research => {
                "Define a common reproduction and independent hypotheses for research-a and research-b. Assign a bounded question, expected evidence and stopping condition to each researcher. Compare their reports, resolve contradictions with observations, and select one implementation decision. Perform or assign the chosen fix after consolidation, and validate the result before reporting it to the user."
            }
            Kind::Lead => {
                "Coordinate the user's task. Define acceptance, dependencies, base commit, file ownership and a bounded assignment for each worker. Agree interfaces before parallel edits. Collect evidence and exact commits, request corrections, and integrate only reviewed deliveries in dependency order. Validate the combined result before accepting it. Handle requirements and architecture decisions yourself unless a separate investigation is useful."
            }
            Kind::Developer => {
                "Implement only your assigned component. Reproduce relevant failures, preserve interfaces and file ownership, make focused changes, and run relevant reproduction and regression checks. Deliver the exact commit, affected files, commands and results, and limitations to the coordinator. Address reviewer findings; do not integrate another worker's changes yourself."
            }
            Kind::Reviewer => {
                "Review the exact delivered commit against its acceptance criteria. Check correctness, regressions, interfaces and missing validation. Return actionable findings with file locations, consequences and reproduction steps. Request changes or explicitly accept with evidence. Do not silently fix the implementation; the developer owns corrections and the coordinator owns integration."
            }
            Kind::Researcher => {
                "Investigate only the assigned hypothesis or question. Gather commands, observations, relevant code locations and primary references. Explain evidence for and against the hypothesis, uncertainty and a recommended next step. Report to the coordinator before any implementation. Do not edit source files, change Git state, or start shared services."
            }
        };
        let reporting = if matches!(kind, Kind::Solo | Kind::Lead) {
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
        roles.push(Role {
            name: format!("{}-{suffix}", selection.name),
            prompt: format!("You are {{{{NAME}}}} in {{{{REPO}}}}.\n{responsibilities}\n{reporting}\n{isolation}\n"),
            worktree,
            extra: worktree && matches!(kind, Kind::Developer),
        });
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
    let expansion = if roles.iter().any(|role| role.extra) {
        "Add extra developers with {{LAUNCHER}} add <role> only when the user requests or approves it and the task has independent ownership."
    } else {
        "No extra instances are enabled in this configuration. Reconsider the workflow and role ownership before expanding the crew."
    };
    let template = Template {
        kind: (selection.agent == crate::agent::Kind::Codex).then_some(selection.agent),
        version: config::SUPPORTED_VERSION,
        common_prompt: format!(
            "Workflow: {workflow}\nFollow the repository's contributor instructions and discover its actual build, lint and test commands. Wait for an explicit task before changing files. A handoff includes objective, acceptance criteria, exact base and delivery commits, validation results and limitations. A 'done' report is not acceptance of the integrated result.\nOnly {coordinator} writes {{{{STATUS}}}}, following {{{{SCHEMA}}}}; other roles report status without editing the board. Write the whole board atomically through a .tmp file and rename.\nAdditional sessions cost time and tokens. {expansion}\n"
        ),
        start_message: "Confirm your role in one line and wait for an explicit task before changing files.",
        workspace: Workspace {
            label: selection.name.clone(),
        },
        board: Board {
            tab: format!("{}-status", selection.name),
            writer: coordinator,
        },
        worktrees: selection.base.as_ref().map(|base| Worktrees {
            dir: ".worktrees",
            base: base.clone(),
        }),
        roles,
    };
    let text = format!(
        "# Generated by herdr-crew init ({} workflow). Edit prompts and roles for your project.\n# Workflow guidance: https://github.com/rarce/herdr-crew/blob/main/docs/workflows.md\n{}",
        selection.preset.id(),
        toml::to_string_pretty(&template)
            .map_err(|e| Fail::run(format!("cannot render crew configuration: {e}")))?
    );
    config::Config::parse(&text, root).map_err(|errors| Fail {
        code: 2,
        lines: errors.iter().map(ToString::to_string).collect(),
    })?;
    Ok(text)
}

fn ensure_new_config(root: &Path) -> Result<(), Fail> {
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

fn ignore_addition(root: &Path, worktrees: bool) -> Result<String, Fail> {
    let path = root.join(".gitignore");
    let contents = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() => fs::read_to_string(&path).map_err(io_fail)?,
        Ok(_) => {
            return Err(Fail::run(
                ".gitignore is not a regular file; use --no-ignore and add the rules manually",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(io_fail(error)),
    };
    let mut rules = IGNORE_RULES.to_vec();
    if worktrees {
        rules.push("/.worktrees/");
    }
    let missing: Vec<&str> = rules
        .into_iter()
        .filter(|rule| !contents.lines().any(|line| line.trim() == *rule))
        .collect();
    if missing.is_empty() {
        return Ok(String::new());
    }
    Ok(format!(
        "{}\n# Generated crew files\n{}\n",
        if contents.is_empty() || contents.ends_with('\n') {
            ""
        } else {
            "\n"
        },
        missing.join("\n")
    ))
}

fn wizard(
    root: &Path,
    options: &Options,
    dry_run: bool,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<(), Fail> {
    ensure_new_config(root)?;
    writeln!(
        output,
        "Crew setup for {}\nCreates configuration only; assign tasks after starting your sessions.",
        root.display()
    )
    .map_err(io_fail)?;
    let selection = choose(root, options, input, output)?;
    let text = render(&selection, root)?;
    let addition = if selection.ignore {
        ignore_addition(root, selection.base.is_some())?
    } else {
        String::new()
    };
    writeln!(
        output,
        "\n{}\nWorkspace: {}\nBoard writer: {}-{}\n",
        selection.preset.description(),
        selection.name,
        selection.name,
        selection.preset.roles()[0].0
    )
    .map_err(io_fail)?;
    if !options.yes || dry_run {
        writeln!(output, "{text}").map_err(io_fail)?;
    }
    if !addition.is_empty() {
        writeln!(output, ".gitignore additions:{addition}").map_err(io_fail)?;
    }
    if dry_run {
        writeln!(output, "Dry run: no files created or changed.").map_err(io_fail)?;
        return Ok(());
    }
    let question = if addition.is_empty() {
        "Create .herdr/crew.toml?"
    } else {
        "Create .herdr/crew.toml and update .gitignore?"
    };
    if !options.yes && !yes_no(input, output, question, true)? {
        writeln!(output, "Cancelled: no files created or changed.").map_err(io_fail)?;
        return Ok(());
    }
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
            let addition = ignore_addition(root, selection.base.is_some())?;
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
    writeln!(output, "Created {}\nNext: review the prompts, run herdr-crew check, then herdr-crew up --dry-run.\nStart sessions with herdr or herdr-crew up --no-attach when ready.", path.display()).map_err(io_fail)?;
    if selection.agent == crate::agent::Kind::Codex {
        writeln!(output, "Codex setup: run herdr-crew codex-install, then review/trust the crew hook in Codex /hooks before launching roles.").map_err(io_fail)?;
    }
    Ok(())
}

pub fn run(root: &Path, options: &Options, dry_run: bool) -> Result<(), Fail> {
    wizard(
        root,
        options,
        dry_run,
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
                base,
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

    #[test]
    fn answers_retry_invalid_choices_and_respect_negative_default() {
        let mut output = Vec::new();
        assert!(
            !yes_no(&mut &b"wrong\n\n"[..], &mut output, "Test", false)
                .unwrap_or_else(|f| panic!("{:?}", f.lines))
        );
        assert!(String::from_utf8(output).unwrap().contains("Enter y or n"));
    }
}
