//! The seam between the pure core and the terminal host that runs the crew's agents.
//!
//! `plan.rs` describes what `up`, `add` and `close` need in host-neutral terms
//! ([`HerdrState`], [`Step`](crate::plan::Step)); a [`Backend`] reads that state and carries out
//! the steps. [`crate::herdr::Herdr`] implements it over the Herdr TUI CLI. A herdr-core backend
//! will implement it over `hc` (docs/herdr-core.md).

use std::path::Path;

use crate::agent::Kind;
use crate::config::Config;
use crate::herdr::{Herdr, Output};
use crate::plan::{Foreground, HerdrState};

/// A new workspace with its initial tab and that tab's pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    pub workspace: String,
    pub tab: String,
    pub pane: String,
}

/// An agent to start in an existing pane.
#[derive(Debug, Clone, Copy)]
pub struct AgentStart<'a> {
    /// The session name, global across the host.
    pub name: &'a str,
    pub pane: &'a str,
    pub kind: Kind,
    /// The agent's own arguments, after its program name.
    pub args: &'a [String],
    /// The first message, typed once the agent is ready.
    pub message: Option<&'a str>,
    /// The recorded session to resume instead of starting a new one.
    pub resume: Option<&'a str>,
}

/// How a started agent was left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Started {
    Ready,
    /// It runs but waits for an answer at startup (e.g. trusting the folder).
    Waiting,
    /// A resume found no conversation to resume (the agent died before it saved one), so a
    /// new conversation started under the same session id.
    NewConversation {
        waiting: bool,
    },
}

/// An agent's state, as `send` needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentState {
    /// Idle, done or working: a message can be queued.
    Available,
    /// At an approval or question dialog, which only the user answers.
    Blocked,
    /// Anything else, as the host reports it.
    Other(String),
}

/// Why a prompt could not be delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptError {
    Blocked,
    Failed(String),
}

/// What [`Backend::load`] does when no host server answers.
pub enum Load<'a> {
    /// Start one in the project root (not in an action), reporting it to the output.
    Start(&'a Path, &'a mut Output),
    /// Only read: fail with [`LoadError::Unreachable`].
    Read,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// No host server answers. `denied` when the socket itself is refused, as in a sandbox.
    Unreachable { message: String, denied: bool },
    /// The server answered, but the state is unusable (e.g. an unverifiable workspace).
    Failed(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Unreachable { message, .. } => {
                write!(f, "the herdr server does not answer: {message}")
            }
            LoadError::Failed(message) => f.write_str(message),
        }
    }
}

/// Environment variables for a new pane's shell.
pub type PaneEnv = [(String, String)];

pub trait Backend {
    /// Runs as a host action, with no visible terminal: warnings also become notifications.
    fn action(&self) -> bool;

    /// How the user sees the crew's workspace, for `up`'s closing summary.
    fn open_hint(&self, label: &str, root: &Path) -> String;

    /// Shows `message` as a host notification when this is an action (see `Herdr::notifies`).
    fn notify(&self, message: &str);

    /// The crew's state.
    fn load(&self, c: &Config, mode: Load) -> Result<HerdrState, LoadError>;

    /// What runs in the foreground of a pane.
    fn foreground(&self, pane: &str) -> Foreground;

    /// Checks that the host supports the native integrations of Codex, pi and agy roles.
    fn require_native(&self, kind: &str) -> Result<(), String>;

    fn create_workspace(&self, label: &str, cwd: &Path, env: &PaneEnv) -> Result<Created, String>;

    fn rename_workspace(&self, workspace: &str, label: &str) -> Result<(), String>;

    fn rename_tab(&self, tab: &str, label: &str) -> Result<(), String>;

    /// Creates a tab and returns its pane.
    fn create_tab(
        &self,
        workspace: &str,
        label: &str,
        cwd: &Path,
        env: &PaneEnv,
    ) -> Result<String, String>;

    fn start_agent(&self, start: &AgentStart) -> Result<Started, String>;

