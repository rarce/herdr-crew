//! herdr state, steps and `plan(config, state, env)` (design §4.2 and §6.2). Pure: the adapter
//! (`herdr.rs`) queries herdr, calls these functions and runs the steps.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::agent::{CodexOptions, Kind, PiOptions};
use crate::board::schema;
use crate::config::{Config, extra_number};
use crate::prompt;

/// What `up` needs to know from herdr: the project's workspace (by label) and the live agents
/// of every workspace, because their names are global. `adopt` is herdr's initial workspace
/// that `startup` takes over instead of creating one, and `restored` marks a workspace that
/// `startup` only repairs (§4.4).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HerdrState {
    pub workspace: Option<Workspace>,
    pub agents: Vec<Agent>,
    pub adopt: Option<Initial>,
    pub restored: bool,
}

/// herdr's initial workspace with its only tab and pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Initial {
    pub workspace: String,
    pub tab: String,
    pub pane: String,
    /// Its current label, its directory's name.
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub id: String,
    pub tabs: Vec<Tab>,
    pub panes: Vec<Pane>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Tab {
    #[serde(rename = "tab_id")]
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    pub id: String,
    pub tab_id: String,
    /// `agent_session.value` when herdr reports it: the Claude session that can be resumed.
    pub agent_session: Option<String>,
    pub session_agent: Option<String>,
    pub session_kind: Option<String>,
    pub session_source: Option<String>,
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Agent {
    #[serde(default, alias = "agent")]
    pub kind: Option<String>,
    pub name: Option<String>,
    pub workspace_id: String,
    pub tab_id: String,
    pub pane_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WorkspaceItem {
    pub workspace_id: String,
    pub label: String,
}

#[derive(Deserialize)]
struct PaneItem {
    pane_id: String,
    tab_id: String,
    agent_session: Option<SessionItem>,
    cwd: Option<PathBuf>,
}

#[derive(Deserialize)]
struct SessionItem {
    agent: Option<String>,
    kind: Option<String>,
    source: Option<String>,
    value: String,
}

fn items<T: for<'de> Deserialize<'de>>(result: &Value, key: &str) -> Result<Vec<T>, String> {
    let list = result
        .get(key)
        .ok_or_else(|| format!("herdr response without \"{key}\""))?;
    serde_json::from_value(list.clone())
        .map_err(|e| format!("unreadable herdr response in \"{key}\": {e}"))
}

/// The workspaces in the `.result` of `workspace list`.
pub fn workspace_items(workspaces: &Value) -> Result<Vec<WorkspaceItem>, String> {
    items(workspaces, "workspaces")
}

/// The panes in the `.result` of `pane list`.
pub fn pane_items(panes: &Value) -> Result<Vec<Pane>, String> {
    Ok(items::<PaneItem>(panes, "panes")?
        .into_iter()
        .map(|p| Pane {
            id: p.pane_id,
            tab_id: p.tab_id,
            session_agent: p.agent_session.as_ref().and_then(|s| s.agent.clone()),
            session_kind: p.agent_session.as_ref().and_then(|s| s.kind.clone()),
            session_source: p.agent_session.as_ref().and_then(|s| s.source.clone()),
            agent_session: p.agent_session.map(|s| s.value),
            cwd: p.cwd,
        })
        .collect())
}

/// The id of the workspace with that label in the `.result` of `workspace list`.
pub fn find_workspace(workspaces: &Value, label: &str) -> Result<Option<String>, String> {
    let matches: Vec<_> = workspace_items(workspaces)?
        .into_iter()
        .filter(|w| w.label == label)
        .collect();
    match matches.as_slice() {
        [] => Ok(None),
        [workspace] => Ok(Some(workspace.workspace_id.clone())),
        _ => Err(format!(
            "workspace label {label:?} is ambiguous; rename the duplicate workspaces"
        )),
    }
}

impl HerdrState {
    /// Builds the state from the `.result` of `tab list`, `pane list` (of the workspace, when it
    /// exists) and `agent list`.
    pub fn from_results(
        workspace: Option<(String, &Value, &Value)>,
        agents: &Value,
    ) -> Result<Self, String> {
        let workspace = match workspace {
            None => None,
            Some((id, tabs, panes)) => Some(Workspace {
                id,
                tabs: items(tabs, "tabs")?,
                panes: pane_items(panes)?,
            }),
        };
        Ok(HerdrState {
            workspace,
            agents: items(agents, "agents")?,
            adopt: None,
            restored: false,
        })
    }

    fn agent_in_tab(&self, tab_id: &str) -> Option<&Agent> {
        self.agents.iter().find(|a| a.tab_id == tab_id)
    }
}

/// What runs in the foreground of the board pane, according to `pane process-info`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Foreground {
    /// The pane's shell is idle at its prompt.
    Shell,
    /// Something else: the names of the foreground processes.
    Other(String),
    /// herdr did not provide the information.
    Unknown(String),
}

impl Foreground {
    /// Reads the `.result` of `pane process-info --pane P`: the shell is in the foreground when
    /// the foreground process group is its own (verified in step 1, design §4.2 step 9).
    pub fn from_result(result: &Value) -> Foreground {
        let info = &result["process_info"];
        match (
            info["foreground_process_group_id"].as_u64(),
            info["shell_pid"].as_u64(),
        ) {
            (Some(g), Some(s)) if g == s => Foreground::Shell,
            (Some(_), Some(_)) => {
                let names: Vec<&str> = info["foreground_processes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| p["name"].as_str())
                    .collect();
                Foreground::Other(if names.is_empty() {
                    "?".into()
                } else {
                    names.join(", ")
                })
            }
            _ => Foreground::Unknown("no foreground group or shell".into()),
        }
    }
}

/// What the adapter knows outside herdr.
#[derive(Debug, Clone, Default)]
pub struct Env {
    /// `CREW_AGENT_CMD`: types this command in the pane instead of `agent start`.
    pub agent_cmd: Option<String>,
    /// Absolute path of this binary.
    pub binary: PathBuf,
    /// Sessions whose worktree already exists.
    pub worktrees: BTreeSet<String>,
    /// Foreground of the board pane, when the tab exists.
    pub board_pane: Option<Foreground>,
    pub status_exists: bool,
    /// Current content of `.herdr/status.schema.json`, when it exists.
    pub current_schema: Option<String>,
    /// Current time as RFC 3339 with offset, to seed the board.
    pub now: String,
    pub windows: bool,
}

/// The pane a step acts on: an existing one or the root pane of a tab created by the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneRef {
    Existing(String),
    OfTab(String),
}

impl fmt::Display for PaneRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PaneRef::Existing(id) => write!(f, "{id}"),
            PaneRef::OfTab(label) => write!(f, "<pane of \"{label}\">"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceRef {
    Existing(String),
    Created,
}

impl fmt::Display for WorkspaceRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WorkspaceRef::Existing(id) => write!(f, "{id}"),
            WorkspaceRef::Created => write!(f, "<new workspace>"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    CreateWorkspace {
        label: String,
        cwd: PathBuf,
    },
    /// Takes over herdr's initial workspace (§4.4): renames it to `label` when it has another
    /// one and makes its tab the initial tab that `RenameTab` renames.
    AdoptWorkspace {
        initial: Initial,
        label: String,
    },
    /// Renames the initial tab of the workspace just created.
    RenameTab {
        label: String,
    },
    CreateWorktree {
        name: String,
        path: PathBuf,
        remote: String,
        branch: String,
    },
    CreateTab {
        workspace: WorkspaceRef,
        label: String,
        cwd: PathBuf,
    },
    WritePrompt {
        name: String,
        path: PathBuf,
        content: String,
    },
    /// Starts the role's agent in a tab the plan has just created; `message` is the first
    /// message of the new conversation (`start_message`).
    StartAgent {
        kind: Kind,
        codex: CodexOptions,
        pi: Box<PiOptions>,
        name: String,
        pane: PaneRef,
        prompt: PathBuf,
        message: Option<String>,
    },
    /// Types `command` in the pane; `new_tab` waits 1 s for the shell to reach its prompt.
    RunInPane {
        pane: PaneRef,
        command: String,
        new_tab: bool,
    },
    WriteSchema {
        path: PathBuf,
        content: String,
    },
    SeedStatus {
        path: PathBuf,
        content: String,
    },
    Warn(String),
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Step::CreateWorkspace { label, cwd } => {
                write!(f, "create workspace \"{label}\" in {}", cwd.display())
            }
            Step::AdoptWorkspace { initial, label } if initial.label != *label => write!(
                f,
                "adopt workspace {} renamed to \"{label}\"",
                initial.workspace
            ),
            Step::AdoptWorkspace { initial, .. } => {
                write!(f, "adopt workspace {}", initial.workspace)
            }
            Step::RenameTab { label } => write!(f, "rename the initial tab to \"{label}\""),
            Step::CreateWorktree {
                name,
                path,
                remote,
                branch,
            } => {
                write!(
                    f,
                    "create the worktree of {name} in {} from {remote}/{branch}",
                    path.display()
                )
            }
            Step::CreateTab {
                workspace,
                label,
                cwd,
            } => {
                write!(
                    f,
                    "create tab \"{label}\" in {workspace} ({})",
                    cwd.display()
                )
            }
            Step::WritePrompt { path, .. } => write!(f, "write {}", path.display()),
            Step::StartAgent {
                name,
                pane,
                kind: Kind::Claude,
                ..
            } => write!(f, "start {name} in {pane}"),
            Step::StartAgent {
                name,
                pane,
                kind: Kind::Pi,
                pi,
                ..
            } => write!(f, "start {name} (pi, {:?}) in {pane}", pi.args()),
            Step::StartAgent {
                name, pane, codex, ..
            } => write!(f, "start {name} (codex, {:?}) in {pane}", codex.args()),
            Step::RunInPane { pane, command, .. } => write!(f, "type in {pane}: {command}"),
            Step::WriteSchema { path, .. } => write!(f, "write {}", path.display()),
            Step::SeedStatus { path, .. } => write!(f, "seed {}", path.display()),
            Step::Warn(msg) => write!(f, "warning: {msg}"),
        }
    }
}

