//! [`Backend`] over herdr-core (`hc`), shown by the Herdr app (docs/herdr-core.md).
//!
//! herdr-core has terminals, not workspaces or tabs. The crew maps them onto labels that the
//! Herdr app adopts (`~/.config/herdr-app/AGENTS.md`):
//!
//! - the project's space is `herdr.space=<space id>`, an id derived from the main checkout;
//! - each tab is one kept terminal running a login shell, `herdr.tab=<tab id>`;
//! - `crew.tab=<label>` names the tab for the crew (the role or the board), and
//!   `crew.agent=<name>` / `crew.kind=<kind>` record the agent the crew typed into its shell.
//!
//! Every `hc` call runs without the variables of a Herdr TUI pane or a Claude Code session: a
//! call may start the core, and its terminals inherit the core's environment.

use std::cell::OnceCell;
use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::agent::Kind;
use crate::backend::{
    AgentStart, AgentState, Backend, Created, Load, LoadError, PaneEnv, PromptError, Started,
};
use crate::config::Config;
use crate::files;
use crate::plan::{Agent, Foreground, HerdrState, Pane, Tab, Workspace, shell_word};

/// Whether a variable of this process stays out of `hc`, and so out of the core it may start:
/// a Herdr TUI pane's, a Claude Code or Codex session's, or this process's own hc terminal's.
/// `HC_SESSION` and `HC_SOCKET` stay: they choose the core.
fn host_variable(name: &str) -> bool {
    name.starts_with("HERDR_")
        || name.starts_with("CLAUDE_CODE_")
        || name == "CLAUDECODE"
        || name == "CODEX_THREAD_ID"
        || name == "CREW_CODEX_BINDING"
        || (name.starts_with("HC_") && name != "HC_SESSION" && name != "HC_SOCKET")
}

/// How long `start_agent` waits for a new shell to reach its prompt.
const SHELL_WAIT: Duration = Duration::from_secs(30);

/// How long `start_agent` waits for the agent's first status report.
const READY_WAIT: Duration = Duration::from_secs(60);

pub struct Hc {
    bin: OsString,
    /// `herdr-app`, for names and focus in an open window; optional.
    app: OsString,
    /// The project's space, set by `load`, which runs before any step.
    space: OnceCell<String>,
}

#[derive(Debug)]
struct HcError {
    kind: String,
    message: String,
}

impl std::fmt::Display for HcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl Hc {
    pub fn from_env() -> Hc {
        Hc {
            bin: std::env::var_os("CREW_HC_BIN").unwrap_or_else(|| "hc".into()),
            app: std::env::var_os("CREW_HERDR_APP_BIN").unwrap_or_else(|| "herdr-app".into()),
            space: OnceCell::new(),
        }
    }

    fn command(&self, program: &OsString) -> Command {
        let mut cmd = Command::new(program);
        for (name, _) in std::env::vars_os() {
            if name.to_str().is_some_and(host_variable) {
                cmd.env_remove(name);
            }
        }
        cmd.stdin(Stdio::null());
        cmd
    }

    /// Runs `hc <args> --json` and returns its JSON answer.
    fn call(&self, args: &[&str]) -> Result<Value, HcError> {
        let out = self
            .command(&self.bin)
            .args(args)
            .arg("--json")
            .output()
            .map_err(|e| HcError {
                kind: "no_hc".into(),
                message: format!("could not run hc: {e}"),
            })?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let parsed: Option<Value> = serde_json::from_str(stdout.trim())
            .ok()
            .or_else(|| serde_json::from_str(stderr.trim()).ok());
        match parsed {
            Some(v) if v.get("error").is_some() => Err(HcError {
                kind: v["error"]["kind"].as_str().unwrap_or("error").into(),
                message: v["error"]["message"].as_str().unwrap_or_default().into(),
            }),
            Some(v) if out.status.success() => Ok(v),
            _ => Err(HcError {
                kind: "output".into(),
                message: format!("{} {}", stdout.trim(), stderr.trim()).trim().into(),
            }),
        }
    }

    /// Best effort `herdr-app do <command> --key value…`: the window may be closed.
    fn app_do(&self, command: &str, args: &[(&str, &str)]) {
        let mut cmd = self.command(&self.app);
        cmd.args(["do", command]);
        for (k, v) in args {
            cmd.arg(format!("--{k}")).arg(v);
        }
        let _ = cmd.stdout(Stdio::null()).stderr(Stdio::null()).status();
    }

    fn terminal(&self, address: &str) -> Result<Value, HcError> {
        self.call(&["get", address])
    }