    /// Types `command` and Enter into the pane's shell.
    fn run_in_pane(&self, pane: &str, command: &str) -> Result<(), String>;

    fn close_tab(&self, tab: &str) -> Result<(), String>;

    fn agent_state(&self, pane: &str) -> Result<AgentState, String>;

    /// Submits `text` to the agent in `pane`.
    fn prompt(&self, pane: &str, text: &str) -> Result<(), PromptError>;

    /// Whether a lost agent leaves its tab as a shell that `up` resumes the session in (see
    /// `Env::resume`).
    fn resumes_agents(&self) -> bool {
        false
    }

    /// Whether `focus_and_attach` would take over this terminal.
    fn will_attach(&self, attach: bool) -> bool;

    /// Shows the workspace and, from a plain terminal, opens the host's UI on it.
    fn focus_and_attach(&self, workspace: &str, root: &Path, attach: bool) -> Result<(), String>;

    /// The key an agent's hook finds its pane by, from inside that pane: the Herdr TUI's
    /// `HERDR_PANE_ID`, or herdr-core's session and terminal (see [`core_pane_key`]).
    fn pane_key(&self, pane: &str) -> String {
        pane.to_string()
    }

    /// Where a Codex role starting in `pane` runs, for its binding (see `codex::prepare`).
    fn codex_context(&self, pane: &str) -> Result<crate::codex::Context, String>;

    /// Best effort, once the plan ran: names and orders the crew's space and tabs where the
    /// host only can while its window is open (the Herdr app).
    fn tidy(&self, _c: &Config) {}

    /// The Herdr TUI, for the features only it has: Codex pane bindings and native queue,
    /// plugin startup.
    fn tui(&self) -> Option<&Herdr> {
        None
    }
}

/// The herdr-core session that `hc` and its terminals use (`HC_SESSION`, else `default`).
pub fn core_session() -> String {
    std::env::var("HC_SESSION")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "default".into())
}

/// The key of a herdr-core terminal for hooks: addresses repeat across core sessions.
pub fn core_pane_key(session: &str, address: &str) -> String {
    format!("hc:{session}:{address}")
}

/// The keys a hook running in this process's terminal may find its pane by: the Herdr TUI
/// pane's, then the herdr-core terminal's. A TUI pane inside an app terminal has both.
pub fn hook_pane_keys() -> Vec<String> {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let mut keys = Vec::new();
    if let Some(pane) = var("HERDR_PANE_ID") {
        keys.push(pane);
    }
    if let Some(address) = var("HC_TERMINAL_ID") {
        keys.push(core_pane_key(&core_session(), &address));
    }
    keys
}

/// Which host a command drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    /// The Herdr TUI (`herdr`), the plugin host.
    Herdr,
    /// herdr-core (`hc`), shown by the Herdr app.
    Core,
}

impl Host {
    /// `CREW_HOST` (`herdr` or `herdr-core`) wins. Otherwise a Herdr plugin action or hook uses
    /// the TUI, a command run inside a herdr-core terminal (`HC_TERMINAL_ID`, which every Herdr
    /// app terminal has) uses core, and anything else the TUI.
    /// `CREW_HOST`, else the host whose terminal runs this process. With both a TUI pane's
    /// and a herdr-core terminal's variables (one host runs inside the other, or a core
    /// started from a TUI pane passed `HERDR_PANE_ID` to its terminals), `inner` names the
    /// nearer one; without an answer the TUI wins.
    pub fn detect(
        var: impl Fn(&str) -> Option<String>,
        inner: impl FnOnce() -> Option<Host>,
    ) -> Result<Host, String> {
        match var("CREW_HOST").as_deref() {
            Some("herdr") => return Ok(Host::Herdr),
            Some("herdr-core" | "hc") => return Ok(Host::Core),
            Some(other) => {
                return Err(format!(
                    "CREW_HOST must be \"herdr\" or \"herdr-core\", not {other:?}"
                ));
            }
            None => {}
        }
        Ok(if var("HERDR_PLUGIN_ID").is_some() {
            Host::Herdr
        } else if var("HC_TERMINAL_ID").is_some() {
            if var("HERDR_PANE_ID").is_some() {
                inner().unwrap_or(Host::Herdr)
            } else {
                Host::Core
            }
        } else {
            Host::Herdr
        })
    }