/// Verdict for a role tab or the board tab, for `--dry-run`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Create,
    Occupied(String),
    NoAgent,
    Repair,
    LeaveAlone(String),
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Verdict::Create => write!(f, "create"),
            Verdict::Occupied(why) => write!(f, "occupied ({why})"),
            Verdict::NoAgent => write!(f, "no agent"),
            Verdict::Repair => write!(f, "repair"),
            Verdict::LeaveAlone(why) => write!(f, "leave alone ({why})"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub verdicts: Vec<(String, Verdict)>,
    pub steps: Vec<Step>,
}

impl Plan {
    pub fn warnings(&self) -> impl Iterator<Item = &str> {
        self.steps.iter().filter_map(|s| match s {
            Step::Warn(m) => Some(m.as_str()),
            _ => None,
        })
    }

    /// Steps that change something (all but the warnings).
    pub fn actions(&self) -> impl Iterator<Item = &Step> {
        self.steps.iter().filter(|s| !matches!(s, Step::Warn(_)))
    }

    /// One line per role and one for the board with its verdict, as `--dry-run` prints them.
    pub fn verdict_lines(&self) -> String {
        let width = self
            .verdicts
            .iter()
            .map(|(n, _)| n.len())
            .max()
            .unwrap_or(0)
            .max(14);
        self.verdicts
            .iter()
            .map(|(name, verdict)| format!("{name:<width$} {verdict}\n"))
            .collect()
    }

    /// Output of `up --dry-run` (design §6.2).
    pub fn dry_run(&self) -> String {
        let mut out = self.verdict_lines();
        for w in self.warnings() {
            out.push_str(&format!("warning: {w}\n"));
        }
        let actions: Vec<_> = self.actions().collect();
        if actions.is_empty() {
            out.push_str("plan: no steps\n");
        } else {
            out.push_str(&format!("plan: {} steps\n", actions.len()));
            for (i, s) in actions.iter().enumerate() {
                out.push_str(&format!("  {}. {s}\n", i + 1));
            }
        }
        out
    }
}

/// `s` as one POSIX shell word, for a command the user copies.
fn shell_word(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-+:@=,".contains(&b))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn describe(agent: &Agent) -> String {
    match &agent.name {
        Some(n) => format!("agent \"{n}\" in {}", agent.pane_id),
        None => format!("unnamed agent in {}", agent.pane_id),
    }
}

/// Verdict of a tab missing from a restored workspace (§4.4).
const MISSING: &str = "missing after a restore";