    /// The live terminals carrying `key=value`, with their full facts.
    fn labeled(&self, key: &str, value: &str) -> Result<Vec<Value>, HcError> {
        let label = format!("{key}={value}");
        let list = self.call(&["ls", "--label", &label])?;
        let mut terminals = Vec::new();
        for t in list["terminals"].as_array().into_iter().flatten() {
            if let Some(address) = t["address"].as_str() {
                terminals.push(self.terminal(address)?);
            }
        }
        terminals.sort_by_key(created);
        Ok(terminals)
    }

    /// The terminal hosting `tab`.
    fn tab_terminal(&self, tab: &str) -> Result<String, String> {
        let terminals = self
            .labeled("herdr.tab", tab)
            .map_err(|e| format!("could not find tab {tab}: {e}"))?;
        terminals
            .first()
            .and_then(|t| t["address"].as_str())
            .map(str::to_string)
            .ok_or_else(|| format!("tab {tab} has no live terminal"))
    }

    /// A kept login shell in a new tab of `space`.
    fn new_tab(
        &self,
        space: &str,
        label: Option<&str>,
        cwd: &Path,
        env: &PaneEnv,
    ) -> Result<(String, String), String> {
        let tab = format!(
            "{space}-{}",
            files::random_hex(4).map_err(|e| e.to_string())?
        );
        let cwd = cwd.to_string_lossy();
        let mut labels = vec![format!("herdr.space={space}"), format!("herdr.tab={tab}")];
        if let Some(label) = label {
            labels.push(format!("crew.tab={label}"));
        }
        let mut args: Vec<&str> = vec!["run", "--bg", "--no-parent", "--no-notify", "--keep"];
        args.extend(["--cwd", &cwd]);
        for l in &labels {
            args.extend(["--label", l]);
        }
        // `hc run` has no --env: `env` sets the variables and execs the login shell.
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let assignments: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        if !env.is_empty() {
            args.extend(["--", "env"]);
            args.extend(assignments.iter().map(String::as_str));
            args.extend([shell.as_str(), "-l"]);
        }
        let r = self.call(&args).map_err(|e| e.to_string())?;
        let address = r["address"]
            .as_str()
            .ok_or("hc run answered without an address")?
            .to_string();
        Ok((tab, address))
    }
}

/// The creation time of a terminal, to keep the first one first.
fn created(t: &Value) -> u64 {
    t["created_ms"].as_u64().unwrap_or(u64::MAX)
}

fn label<'a>(t: &'a Value, key: &str) -> Option<&'a str> {
    t["labels"][key].as_str()
}

/// The shell itself is in the foreground: nothing runs in it.
fn shell_in_front(t: &Value) -> bool {
    match (t["foreground"]["pid"].as_u64(), t["pid"].as_u64()) {
        (Some(fg), Some(pid)) => fg == pid,
        _ => false,
    }
}

/// The agent kind from an OSC 7501 `app`.
fn kind_of_app(app: &str) -> String {
    match app {
        "claude-code" => "claude".into(),
        "antigravity" => "agy".into(),
        other => other.into(),
    }
}

/// The project's space id: stable for its main checkout, readable in labels.
pub fn space_id(root: &Path) -> String {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let hash = files::fnv64(0xcbf2_9ce4_8422_2325, root.to_string_lossy().as_bytes());
    format!("crew-{hash:016x}")
}

/// The crew's state from its space's terminals (those with `crew.tab`).
fn state_of(space: &str, terminals: &[Value]) -> HerdrState {
    let mut tabs: Vec<Tab> = Vec::new();
    let mut panes = Vec::new();
    let mut agents = Vec::new();
    for t in terminals {
        let (Some(address), Some(tab)) = (t["address"].as_str(), label(t, "herdr.tab")) else {
            continue;
        };
        if let Some(name) = label(t, "crew.tab")
            && !tabs.iter().any(|x| x.id == tab)
        {
            tabs.push(Tab {
                id: tab.into(),
                label: name.into(),
            });
        }
        panes.push(Pane {
            id: address.into(),
            tab_id: tab.into(),
            agent_session: None,
            session_agent: None,
            session_kind: None,
            session_source: None,
            cwd: t["cwd"].as_str().map(Into::into),
        });
        // An agent runs while something other than the shell is in front and either the crew
        // started it or it reports a status.
        let app = t["status"]["app"].as_str();
        let crew_agent = label(t, "crew.agent");
        if !shell_in_front(t) && (crew_agent.is_some() || app.is_some()) {
            agents.push(Agent {
                kind: app
                    .map(kind_of_app)
                    .or_else(|| label(t, "crew.kind").map(str::to_string)),
                name: crew_agent.map(str::to_string),
                workspace_id: space.into(),
                tab_id: tab.into(),
                pane_id: address.into(),
            });
        }
    }
    let workspace = (!panes.is_empty()).then(|| Workspace {
        id: space.into(),
        tabs,
        panes,
    });
    HerdrState {
        workspace,
        agents,
        adopt: None,
        restored: false,
    }
}

