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
}

/// How a started agent was left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Started {
    Ready,
    /// It runs but waits for an answer at startup (e.g. trusting the folder).
    Waiting,
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

    /// Whether `focus_and_attach` would take over this terminal.
    fn will_attach(&self, attach: bool) -> bool;

    /// Shows the workspace and, from a plain terminal, opens the host's UI on it.
    fn focus_and_attach(&self, workspace: &str, root: &Path, attach: bool) -> Result<(), String>;

    /// The Herdr TUI, for the features only it has: Codex pane bindings and native queue,
    /// agy's pane-keyed launch slot, and plugin startup.
    fn tui(&self) -> Option<&Herdr> {
        None
    }
}