/// The plan of `up` (design §4.2 steps 3 to 9).
pub fn plan(c: &Config, s: &HerdrState, e: &Env) -> Plan {
    let mut p = Plan::default();
    let ws = s.workspace.as_ref();
    let tabs = ws.map_or(&[][..], |w| &w.tabs[..]);

    let foreign: Vec<&Tab> = tabs
        .iter()
        .filter(|t| !c.is_known_tab(&t.label) && s.agent_in_tab(&t.id).is_some())
        .collect();
    for t in &foreign {
        p.steps.push(Step::Warn(format!(
            "tab \"{}\" ({}) has a live agent that belongs to no role; no agent will be started \
             until it is renamed with its role's label: herdr tab rename {} <role>",
            t.label, t.id, t.id
        )));
    }

    let mut create = Vec::new();
    for r in &c.roles {
        if let Some(a) = s
            .agents
            .iter()
            .find(|a| a.name.as_deref() == Some(r.name.as_str()))
        {
            p.verdicts
                .push((r.name.clone(), Verdict::Occupied(describe(a))));
            continue;
        }
        let same: Vec<&Tab> = tabs.iter().filter(|t| t.label == r.name).collect();
        if !same.is_empty() {
            if let Some(a) = same.iter().find_map(|t| s.agent_in_tab(&t.id)) {
                p.verdicts
                    .push((r.name.clone(), Verdict::Occupied(describe(a))));
            } else {
                p.verdicts.push((r.name.clone(), Verdict::NoAgent));
                let session_pane = ws.and_then(|w| {
                    w.panes
                        .iter()
                        .filter(|pane| same.iter().any(|t| t.id == pane.tab_id))
                        .find(|pane| pane.agent_session.is_some())
                });
                if let Some(agent) = session_pane.and_then(|pane| pane.session_agent.as_deref())
                    && Kind::parse(agent).is_none()
                {
                    p.steps.push(Step::Warn(format!("tab {} has no agent; its recorded {} session must be recovered with that agent\'s own resume command", r.name, agent)));
                    continue;
                }
                let recorded_kind = session_pane
                    .and_then(|p| p.session_agent.as_deref())
                    .and_then(Kind::parse)
                    .unwrap_or(r.kind);
                let session = session_pane.and_then(|p| p.agent_session.clone());
                if recorded_kind == Kind::Codex {
                    p.steps.push(Step::Warn(format!("tab {} has no agent; use `herdr-crew codex-resume <binding>` in that tab with its saved binding from `herdr-crew codex-list` (recorded session: {})", r.name, session.as_deref().unwrap_or("unknown"))));
                    continue;
                }
                if recorded_kind == Kind::Pi {
                    // The crew's pi extension restores the role prompt from the session file.
                    p.steps.push(Step::Warn(match session {
                        Some(v) => format!(
                            "tab {} has no agent; restart it there with `pi --session {}`",
                            r.name,
                            shell_word(&v)
                        ),
                        None => format!(
                            "tab {} has no agent and herdr knows no session; close the tab and \
                             run `herdr-crew up` to start it again with its prompt",
                            r.name
                        ),
                    }));
                    continue;
                }
                p.steps.push(Step::Warn(match session {
                    Some(v) => format!(
                        "tab {} has no agent; restart it there with `claude -r {v} -n {}`",
                        r.name, r.name
                    ),
                    // Roles start with `-n <role>`: the picker lists the conversations with that
                    // name, possibly older ones, and the user picks one.
                    None => format!(
                        "tab {} has no agent and herdr knows no session; restart it there with \
                         `claude -r {}` and pick its conversation, or, if it never had one, close \
                         the tab and run `herdr-crew up`",
                        r.name, r.name
                    ),
                }));
            }
            continue;
        }
        if let Some(t) = foreign.first() {
            p.verdicts.push((
                r.name.clone(),
                Verdict::LeaveAlone(format!("foreign agent in \"{}\"", t.label)),
            ));
            continue;
        }
        if s.restored {
            // It may have been closed on purpose; only an explicit `up` brings it back.
            p.verdicts
                .push((r.name.clone(), Verdict::LeaveAlone(MISSING.into())));
            p.steps.push(Step::Warn(format!(
                "tab {} is missing; `herdr-crew up` creates it",
                r.name
            )));
            continue;
        }
        p.verdicts.push((r.name.clone(), Verdict::Create));
        create.push(r.name.clone());
    }

    let board_tab = tabs.iter().find(|t| t.label == c.board.tab);
    let workspace = match ws {
        Some(w) => WorkspaceRef::Existing(w.id.clone()),
        None => WorkspaceRef::Created,
    };

    // The initial tab of a new workspace goes to the first role to create that works in the
    // root, or to the board if every role works in a worktree.
    let mut renamed = None;
    if ws.is_none() {
        p.steps.push(match &s.adopt {
            Some(initial) => Step::AdoptWorkspace {
                initial: initial.clone(),
                label: c.label.clone(),
            },
            None => Step::CreateWorkspace {
                label: c.label.clone(),
                cwd: c.root.clone(),
            },
        });
        let first = create
            .iter()
            .find(|n| !c.role(n).is_some_and(|r| r.worktree));
        let label = first.cloned().unwrap_or_else(|| c.board.tab.clone());
        p.steps.push(Step::RenameTab {
            label: label.clone(),
        });
        renamed = Some(label);
    }

    for name in &create {
        tab_steps(c, e, &mut p.steps, &workspace, name, renamed.as_deref());
    }
    for name in &create {
        agent_steps(c, e, &mut p.steps, name);
    }

    let schema = schema::generate(c);
    if e.current_schema.as_deref() != Some(schema.as_str()) {
        p.steps.push(Step::WriteSchema {
            path: c.root.join(prompt::SCHEMA_FILE),
            content: schema,
        });
    }
    if !e.status_exists {
        p.steps.push(Step::SeedStatus {
            path: c.board_path(),
            content: schema::seed(c, &e.now),
        });
    }
    let command = board_command(c, e);
    match board_tab {
        None if s.restored => {
            p.verdicts
                .push((c.board.tab.clone(), Verdict::LeaveAlone(MISSING.into())));
            p.steps.push(Step::Warn(format!(
                "tab {} is missing; `herdr-crew up` creates it",
                c.board.tab
            )));
        }
        None => {
            if renamed.as_deref() != Some(c.board.tab.as_str()) {
                p.steps.push(Step::CreateTab {
                    workspace: workspace.clone(),
                    label: c.board.tab.clone(),
                    cwd: c.root.clone(),
                });
            }
            p.steps.push(Step::RunInPane {
                pane: PaneRef::OfTab(c.board.tab.clone()),
                command,
                new_tab: true,
            });
            p.verdicts.push((c.board.tab.clone(), Verdict::Create));
        }
        Some(tab) => {
            let pane = ws.and_then(|w| w.panes.iter().find(|pane| pane.tab_id == tab.id));
            let verdict = match (pane, &e.board_pane) {
                (None, _) => Verdict::LeaveAlone("the tab has no pane".into()),
                (Some(_), None) => Verdict::LeaveAlone("no pane information".into()),
                (Some(pane), Some(Foreground::Shell)) => {
                    p.steps.push(Step::RunInPane {
                        pane: PaneRef::Existing(pane.id.clone()),
                        command,
                        new_tab: false,
                    });
                    Verdict::Repair
                }
                (Some(_), Some(Foreground::Other(what))) => {
                    Verdict::LeaveAlone(format!("in the foreground: {what}"))
                }
                (Some(_), Some(Foreground::Unknown(why))) => {
                    Verdict::LeaveAlone(format!("no pane information: {why}"))
                }
            };
            p.verdicts.push((c.board.tab.clone(), verdict));
        }
    }
    p
}

fn tab_steps(
    c: &Config,
    e: &Env,
    steps: &mut Vec<Step>,
    workspace: &WorkspaceRef,
    name: &str,
    renamed: Option<&str>,
) {
    let role = c.session_role(name).expect("a session of a role");
    let mut cwd = c.root.clone();
    if role.worktree {
        let w = c
            .worktrees
            .as_ref()
            .expect("validated: worktree requires [worktrees]");
        let path = c.worktree_path(name).expect("[worktrees] is present");
        if !e.worktrees.contains(name) {
            steps.push(Step::CreateWorktree {
                name: name.to_string(),
                path: path.clone(),
                remote: w.remote.clone(),
                branch: w.branch.clone(),
            });
        }
        cwd = path;
    }
    if renamed != Some(name) {
        steps.push(Step::CreateTab {
            workspace: workspace.clone(),
            label: name.to_string(),
            cwd,
        });
    }
}

fn agent_steps(c: &Config, e: &Env, steps: &mut Vec<Step>, name: &str) {
    let path = prompt::prompt_path(c, name);
    steps.push(Step::WritePrompt {
        name: name.to_string(),
        path: path.clone(),
        content: prompt::render(c, name, &e.binary),
    });
    let pane = PaneRef::OfTab(name.to_string());
    steps.push(match &e.agent_cmd {
        Some(cmd) => Step::RunInPane {
            pane,
            command: cmd.clone(),
            new_tab: true,
        },
        None => Step::StartAgent {
            kind: c.session_role(name).unwrap().kind,
            codex: c.session_role(name).unwrap().codex.clone(),
            pi: Box::new(c.session_role(name).unwrap().pi.clone()),
            name: name.to_string(),
            pane,
            prompt: path,
            message: c.start_message(name).map(str::to_string),
        },
    });
}

/// The command the board types in its pane, quoted for the platform's shell.
pub fn board_command(c: &Config, e: &Env) -> String {
    let bin = e.binary.display().to_string();
    let root = c.root.display().to_string();
    let file = c.board_path().display().to_string();
    if e.windows {
        let q = |s: &str| format!("'{}'", s.replace('\'', "''"));
        format!(
            "& {} board --root {} --file {}",
            q(&bin),
            q(&root),
            q(&file)
        )
    } else {
        let q = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
        format!("{} board --root {} --file {}", q(&bin), q(&root), q(&file))
    }
}