impl Backend for Hc {
    fn action(&self) -> bool {
        false
    }

    fn notify(&self, _message: &str) {}

    fn load(&self, c: &Config, _mode: Load) -> Result<HerdrState, LoadError> {
        let space = self.space.get_or_init(|| space_id(&c.root)).clone();
        let terminals =
            self.labeled("herdr.space", &space)
                .map_err(|e| LoadError::Unreachable {
                    denied: e.kind == "output"
                        && (e.message.contains("Operation not permitted")
                            || e.message.contains("PermissionDenied")),
                    message: e.to_string(),
                })?;
        Ok(state_of(&space, &terminals))
    }

    fn foreground(&self, pane: &str) -> Foreground {
        match self.terminal(pane) {
            Ok(t) if shell_in_front(&t) => Foreground::Shell,
            Ok(t) if t["foreground"].is_object() => Foreground::Other(
                t["foreground"]["name"]
                    .as_str()
                    .or_else(|| t["foreground"]["argv"][0].as_str())
                    .unwrap_or("?")
                    .into(),
            ),
            Ok(_) => Foreground::Unknown("hc reported no foreground process".into()),
            Err(e) => Foreground::Unknown(e.to_string()),
        }
    }

    /// herdr-core needs no integration for status: agents report it themselves.
    fn require_native(&self, _kind: &str) -> Result<(), String> {
        Ok(())
    }

    fn create_workspace(&self, label: &str, cwd: &Path, env: &PaneEnv) -> Result<Created, String> {
        let space = self
            .space
            .get()
            .cloned()
            .ok_or("the crew's state was not loaded")?;
        let (tab, pane) = self
            .new_tab(&space, None, cwd, env)
            .map_err(|e| format!("could not create workspace \"{label}\": {e}"))?;
        self.app_do("space.rename", &[("id", &space), ("name", label)]);
        Ok(Created {
            workspace: space,
            tab,
            pane,
        })
    }

    fn rename_workspace(&self, workspace: &str, label: &str) -> Result<(), String> {
        self.app_do("space.rename", &[("id", workspace), ("name", label)]);
        Ok(())
    }

    fn rename_tab(&self, tab: &str, label: &str) -> Result<(), String> {
        let terminal = self.tab_terminal(tab)?;
        let crew = format!("crew.tab={label}");
        self.call(&["label", &terminal, &crew])
            .map_err(|e| format!("could not rename {tab}: {e}"))?;
        self.app_do("tab.rename", &[("id", tab), ("name", label)]);
        Ok(())
    }

    fn create_tab(
        &self,
        workspace: &str,
        label: &str,
        cwd: &Path,
        env: &PaneEnv,
    ) -> Result<String, String> {
        let (tab, pane) = self
            .new_tab(workspace, Some(label), cwd, env)
            .map_err(|e| format!("could not create tab \"{label}\": {e}"))?;
        self.app_do("tab.rename", &[("id", &tab), ("name", label)]);
        Ok(pane)
    }

    fn start_agent(&self, s: &AgentStart) -> Result<Started, String> {
        let fail = |e: HcError| format!("could not start {} in {}: {e}", s.name, s.pane);
        // A new terminal's shell may still be reading its startup files.
        let since = Instant::now();
        while !shell_in_front(&self.terminal(s.pane).map_err(fail)?) {
            if since.elapsed() >= SHELL_WAIT {
                return Err(format!(
                    "could not start {} in {}: its shell is not in the foreground",
                    s.name, s.pane
                ));
            }
            sleep(Duration::from_millis(250));
        }
        let mut words = vec![s.kind.as_str().to_string()];
        words.extend(s.args.iter().cloned());
        if let Some(msg) = s.message {
            if s.kind == Kind::Agy {
                // `agy` rejects positional prompts.
                words.push("--prompt-interactive".into());
            }
            words.push(msg.to_string());
        }
        let line = words
            .iter()
            .map(|w| shell_word(w, false))
            .collect::<Vec<_>>()
            .join(" ");
        let agent = format!("crew.agent={}", s.name);
        let kind = format!("crew.kind={}", s.kind.as_str());
        self.call(&["label", s.pane, &agent, &kind]).map_err(fail)?;
        self.call(&["send", s.pane, &line]).map_err(fail)?;
        let timeout = READY_WAIT.as_secs().to_string();
        let waited = self
            .call(&["wait", s.pane, "--timeout", &timeout])
            .map_err(fail)?;
        Ok(match waited["status"].as_str() {
            Some("idle" | "done" | "working") => Started::Ready,
            // Blocked at a dialog, or silent (Codex shows its trust dialog without a status).
            _ => Started::Waiting,
        })
    }

    fn run_in_pane(&self, pane: &str, command: &str) -> Result<(), String> {
        self.call(&["send", pane, command])
            .map(drop)
            .map_err(|e| format!("could not type in {pane}: {e}"))
    }