    pub fn from_env() -> Result<Host, String> {
        Host::detect(
            |k| std::env::var(k).ok().filter(|v| !v.is_empty()),
            nearest_host,
        )
    }
}

/// The host server nearest this process among its ancestors: `herdr` (the TUI) or
/// `herdr-core`/`hc` (the core), from `ps`.
fn nearest_host() -> Option<Host> {
    let mut pid = std::os::unix::process::parent_id();
    for _ in 0..32 {
        if pid <= 1 {
            return None;
        }
        let out = std::process::Command::new("ps")
            .args(["-o", "ppid=,comm=", "-p", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        let line = String::from_utf8_lossy(&out.stdout);
        let (ppid, comm) = line.trim().split_once(char::is_whitespace)?;
        match host_process(comm.trim()) {
            Some(host) => return Some(host),
            None => pid = ppid.trim().parse().ok()?,
        }
    }
    None
}

/// The host a server process's command name belongs to.
fn host_process(comm: &str) -> Option<Host> {
    match comm.rsplit('/').next()? {
        "herdr" => Some(Host::Herdr),
        "herdr-core" | "hc" => Some(Host::Core),
        _ => None,
    }
}

/// The backend of the detected host.
pub fn from_env() -> Result<Box<dyn Backend>, String> {
    Ok(match Host::from_env()? {
        Host::Herdr => Box::new(Herdr::from_env()),
        Host::Core => Box::new(crate::hc::Hc::from_env()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(vars: &[(&str, &str)]) -> Result<Host, String> {
        detect_within(vars, None)
    }

    fn detect_within(vars: &[(&str, &str)], inner: Option<Host>) -> Result<Host, String> {
        Host::detect(
            |k| {
                vars.iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            },
            || inner,
        )
    }

    #[test]
    fn host_detection() {
        assert_eq!(detect(&[]), Ok(Host::Herdr));
        assert_eq!(detect(&[("HC_TERMINAL_ID", "t3")]), Ok(Host::Core));
        assert_eq!(
            detect(&[("HC_TERMINAL_ID", "t3"), ("HERDR_PLUGIN_ID", "herdr-crew")]),
            Ok(Host::Herdr),
            "a TUI plugin action stays on the TUI"
        );
        assert_eq!(
            detect(&[("HC_TERMINAL_ID", "t3"), ("CREW_HOST", "herdr")]),
            Ok(Host::Herdr)
        );
        assert_eq!(detect(&[("CREW_HOST", "herdr-core")]), Ok(Host::Core));
        assert!(detect(&[("CREW_HOST", "tmux")]).is_err());
        // Both hosts' variables: the nearer server decides, else the TUI.
        let both = [("HC_TERMINAL_ID", "t3"), ("HERDR_PANE_ID", "w1:p1")];
        assert_eq!(detect_within(&both, Some(Host::Core)), Ok(Host::Core));
        assert_eq!(detect_within(&both, Some(Host::Herdr)), Ok(Host::Herdr));
        assert_eq!(detect_within(&both, None), Ok(Host::Herdr));
    }

    #[test]
    fn host_processes() {
        assert_eq!(
            host_process("/Users/me/.local/bin/herdr"),
            Some(Host::Herdr)
        );
        assert_eq!(
            host_process("/x/Herdr Core.app/Contents/MacOS/herdr-core"),
            Some(Host::Core)
        );
        assert_eq!(host_process("hc"), Some(Host::Core));
        assert_eq!(host_process("-zsh"), None);
        assert_eq!(host_process("herdr-crew"), None);
    }
}