/// Label of the only tab of a workspace herdr has just created.
pub const INITIAL_TAB: &str = "1";

/// Why `startup` does not adopt a workspace (§4.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    NoContext,
    NotAlone(usize),
    NotInitial(String),
    Session(String),
    OtherDir(String),
    Foreground(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::NoContext => write!(f, "the startup context has no workspace"),
            Refusal::NotAlone(n) => write!(f, "{n} workspaces, not only herdr's initial one"),
            Refusal::NotInitial(why) => write!(f, "not a fresh initial workspace: {why}"),
            Refusal::Session(v) => write!(f, "its pane has the agent session {v}"),
            Refusal::OtherDir(d) => write!(f, "its pane is in {d}, not in the root"),
            Refusal::Foreground(what) => write!(f, "its shell is not idle: {what}"),
        }
    }
}

/// What `startup` knows about the workspace of its context, to decide whether to adopt it.
pub struct Candidate<'a> {
    /// `workspace_id` of the startup context.
    pub context: Option<&'a str>,
    pub workspaces: &'a [WorkspaceItem],
    pub tabs: &'a [Tab],
    pub panes: &'a [Pane],
    /// The pane's `cwd` with symbolic links resolved.
    pub pane_dir: Option<&'a Path>,
    /// The root with symbolic links resolved.
    pub root: &'a Path,
    pub foreground: &'a Foreground,
}

/// herdr's initial workspace when `startup` may adopt it (§4.4): the context's workspace is the
/// only one, labelled with its directory's name, with one tab «1» and one pane without an agent
/// session, in the root, with its shell idle. Being the only workspace, no other one carries the
/// project's label.
pub fn adoption(k: &Candidate) -> Result<Initial, Refusal> {
    let id = k.context.ok_or(Refusal::NoContext)?;
    let ws = match k.workspaces {
        [w] if w.workspace_id == id => w,
        _ => return Err(Refusal::NotAlone(k.workspaces.len())),
    };
    let (tab, pane) = match (k.tabs, k.panes) {
        ([t], [p]) if t.label == INITIAL_TAB && p.tab_id == t.id => (t, p),
        _ => {
            return Err(Refusal::NotInitial(format!(
                "{} tabs and {} panes",
                k.tabs.len(),
                k.panes.len()
            )));
        }
    };
    if let Some(v) = &pane.agent_session {
        return Err(Refusal::Session(v.clone()));
    }
    let dir = pane.cwd.as_deref().unwrap_or(Path::new(""));
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if ws.label != name {
        return Err(Refusal::NotInitial(format!(
            "label \"{}\" is not its directory's name",
            ws.label
        )));
    }
    if k.pane_dir != Some(k.root) {
        return Err(Refusal::OtherDir(dir.display().to_string()));
    }
    match k.foreground {
        Foreground::Shell => {}
        Foreground::Other(what) | Foreground::Unknown(what) => {
            return Err(Refusal::Foreground(what.clone()));
        }
    }
    Ok(Initial {
        workspace: id.to_string(),
        tab: tab.id.clone(),
        pane: pane.id.clone(),
        label: ws.label.clone(),
    })
}

/// The state `startup` plans over for a project (§4.4): herdr's initial workspace to adopt, or
/// the project's existing workspace to repair, marked `restored`. A project without its
/// workspace that cannot adopt one is left alone: a closed project is not brought back.
pub fn startup_state(
    mut s: HerdrState,
    adoption: Result<Initial, Refusal>,
) -> Result<HerdrState, Refusal> {
    match adoption {
        Ok(initial) => {
            s.workspace = None;
            s.adopt = Some(initial);
            Ok(s)
        }
        Err(why) if s.workspace.is_none() => Err(why),
        Err(_) => {
            s.restored = true;
            Ok(s)
        }
    }
}

/// The plan of `add <role>`: the assigned name and its steps (design §4.2, `add`).
pub fn plan_add(c: &Config, s: &HerdrState, e: &Env, role: &str) -> Result<(String, Plan), String> {
    if !c.role(role).is_some_and(|r| r.extra) {
        return Err(format!("\"{role}\" is not a role with extra = true"));
    }
    let Some(ws) = &s.workspace else {
        return Err(format!(
            "workspace \"{}\" does not exist; start it first with up",
            c.label
        ));
    };
    let max = ws
        .tabs
        .iter()
        .filter_map(|t| {
            if t.label == role {
                Some(1)
            } else {
                extra_number(&t.label, role)
            }
        })
        .max()
        .unwrap_or(1);
    let name = format!("{role}-{}", max + 1);
    let mut p = Plan::default();
    p.verdicts.push((name.clone(), Verdict::Create));
    tab_steps(
        c,
        e,
        &mut p.steps,
        &WorkspaceRef::Existing(ws.id.clone()),
        &name,
        None,
    );
    agent_steps(c, e, &mut p.steps, &name);
    Ok((name, p))
}

/// What `close <name>` closes: the tab, and the worktree it keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Close {
    pub tab_id: String,
    pub worktree: Option<PathBuf>,
}