    fn close_tab(&self, tab: &str) -> Result<(), String> {
        let terminals = self.labeled("herdr.tab", tab).map_err(|e| e.to_string())?;
        for t in &terminals {
            if let Some(address) = t["address"].as_str() {
                self.call(&["kill", address]).map_err(|e| e.to_string())?;
                let _ = self.call(&["rm", address]);
            }
        }
        Ok(())
    }

    fn agent_state(&self, pane: &str) -> Result<AgentState, String> {
        let t = self.terminal(pane).map_err(|e| e.to_string())?;
        Ok(match t["status"]["state"].as_str() {
            Some("idle" | "done" | "working") => AgentState::Available,
            Some("blocked") => AgentState::Blocked,
            Some(other) => AgentState::Other(other.into()),
            None => AgentState::Other("unknown".into()),
        })
    }

    fn prompt(&self, pane: &str, text: &str) -> Result<(), PromptError> {
        self.call(&["send", pane, text]).map(drop).map_err(|e| {
            if e.kind.contains("block") {
                PromptError::Blocked
            } else {
                PromptError::Failed(e.to_string())
            }
        })
    }

    fn will_attach(&self, _attach: bool) -> bool {
        false
    }

    fn focus_and_attach(&self, workspace: &str, _root: &Path, _attach: bool) -> Result<(), String> {
        self.app_do("space.select", &[("id", workspace)]);
        Ok(())
    }

    fn open_hint(&self, label: &str, _root: &Path) -> String {
        format!("open the Herdr app: the crew is the space \"{label}\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn terminal(address: &str, tab: &str, labels: Value, fg_pid: u64, status: Value) -> Value {
        let mut l = json!({"herdr.space": "s", "herdr.tab": tab});
        l.as_object_mut()
            .unwrap()
            .extend(labels.as_object().unwrap().clone());
        json!({
            "address": address, "pid": 10, "cwd": "/repo", "labels": l,
            "foreground": {"pid": fg_pid}, "status": status, "created_ms": 1
        })
    }

    #[test]
    fn state_maps_tabs_panes_and_agents() {
        let terminals = [
            terminal(
                "t1",
                "s-a",
                json!({"crew.tab": "lead", "crew.agent": "lead", "crew.kind": "claude"}),
                11,
                json!({"app": "claude-code", "state": "idle"}),
            ),
            terminal("t2", "s-b", json!({"crew.tab": "board"}), 12, Value::Null),
            // The agent exited: its shell is back in front.
            terminal(
                "t3",
                "s-c",
                json!({"crew.tab": "dev", "crew.agent": "dev"}),
                10,
                Value::Null,
            ),
        ];
        let s = state_of("s", &terminals);
        let w = s.workspace.unwrap();
        assert_eq!(
            w.tabs.iter().map(|t| t.label.as_str()).collect::<Vec<_>>(),
            ["lead", "board", "dev"]
        );
        assert_eq!(w.panes.len(), 3);
        assert_eq!(s.agents.len(), 1, "only lead runs an agent: {:?}", s.agents);
        assert_eq!(s.agents[0].kind.as_deref(), Some("claude"));
        assert_eq!(s.agents[0].name.as_deref(), Some("lead"));
        assert_eq!(s.agents[0].tab_id, "s-a");
    }

    #[test]
    fn no_terminals_means_no_workspace() {
        assert_eq!(state_of("s", &[]), HerdrState::default());
    }

    #[test]
    fn an_unlabeled_agent_still_occupies_its_tab() {
        let t = terminal(
            "t1",
            "s-a",
            json!({"crew.tab": "lead"}),
            11,
            json!({"app": "antigravity", "state": "done"}),
        );
        let s = state_of("s", &[t]);
        assert_eq!(s.agents[0].kind.as_deref(), Some("agy"));
        assert_eq!(s.agents[0].name, None);
    }

    #[test]
    fn host_variables_stay_out_of_the_core() {
        for name in [
            "HERDR_PANE_ID",
            "HERDR_ENV",
            "CLAUDECODE",
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_SESSION_ID",
            "CODEX_THREAD_ID",
            "HC_TERMINAL_ID",
            "HC_SCOPE",
        ] {
            assert!(host_variable(name), "{name}");
        }
        for name in ["HC_SESSION", "HC_SOCKET", "HOME", "PATH", "CODEX_HOME"] {
            assert!(!host_variable(name), "{name}");
        }
    }

    #[test]
    fn space_ids_are_stable_and_readable() {
        let a = space_id(Path::new("/nonexistent/project"));
        assert_eq!(a, space_id(Path::new("/nonexistent/project")));
        assert_ne!(a, space_id(Path::new("/nonexistent/other")));
        assert!(a.starts_with("crew-") && a.len() == 21, "{a}");
    }
}