pub fn plan_close(c: &Config, s: &HerdrState, name: &str) -> Result<Close, String> {
    let Some((role, _)) = c.extra_instance(name) else {
        return Err(format!(
            "only extra instances (<role with extra>-N) can be closed; \"{name}\" is not one"
        ));
    };
    let tab = s
        .workspace
        .as_ref()
        .and_then(|w| w.tabs.iter().find(|t| t.label == name))
        .ok_or_else(|| format!("there is no tab \"{name}\" in workspace \"{}\"", c.label))?;
    Ok(Close {
        tab_id: tab.id.clone(),
        worktree: if role.worktree {
            c.worktree_path(name)
        } else {
            None
        },
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::config::tests::{basic, worktrees};

    fn read_fixture(name: &str) -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        v["result"].clone()
    }

    /// A project migrating from launcher scripts (`herdr-state/`): the four role tabs hold agents
    /// started by hand (`name: null`), the board tab is still labelled "status", and a leftover
    /// tab "6" has no agent.
    fn migrating() -> HerdrState {
        let ws = read_fixture("herdr-state/workspace-list.json");
        let id = find_workspace(&ws, "acme").unwrap().unwrap();
        let tabs = read_fixture("herdr-state/tab-list.json");
        let panes = read_fixture("herdr-state/pane-list.json");
        let agents = read_fixture("herdr-state/agent-list.json");
        HerdrState::from_results(Some((id, &tabs, &panes)), &agents).unwrap()
    }

    #[test]
    fn duplicate_workspace_labels_are_ambiguous() {
        let workspaces = serde_json::json!({"workspaces": [
            {"workspace_id": "w1", "label": "acme"},
            {"workspace_id": "w2", "label": "acme"}
        ]});
        assert!(
            find_workspace(&workspaces, "acme")
                .unwrap_err()
                .contains("ambiguous")
        );
    }

    fn env() -> Env {
        Env {
            binary: "/bin/crew".into(),
            now: "2026-09-26T10:00:00-03:00".into(),
            status_exists: true,
            ..Env::default()
        }
    }

    /// An environment with the schema up to date and the board pane running `fg`.
    fn settled(c: &Config, fg: Foreground) -> Env {
        Env {
            current_schema: Some(schema::generate(c)),
            board_pane: Some(fg),
            ..env()
        }
    }

    fn tab(id: &str, label: &str) -> Tab {
        Tab {
            id: id.into(),
            label: label.into(),
        }
    }

    fn pane(id: &str, tab: &str, session: Option<&str>) -> Pane {
        Pane {
            id: id.into(),
            tab_id: tab.into(),
            agent_session: session.map(Into::into),
            session_agent: None,
            session_kind: None,
            session_source: None,
            cwd: None,
        }
    }

    fn agent(name: Option<&str>, ws: &str, tab: &str, pane: &str) -> Agent {
        Agent {
            kind: None,
            name: name.map(Into::into),
            workspace_id: ws.into(),
            tab_id: tab.into(),
            pane_id: pane.into(),
        }
    }

    /// A workspace `w1` with one tab (and its pane) per label; `with_agent` puts an unnamed agent
    /// in the given tabs.
    fn state(labels: &[&str], with_agent: &[&str]) -> HerdrState {
        let tabs: Vec<Tab> = labels
            .iter()
            .enumerate()
            .map(|(i, l)| tab(&format!("w1:t{i}"), l))
            .collect();
        let panes = tabs
            .iter()
            .enumerate()
            .map(|(i, t)| pane(&format!("w1:p{i}"), &t.id, None))
            .collect();
        let agents = tabs
            .iter()
            .enumerate()
            .filter(|(_, t)| with_agent.contains(&t.label.as_str()))
            .map(|(i, t)| agent(None, "w1", &t.id, &format!("w1:p{i}")))
            .collect();
        HerdrState {
            workspace: Some(Workspace {
                id: "w1".into(),
                tabs,
                panes,
            }),
            agents,
            adopt: None,
            restored: false,
        }
    }

    const ALL: [&str; 5] = [
        "acme-lead",
        "acme-reviewer",
        "acme-designer",
        "acme-dev",
        "acme-status",
    ];

    fn kinds(p: &Plan) -> Vec<String> {
        p.steps
            .iter()
            .map(|s| match s {
                Step::CreateWorkspace { .. } => "workspace".into(),
                Step::AdoptWorkspace { initial, .. } => format!("adopt {}", initial.workspace),
                Step::RenameTab { label } => format!("rename {label}"),
                Step::CreateWorktree { name, .. } => format!("worktree {name}"),
                Step::CreateTab { label, .. } => format!("tab {label}"),
                Step::WritePrompt { name, .. } => format!("prompt {name}"),
                Step::StartAgent { name, .. } => format!("agent {name}"),
                Step::RunInPane { pane, .. } => format!("run {pane}"),
                Step::WriteSchema { .. } => "schema".into(),
                Step::SeedStatus { .. } => "seed".into(),
                Step::Warn(_) => "warn".into(),
            })
            .collect()
    }

    #[test]
    fn empty_state_creates_everything_in_order() {
        let c = basic();
        let p = plan(
            &c,
            &HerdrState::default(),
            &Env {
                status_exists: false,
                ..env()
            },
        );
        assert_eq!(
            kinds(&p),
            [
                "workspace",
                "rename acme-lead",
                "tab acme-reviewer",
                "tab acme-designer",
                "tab acme-dev",
                "prompt acme-lead",
                "agent acme-lead",
                "prompt acme-reviewer",
                "agent acme-reviewer",
                "prompt acme-designer",
                "agent acme-designer",
                "prompt acme-dev",
                "agent acme-dev",
                "schema",
                "seed",
                "tab acme-status",
                "run <pane of \"acme-status\">",
            ]
        );
        assert!(p.verdicts.iter().all(|(_, v)| *v == Verdict::Create));
        let Step::StartAgent { prompt, .. } = &p.steps[6] else {
            panic!()
        };
        assert_eq!(prompt, Path::new("/r/acme/.herdr/prompts/acme-lead.txt"));
    }

    #[test]
    fn worktree_roles_get_their_worktree_and_cwd() {
        let c = worktrees();
        let p = plan(&c, &HerdrState::default(), &env());
        assert_eq!(
            kinds(&p)[..6],
            [
                "workspace",
                "rename globex-lead",
                "worktree globex-dev",
                "tab globex-dev",
                "worktree globex-reviewer",
                "tab globex-reviewer"
            ]
        );
        let Step::CreateTab { cwd, .. } = &p.steps[3] else {
            panic!()
        };
        assert_eq!(cwd, Path::new("/r/globex/.worktrees/globex-dev"));
        // An existing worktree is reused.
        let e = Env {
            worktrees: ["globex-dev".to_string()].into(),
            ..env()
        };
        assert!(
            !kinds(&plan(&c, &HerdrState::default(), &e)).contains(&"worktree globex-dev".into())
        );
    }

    #[test]
    fn complete_workspace_has_no_steps() {
        let c = basic();
        let p = plan(
            &c,
            &state(&ALL, &ALL[..4]),
            &settled(&c, Foreground::Other("herdr-crew".into())),
        );
        assert!(p.steps.is_empty(), "{p:?}");
    }

    #[test]
    fn missing_role_tab_creates_only_that_tab_and_agent() {
        let c = basic();
        let s = state(
            &["acme-lead", "acme-reviewer", "acme-dev", "acme-status"],
            &["acme-lead", "acme-reviewer", "acme-dev"],
        );
        let p = plan(&c, &s, &settled(&c, Foreground::Other("herdr-crew".into())));
        assert_eq!(
            kinds(&p),
            [
                "tab acme-designer",
                "prompt acme-designer",
                "agent acme-designer"
            ]
        );
        let Step::CreateTab { workspace, .. } = &p.steps[0] else {
            panic!()
        };
        assert_eq!(*workspace, WorkspaceRef::Existing("w1".into()));
    }

    #[test]
    fn migrating_project_starts_no_agent() {
        // board.tab = "acme-status" does not exist yet: the board tab is still labelled "status".
        let c = basic();
        let p = plan(&c, &migrating(), &settled(&c, Foreground::Shell));
        assert!(
            !p.steps.iter().any(|s| matches!(s, Step::StartAgent { .. })),
            "{p:?}"
        );
        assert_eq!(
            kinds(&p),
            ["tab acme-status", "run <pane of \"acme-status\">"]
        );
    }

    #[test]
    fn migrating_project_after_renaming_the_board_tab_has_no_steps() {
        let c = basic();
        let mut s = migrating();
        let ws = s.workspace.as_mut().unwrap();
        ws.tabs
            .iter_mut()
            .find(|t| t.label == "status")
            .unwrap()
            .label = "acme-status".into();
        let fg = Foreground::from_result(&read_fixture("herdr-state/process-info-status.json"));
        let p = plan(&c, &s, &settled(&c, fg));
        assert!(p.steps.is_empty(), "{p:?}");
        assert_eq!(
            p.dry_run(),
            "acme-lead      occupied (unnamed agent in w3:p1)\n\
             acme-reviewer  occupied (unnamed agent in w3:p2)\n\
             acme-designer  occupied (unnamed agent in w3:p3)\n\
             acme-dev       occupied (unnamed agent in w3:p4)\n\
             acme-status    leave alone (in the foreground: python3, uv)\n\
             plan: no steps\n"
        );
    }

    #[test]
    fn foreign_agent_blocks_every_agent_but_not_the_board() {
        let c = basic();
        let s = state(&["acme-lead", "6", "zz"], &["acme-lead", "zz"]);
        let p = plan(&c, &s, &settled(&c, Foreground::Shell));
        assert!(!p.steps.iter().any(|s| matches!(s, Step::StartAgent { .. })));
        let warns: Vec<_> = p.warnings().collect();
        assert_eq!(warns.len(), 1);
        assert!(
            warns[0].contains("\"zz\" (w1:t2)")
                && warns[0].contains("herdr tab rename w1:t2 <role>"),
            "{warns:?}"
        );
        assert!(!warns[0].contains("\"6\""));
        assert_eq!(
            kinds(&p),
            ["warn", "tab acme-status", "run <pane of \"acme-status\">"]
        );
        assert_eq!(
            p.verdicts[1],
            (
                "acme-reviewer".into(),
                Verdict::LeaveAlone("foreign agent in \"zz\"".into())
            )
        );
    }

    #[test]
    fn foreign_tab_without_agent_is_ignored() {
        let c = basic();
        let labels = [&ALL[..], &["6"]].concat();
        let p = plan(
            &c,
            &state(&labels, &ALL[..4]),
            &settled(&c, Foreground::Other("herdr-crew".into())),
        );
        assert!(p.steps.is_empty(), "{p:?}");
    }

    #[test]
    fn unnamed_agent_occupies_its_tab() {
        let c = basic();
        let p = plan(
            &c,
            &state(&ALL, &ALL[..4]),
            &settled(&c, Foreground::Other("x".into())),
        );
        assert_eq!(
            p.verdicts[0].1,
            Verdict::Occupied("unnamed agent in w1:p0".into())
        );
    }

    #[test]
    fn extra_instance_alive_and_base_tab_missing() {
        let c = worktrees();
        let s = state(
            &[
                "globex-lead",
                "globex-dev-2",
                "globex-reviewer",
                "globex-status",
            ],
            &["globex-lead", "globex-dev-2", "globex-reviewer"],
        );
        let e = Env {
            worktrees: ["globex-dev".to_string()].into(),
            ..settled(&c, Foreground::Other("herdr-crew".into()))
        };
        assert_eq!(
            kinds(&plan(&c, &s, &e)),
            ["tab globex-dev", "prompt globex-dev", "agent globex-dev"]
        );
    }

    #[test]
    fn role_tab_without_agent_only_warns() {
        let c = basic();
        let mut s = state(&ALL, &ALL[1..4]);
        let e = settled(&c, Foreground::Other("herdr-crew".into()));
        let p = plan(&c, &s, &e);
        assert_eq!(kinds(&p), ["warn"]);
        assert_eq!(
            p.warnings().next().unwrap(),
            "tab acme-lead has no agent and herdr knows no session; restart it there with \
             `claude -r acme-lead` and pick its conversation, or, if it never had one, close the \
             tab and run `herdr-crew up`"
        );
        s.workspace.as_mut().unwrap().panes[0].agent_session = Some("0f3c".into());
        let p = plan(&c, &s, &e);
        assert_eq!(
            p.warnings().next().unwrap(),
            "tab acme-lead has no agent; restart it there with `claude -r 0f3c -n acme-lead`"
        );
        assert_eq!(p.verdicts[0].1, Verdict::NoAgent);
    }

    #[test]
    fn recovery_uses_the_recorded_agent_after_configuration_changes() {
        let mut c = basic();
        let mut s = state(&ALL, &ALL[1..4]);
        let pane = &mut s.workspace.as_mut().unwrap().panes[0];
        pane.agent_session = Some("saved-codex-id".into());
        pane.session_agent = Some("codex".into());
        let p = plan(&c, &s, &settled(&c, Foreground::Other("herdr-crew".into())));
        let warning = p.warnings().next().unwrap();
        assert!(warning.contains("codex-list") && warning.contains("codex-resume"));
        assert!(!warning.contains("claude -r"));
        assert_eq!(kinds(&p), ["warn"]);

        c.roles[0].kind = Kind::Codex;
        s.workspace.as_mut().unwrap().panes[0].session_agent = Some("claude".into());
        let p = plan(&c, &s, &settled(&c, Foreground::Other("herdr-crew".into())));
        assert!(
            p.warnings()
                .next()
                .unwrap()
                .contains("claude -r saved-codex-id")
        );
        assert_eq!(kinds(&p), ["warn"]);

        c.roles[0].kind = Kind::Claude;
        let pane = &mut s.workspace.as_mut().unwrap().panes[0];
        pane.session_agent = Some("pi".into());
        pane.agent_session = Some("/home/u/.pi/agent/sessions/--r--/it's.jsonl".into());
        let p = plan(&c, &s, &settled(&c, Foreground::Other("herdr-crew".into())));
        assert_eq!(
            p.warnings().next().unwrap(),
            "tab acme-lead has no agent; restart it there with \
             `pi --session '/home/u/.pi/agent/sessions/--r--/it'\\''s.jsonl'`"
        );
        assert_eq!(kinds(&p), ["warn"]);
        s.workspace.as_mut().unwrap().panes[0].agent_session = None;
        c.roles[0].kind = Kind::Pi;
        let p = plan(&c, &s, &settled(&c, Foreground::Other("herdr-crew".into())));
        assert!(
            p.warnings()
                .next()
                .unwrap()
                .contains("close the tab and run `herdr-crew up`")
        );
    }

    #[test]
    fn board_repair_only_with_the_shell_in_the_foreground() {
        let c = basic();
        let s = state(&ALL, &ALL[..4]);
        let p = plan(&c, &s, &settled(&c, Foreground::Shell));
        assert_eq!(
            p.steps,
            [Step::RunInPane {
                pane: PaneRef::Existing("w1:p4".into()),
                command: "'/bin/crew' board --root '/r/acme' --file '/r/acme/.herdr/status.json'"
                    .into(),
                new_tab: false,
            }]
        );
        assert_eq!(p.verdicts[4].1, Verdict::Repair);
        let p = plan(&c, &s, &settled(&c, Foreground::Other("vim".into())));
        assert!(p.steps.is_empty());
        assert_eq!(
            p.verdicts[4].1,
            Verdict::LeaveAlone("in the foreground: vim".into())
        );
    }

    #[test]
    fn foreground_from_process_info() {
        let shell = serde_json::json!({"process_info": {"foreground_process_group_id": 7, "shell_pid": 7,
            "foreground_processes": [{"pid": 7, "name": "zsh"}]}});
        assert_eq!(Foreground::from_result(&shell), Foreground::Shell);
        let cat = serde_json::json!({"process_info": {"foreground_process_group_id": 9, "shell_pid": 7,
            "foreground_processes": [{"pid": 9, "name": "cat"}]}});
        assert_eq!(
            Foreground::from_result(&cat),
            Foreground::Other("cat".into())
        );
        assert!(matches!(
            Foreground::from_result(&serde_json::json!({})),
            Foreground::Unknown(_)
        ));
    }

    #[test]
    fn named_agent_elsewhere_blocks_its_role() {
        let c = basic();
        let mut s = state(
            &["acme-lead", "acme-reviewer", "acme-designer", "acme-status"],
            &["acme-lead", "acme-reviewer", "acme-designer"],
        );
        s.agents
            .push(agent(Some("acme-dev"), "w9", "w9:t1", "w9:p1"));
        let p = plan(&c, &s, &settled(&c, Foreground::Other("herdr-crew".into())));
        assert!(p.steps.is_empty(), "{p:?}");
        assert_eq!(
            p.verdicts[3].1,
            Verdict::Occupied("agent \"acme-dev\" in w9:p1".into())
        );
    }

    #[test]
    fn another_project_workspace_is_ignored() {
        // The state only carries the workspace with the project's label: another project's
        // workspace with same-named tabs does not show up, and its unnamed agents occupy nothing.
        let c = basic();
        let others = serde_json::json!({"workspaces": [{"workspace_id": "w2", "label": "other"}]});
        assert_eq!(find_workspace(&others, "acme").unwrap(), None);
        let s = HerdrState {
            workspace: None,
            agents: vec![agent(None, "w2", "w2:t1", "w2:p1")],
            adopt: None,
            restored: false,
        };
        let p = plan(&c, &s, &env());
        assert_eq!(kinds(&p)[..2], ["workspace", "rename acme-lead"]);
        assert_eq!(
            p.steps
                .iter()
                .filter(|s| matches!(s, Step::StartAgent { .. }))
                .count(),
            4
        );
    }

    #[test]
    fn agent_cmd_replaces_agent_start() {
        let c = basic();
        let p = plan(
            &c,
            &HerdrState::default(),
            &Env {
                agent_cmd: Some("cat".into()),
                ..env()
            },
        );
        assert!(!p.steps.iter().any(|s| matches!(s, Step::StartAgent { .. })));
        assert!(p.steps.contains(&Step::RunInPane {
            pane: PaneRef::OfTab("acme-dev".into()),
            command: "cat".into(),
            new_tab: true
        }));
    }

    #[test]
    fn add_numbers_after_the_highest_instance() {
        let c = worktrees();
        let s = state(
            &[
                "globex-lead",
                "globex-dev",
                "globex-dev-2",
                "globex-dev-5",
                "globex-reviewer",
                "globex-devx-9",
            ],
            &[],
        );
        let (name, p) = plan_add(&c, &s, &env(), "globex-dev").unwrap();
        assert_eq!(name, "globex-dev-6");
        assert_eq!(
            kinds(&p),
            [
                "worktree globex-dev-6",
                "tab globex-dev-6",
                "prompt globex-dev-6",
                "agent globex-dev-6"
            ]
        );
        let (name, _) =
            plan_add(&c, &state(&["globex-lead"], &[]), &env(), "globex-reviewer").unwrap();
        assert_eq!(name, "globex-reviewer-2");
        assert!(
            plan_add(&c, &s, &env(), "globex-lead")
                .unwrap_err()
                .contains("is not a role with extra")
        );
        assert!(
            plan_add(&c, &HerdrState::default(), &env(), "globex-dev")
                .unwrap_err()
                .contains("start it first")
        );
    }

    #[test]
    fn close_only_extra_instances() {
        let c = worktrees();
        let s = state(&["globex-lead", "globex-dev", "globex-dev-2"], &[]);
        assert!(
            plan_close(&c, &s, "globex-dev")
                .unwrap_err()
                .contains("only extra instances")
        );
        assert!(plan_close(&c, &s, "globex-lead").is_err());
        let close = plan_close(&c, &s, "globex-dev-2").unwrap();
        assert_eq!(close.tab_id, "w1:t2");
        assert_eq!(
            close.worktree.unwrap(),
            Path::new("/r/globex/.worktrees/globex-dev-2")
        );
        assert!(
            plan_close(&c, &s, "globex-dev-3")
                .unwrap_err()
                .contains("there is no tab")
        );
    }

    #[test]
    fn board_command_quoting() {
        let c = basic();
        let e = Env {
            binary: "/o'k/crew".into(),
            ..env()
        };
        assert_eq!(
            board_command(&c, &e),
            r"'/o'\''k/crew' board --root '/r/acme' --file '/r/acme/.herdr/status.json'"
        );
        let e = Env {
            binary: r"C:\o'k\crew.exe".into(),
            windows: true,
            ..env()
        };
        assert!(board_command(&c, &e).starts_with(r"& 'C:\o''k\crew.exe' board --root "));
    }

    /// herdr's initial workspace for `basic()` (root `/r/acme`) launched from `dir`.
    struct Fresh {
        workspaces: Vec<WorkspaceItem>,
        tabs: Vec<Tab>,
        panes: Vec<Pane>,
        dir: PathBuf,
        foreground: Foreground,
    }

    fn fresh(dir: &str) -> Fresh {
        let label = Path::new(dir).file_name().unwrap().to_str().unwrap();
        Fresh {
            workspaces: vec![WorkspaceItem {
                workspace_id: "w1".into(),
                label: label.into(),
            }],
            tabs: vec![tab("w1:t1", INITIAL_TAB)],
            panes: vec![Pane {
                cwd: Some(dir.into()),
                ..pane("w1:p1", "w1:t1", None)
            }],
            dir: dir.into(),
            foreground: Foreground::Shell,
        }
    }

    fn adopt(c: &Config, f: &Fresh) -> Result<Initial, Refusal> {
        adoption(&Candidate {
            context: Some("w1"),
            workspaces: &f.workspaces,
            tabs: &f.tabs,
            panes: &f.panes,
            pane_dir: Some(&f.dir),
            root: &c.root,
            foreground: &f.foreground,
        })
    }

    #[test]
    fn startup_adopts_the_fresh_initial_workspace() {
        let c = basic();
        let initial = adopt(&c, &fresh("/r/acme")).unwrap();
        assert_eq!(
            initial,
            Initial {
                workspace: "w1".into(),
                tab: "w1:t1".into(),
                pane: "w1:p1".into(),
                label: "acme".into(),
            }
        );
        // The directory is not called like the label: the workspace is renamed.
        let mut c2 = basic();
        c2.label = "acme-crew".into();
        let s = startup_state(HerdrState::default(), adopt(&c2, &fresh("/r/acme"))).unwrap();
        assert_eq!(
            plan(&c2, &s, &env()).steps[0].to_string(),
            "adopt workspace w1 renamed to \"acme-crew\""
        );

        let s = startup_state(HerdrState::default(), Ok(initial)).unwrap();
        let p = plan(
            &c,
            &s,
            &Env {
                status_exists: false,
                ..env()
            },
        );
        assert_eq!(
            kinds(&p)[..4],
            [
                "adopt w1",
                "rename acme-lead",
                "tab acme-reviewer",
                "tab acme-designer"
            ]
        );
        assert!(!kinds(&p).contains(&"workspace".into()));
        assert!(kinds(&p).contains(&"agent acme-lead".into()));
        assert!(kinds(&p).contains(&"tab acme-status".into()));
        assert_eq!(
            p.dry_run().lines().find(|l| l.contains("adopt")),
            Some("  1. adopt workspace w1")
        );
    }

    #[test]
    fn startup_adopts_a_workspace_already_called_like_the_project() {
        // The directory has the project's label (`/r/acme`, label "acme"): the label taken by the
        // candidate itself does not block adoption.
        let c = basic();
        assert_eq!(c.label, "acme");
        let state = state(&[INITIAL_TAB], &[]);
        let s = startup_state(state, adopt(&c, &fresh("/r/acme"))).unwrap();
        assert_eq!(s.workspace, None);
        assert_eq!(
            kinds(&plan(&c, &s, &env()))[..2],
            ["adopt w1", "rename acme-lead"]
        );
    }

    #[test]
    fn startup_refuses_two_workspaces() {
        let c = basic();
        let mut f = fresh("/r/acme");
        f.workspaces.push(WorkspaceItem {
            workspace_id: "w2".into(),
            label: "other".into(),
        });
        assert_eq!(adopt(&c, &f), Err(Refusal::NotAlone(2)));
    }

    #[test]
    fn startup_refuses_a_renamed_workspace() {
        let c = basic();
        let mut f = fresh("/r/acme");
        f.workspaces[0].label = "mine".into();
        assert!(matches!(adopt(&c, &f), Err(Refusal::NotInitial(_))));
        let mut f = fresh("/r/acme");
        f.tabs[0].label = "notes".into();
        assert!(matches!(adopt(&c, &f), Err(Refusal::NotInitial(_))));
        let mut f = fresh("/r/acme");
        f.tabs.push(tab("w1:t2", "2"));
        assert!(matches!(adopt(&c, &f), Err(Refusal::NotInitial(_))));
    }

    #[test]
    fn startup_refuses_a_pane_with_an_agent_session() {
        let c = basic();
        let mut f = fresh("/r/acme");
        f.panes[0].agent_session = Some("0f3c".into());
        assert_eq!(adopt(&c, &f), Err(Refusal::Session("0f3c".into())));
    }

    #[test]
    fn startup_refuses_a_pane_outside_the_root() {
        let c = basic();
        // A subdirectory of the project resolves to the same root but is not the root.
        let f = fresh("/r/acme/docs");
        assert_eq!(adopt(&c, &f), Err(Refusal::OtherDir("/r/acme/docs".into())));
    }

    #[test]
    fn startup_refuses_a_busy_shell() {
        let c = basic();
        let mut f = fresh("/r/acme");
        f.foreground = Foreground::Other("vim".into());
        assert_eq!(adopt(&c, &f), Err(Refusal::Foreground("vim".into())));
    }

    #[test]
    fn startup_refuses_when_the_label_exists() {
        // Another workspace with the project's label: it is not the only workspace.
        let c = basic();
        let mut f = fresh("/r/acme");
        f.workspaces.insert(
            0,
            WorkspaceItem {
                workspace_id: "w0".into(),
                label: "acme".into(),
            },
        );
        assert_eq!(adopt(&c, &f), Err(Refusal::NotAlone(2)));
        let none = Candidate {
            context: None,
            workspaces: &f.workspaces,
            tabs: &f.tabs,
            panes: &f.panes,
            pane_dir: Some(&f.dir),
            root: &c.root,
            foreground: &f.foreground,
        };
        assert_eq!(adoption(&none), Err(Refusal::NoContext));
    }

    #[test]
    fn startup_repairs_only_an_existing_workspace() {
        let c = basic();
        // Restore: the project's workspace is back with its tabs, the agents are not running yet
        // and the board pane is a bare shell. Nothing is relaunched; only the board is typed.
        let s = startup_state(state(&ALL, &[]), Err(Refusal::NotAlone(3))).unwrap();
        let p = plan(&c, &s, &settled(&c, Foreground::Shell));
        assert!(!p.steps.iter().any(|s| matches!(
            s,
            Step::StartAgent { .. } | Step::CreateTab { .. } | Step::CreateWorkspace { .. }
        )));
        assert_eq!(
            p.verdicts
                .iter()
                .filter(|(_, v)| *v == Verdict::NoAgent)
                .count(),
            4
        );
        assert!(matches!(
            p.actions().collect::<Vec<_>>()[..],
            [Step::RunInPane { new_tab: false, .. }]
        ));
        assert!(s.restored);
        // A closed project (no workspace with its label) is not brought back.
        assert_eq!(
            startup_state(HerdrState::default(), Err(Refusal::NotAlone(2))),
            Err(Refusal::NotAlone(2))
        );
    }

    #[test]
    fn startup_repair_leaves_a_missing_role_tab_to_up() {
        // acme-designer was closed on purpose: a restart does not bring it back with a fresh
        // conversation, and the board is still repaired.
        let c = basic();
        let s = state(
            &["acme-lead", "acme-reviewer", "acme-dev", "acme-status"],
            &[],
        );
        let s = startup_state(s, Err(Refusal::NotAlone(2))).unwrap();
        let p = plan(&c, &s, &settled(&c, Foreground::Shell));
        assert!(!p.steps.iter().any(|s| matches!(
            s,
            Step::CreateTab { .. } | Step::StartAgent { .. } | Step::CreateWorktree { .. }
        )));
        assert!(!p.verdicts.iter().any(|(_, v)| *v == Verdict::Create));
        assert_eq!(
            p.verdicts[2],
            (
                "acme-designer".into(),
                Verdict::LeaveAlone("missing after a restore".into())
            )
        );
        assert!(
            p.warnings()
                .any(|w| w == "tab acme-designer is missing; `herdr-crew up` creates it")
        );
        assert!(matches!(
            p.actions().collect::<Vec<_>>()[..],
            [Step::RunInPane { new_tab: false, .. }]
        ));
        // An explicit `up` over the same workspace creates it.
        let s = state(
            &["acme-lead", "acme-reviewer", "acme-dev", "acme-status"],
            &[],
        );
        assert!(
            kinds(&plan(&c, &s, &settled(&c, Foreground::Shell)))
                .contains(&"agent acme-designer".into())
        );
    }

    #[test]
    fn verdict_lines_are_the_head_of_the_dry_run() {
        let c = basic();
        let mut s = state(&ALL, &ALL[1..4]);
        // Restored: herdr lists the role's named agent before its process runs again.
        s.agents
            .push(agent(Some("acme-lead"), "w1", "w1:t0", "w1:p0"));
        let p = plan(&c, &s, &settled(&c, Foreground::Shell));
        let lines = p.verdict_lines();
        assert_eq!(
            lines,
            "acme-lead      occupied (agent \"acme-lead\" in w1:p0)\n\
             acme-reviewer  occupied (unnamed agent in w1:p1)\n\
             acme-designer  occupied (unnamed agent in w1:p2)\n\
             acme-dev       occupied (unnamed agent in w1:p3)\n\
             acme-status    repair\n"
        );
        assert!(p.dry_run().starts_with(&lines));
    }

    /// The messages of the `StartAgent` steps, by session.
    fn messages(p: &Plan) -> Vec<(String, Option<String>)> {
        p.steps
            .iter()
            .filter_map(|s| match s {
                Step::StartAgent { name, message, .. } => Some((name.clone(), message.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn start_message_only_for_new_tabs() {
        let mut c = worktrees();
        c.start_message = Some("Confirma tu rol «en una línea».".into());
        let hello = Some("Confirma tu rol «en una línea».".to_string());
        // up over an empty herdr: every new tab gets it.
        let p = plan(&c, &HerdrState::default(), &env());
        assert!(!messages(&p).is_empty());
        assert!(messages(&p).iter().all(|(_, m)| *m == hello));
        // Adoption: the same.
        let initial = Initial {
            workspace: "w1".into(),
            tab: "w1:t1".into(),
            pane: "w1:p1".into(),
            label: "globex".into(),
        };
        let s = startup_state(HerdrState::default(), Ok(initial)).unwrap();
        assert!(
            messages(&plan(&c, &s, &env()))
                .iter()
                .all(|(_, m)| *m == hello)
        );
        // add of an extra instance: its base role's.
        let s = state(&["globex-lead", "globex-dev", "globex-reviewer"], &[]);
        let (_, p) = plan_add(&c, &s, &env(), "globex-dev").unwrap();
        assert_eq!(messages(&p), [("globex-dev-2".to_string(), hello.clone())]);
        // up over tabs without an agent (a resume that did not happen) and startup's repair after
        // a restore start nothing, so they send nothing.
        let e = settled(&c, Foreground::Shell);
        let full = state(
            &[
                "globex-lead",
                "globex-dev",
                "globex-reviewer",
                "globex-status",
            ],
            &[],
        );
        assert!(messages(&plan(&c, &full, &e)).is_empty());
        let restored = startup_state(full, Err(Refusal::NotAlone(2))).unwrap();
        assert!(messages(&plan(&c, &restored, &e)).is_empty());
        // Without start_message the step carries none.
        assert!(
            messages(&plan(&worktrees(), &HerdrState::default(), &env()))
                .iter()
                .all(|(_, m)| m.is_none())
        );
    }
}
