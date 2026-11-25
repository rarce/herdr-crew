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
/// `HC_SESSION` and `HC_SOCKET` stay: they choose the core; so do the app's `HERDR_APP_*`
/// (`HERDR_APP_HOME` chooses the window `herdr-app do` talks to).
fn host_variable(name: &str) -> bool {
    (name.starts_with("HERDR_") && !name.starts_with("HERDR_APP_"))
        || name.starts_with("CLAUDE_CODE_")
        || name == "CLAUDECODE"
        || name == "CODEX_THREAD_ID"
        || name == "CREW_CODEX_BINDING"
        || (name.starts_with("HC_") && name != "HC_SESSION" && name != "HC_SOCKET")
}

/// How long `start_agent` waits for a new shell to reach its prompt.
const SHELL_WAIT: Duration = Duration::from_secs(30);

/// How long `foreground` waits for a recovering terminal to report its foreground.
const RECOVERY_WAIT: Duration = Duration::from_secs(10);

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

    /// Runs `hc --json <args>` and returns its JSON answer. `--json` goes first: after `run`'s
    /// `--` everything is the program's command line.
    fn call(&self, args: &[&str]) -> Result<Value, HcError> {
        let out = self
            .command(&self.bin)
            .arg("--json")
            .args(args)
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

    /// The open window's spaces and tabs (`herdr-app ls`), or `None` with no window.
    fn app_ls(&self) -> Option<Value> {
        let mut cmd = self.command(&self.app);
        cmd.arg("ls");
        let stdout = bounded(&mut cmd, APP_WAIT)?;
        serde_json::from_slice(&stdout).ok()
    }

    /// Best effort `herdr-app do <command> --key value…`: the window may be closed.
    fn app_do(&self, command: &str, args: &[(&str, &str)]) {
        let mut cmd = self.command(&self.app);
        cmd.args(["do", command]);
        for (k, v) in args {
            cmd.arg(format!("--{k}")).arg(v);
        }
        let _ = bounded(&mut cmd, APP_WAIT);
    }

    fn terminal(&self, address: &str) -> Result<Value, HcError> {
        self.call(&["get", address])
    }

    /// A terminal's facts (`hc get`), for the agents' hooks.
    pub fn get(&self, address: &str) -> Result<Value, String> {
        self.terminal(address)
            .map_err(|e| format!("could not read terminal {address}: {e}"))
    }

    /// The terminal's crew identity, its `crew.core` label: random, set when crew makes the
    /// terminal, and kept by core across restarts. With `create`, a terminal made by an older
    /// crew gets one.
    pub fn core_id(&self, address: &str, create: bool) -> Result<Option<String>, String> {
        let t = self.get(address)?;
        if let Some(id) = label(&t, "crew.core").filter(|v| !v.is_empty()) {
            return Ok(Some(id.to_string()));
        }
        if !create {
            return Ok(None);
        }
        let id = files::random_hex(8).map_err(|e| e.to_string())?;
        self.set_label(address, "crew.core", &id)?;
        Ok(Some(id))
    }

    /// Sets one label on a terminal.
    pub fn set_label(&self, address: &str, key: &str, value: &str) -> Result<(), String> {
        self.call(&["label", address, &format!("{key}={value}")])
            .map(drop)
            .map_err(|e| e.to_string())
    }

    /// The addresses of the live terminals carrying `key=value`.
    pub fn live_with(&self, key: &str, value: &str) -> Result<Vec<String>, String> {
        Ok(self
            .labeled(key, value)
            .map_err(|e| e.to_string())?
            .iter()
            .filter_map(|t| t["address"].as_str().map(str::to_string))
            .collect())
    }

    /// The live terminals carrying `key=value`, with their full facts.
    fn labeled(&self, key: &str, value: &str) -> Result<Vec<Value>, HcError> {
        let label = format!("{key}={value}");
        let list = self.call(&["ls", "--label", &label])?;
        let mut terminals = Vec::new();
        for t in list["terminals"].as_array().into_iter().flatten() {
            if let Some(address) = t["address"].as_str() {
                match self.terminal(address) {
                    Ok(t) if t["state"] == "live" => terminals.push(t),
                    // It finished or was removed since `ls`.
                    Ok(_) => {}
                    Err(e) if e.kind == "not_found" => {}
                    Err(e) => return Err(e),
                }
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
        let id = files::random_hex(8).map_err(|e| e.to_string())?;
        let mut labels = vec![
            format!("herdr.space={space}"),
            format!("herdr.tab={tab}"),
            format!("crew.core={id}"),
        ];
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

/// How long a `herdr-app` call may take: `up` holds the project lock meanwhile.
const APP_WAIT: Duration = Duration::from_secs(5);

/// Runs `cmd` for at most `limit` and returns its stdout when it succeeds; kills it otherwise.
fn bounded(cmd: &mut Command, limit: Duration) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut pipe = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = pipe.read_to_end(&mut out);
        out
    });
    let since = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if since.elapsed() < limit => sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let out = reader.join().ok()?;
    status.success().then_some(out)
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

/// A random version 4 UUID, as Claude's and pi's `--session-id` take.
fn new_uuid() -> std::io::Result<String> {
    let hex = files::random_hex(16)?;
    let mut b: Vec<char> = hex.chars().collect();
    b[12] = '4';
    // The variant: the two high bits of the 17th digit are 10.
    let low = b[16].to_digit(16).unwrap_or(0) & 3;
    b[16] = ['8', '9', 'a', 'b'][low as usize];
    let s: String = b.into_iter().collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &s[0..8],
        &s[8..12],
        &s[12..16],
        &s[16..20],
        &s[20..32]
    ))
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
            agent_session: label(t, "crew.session")
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            session_agent: label(t, "crew.kind").map(str::to_string),
            session_kind: None,
            session_source: None,
            cwd: t["cwd"].as_str().map(Into::into),
        });
        // An agent runs while something other than the shell is in front and either the crew
        // started it or it reports a status. Right after a core restart a kept terminal has no
        // foreground yet: it is coming back as a shell, so it has no agent.
        let app = t["status"]["app"].as_str();
        let crew_agent = label(t, "crew.agent");
        let in_front = t["foreground"]["pid"].is_u64() && !shell_in_front(t);
        if in_front && (crew_agent.is_some() || app.is_some()) {
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

/// The window commands that give the crew's space its label and its tabs their names and
/// order, from `herdr-app ls` and the space's terminals. Tabs that are not the crew's keep
/// their place after the crew's.
fn tidy_commands(
    c: &Config,
    space: &str,
    ls: &Value,
    terminals: &[Value],
) -> Vec<(&'static str, Vec<(&'static str, String)>)> {
    let mut commands = Vec::new();
    let spaces = ls["spaces"].as_array().or_else(|| ls.as_array());
    let Some(shown) = spaces.into_iter().flatten().find(|s| s["id"] == space) else {
        return commands;
    };
    // `herdr-app ls` has no `named` (the mods' `spaces()` has): a space nobody named is called
    // after its repo, the crew's folder.
    let folder = c.root.file_name().map(|n| n.to_string_lossy().to_string());
    let space_named = shown["named"].as_bool().unwrap_or_else(|| {
        let name = shown["name"].as_str().unwrap_or_default();
        !name.is_empty() && Some(name) != folder.as_deref()
    });
    if !space_named && shown["name"] != c.label.as_str() {
        commands.push((
            "space.rename",
            vec![("id", space.to_string()), ("name", c.label.clone())],
        ));
    }
    // The crew's label of each tab, from its terminals.
    let crew_label = |tab: &str| {
        terminals
            .iter()
            .filter(|t| label(t, "herdr.tab") == Some(tab))
            .find_map(|t| label(t, "crew.tab"))
    };
    let order = |name: &str| {
        c.session_order(name).unwrap_or(if name == c.board.tab {
            (usize::MAX - 1, 0)
        } else {
            (usize::MAX, 0)
        })
    };
    let tabs: Vec<(&str, Option<&str>, &Value)> = shown["tabs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| Some((t["id"].as_str()?, crew_label(t["id"].as_str()?), t)))
        .collect();
    // A tab someone renamed keeps its name. Without `named`, a tab nobody named is called by
    // its place ("1", "2"…).
    for (i, (id, name, t)) in tabs.iter().enumerate() {
        let named = t["named"]
            .as_bool()
            .unwrap_or_else(|| t["name"] != (i + 1).to_string().as_str());
        if let Some(name) = name
            && !named
            && t["name"] != *name
        {
            commands.push((
                "tab.rename",
                vec![("id", id.to_string()), ("name", name.to_string())],
            ));
        }
    }
    // The crew's tabs first, in its order; a stable sort keeps the others as they are.
    let mut wanted: Vec<&(&str, Option<&str>, &Value)> = tabs.iter().collect();
    wanted.sort_by_key(|(_, name, _)| match name {
        Some(name) => (0, order(name)),
        None => (1, (0, 0)),
    });
    let current: Vec<&str> = tabs.iter().map(|(id, _, _)| *id).collect();
    let target: Vec<&str> = wanted.iter().map(|(id, _, _)| *id).collect();
    if current != target {
        for (i, id) in target.iter().enumerate() {
            commands.push((
                "tab.move",
                vec![("id", id.to_string()), ("index", (i + 1).to_string())],
            ));
        }
    }
    commands
}

/// What a launched agent did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Launch {
    /// It reported a status: whether it waits for an answer (blocked at a dialog).
    Reported(bool),
    /// It runs but reported nothing yet (Codex shows its trust dialog without a status).
    Silent,
    /// The shell is back in front: the agent exited.
    Exited,
}

/// How long a typed command may leave the shell in front before crew decides it exited.
const EXIT_GRACE: Duration = Duration::from_secs(5);

/// The shell line that starts `s`: with `session` and `resume`, `claude --resume <id>` or
/// `agy --conversation <id>`; otherwise the agent with `--session-id <id>` (pi resumes that way
/// too) and the message.
fn launch_line(s: &AgentStart, session: Option<&str>, resume: bool) -> String {
    if resume && s.kind == Kind::Codex {
        // The crew's resume checks the role's binding and execs `codex resume` with the
        // recorded conversation, the role's options and its Codex home.
        return ["herdr-crew", "codex-resume", session.unwrap_or_default()]
            .iter()
            .map(|w| shell_word(w, false))
            .collect::<Vec<_>>()
            .join(" ");
    }
    let mut words = vec![s.kind.as_str().to_string()];
    match session {
        Some(id) if resume && s.kind == Kind::Agy => {
            words.extend(["--conversation".into(), id.to_string()])
        }
        Some(id) if resume => words.extend(["--resume".into(), id.to_string()]),
        Some(id) => words.extend(["--session-id".into(), id.to_string()]),
        None => {}
    }
    words.extend(s.args.iter().cloned());
    if let Some(msg) = s.message {
        if s.kind == Kind::Agy {
            // `agy` rejects positional prompts.
            words.push("--prompt-interactive".into());
        }
        words.push(msg.to_string());
    }
    words
        .iter()
        .map(|w| shell_word(w, false))
        .collect::<Vec<_>>()
        .join(" ")
}

fn exited(s: &AgentStart) -> String {
    format!(
        "{} exited as soon as it started in {}; see that tab",
        s.kind.as_str(),
        s.pane
    )
}

/// Whether `t` carries a status reported after the one in `before` (`status_ms` differs).
fn fresh_status<'a>(t: &'a Value, before: &Value) -> Option<&'a str> {
    let state = t["status"]["state"].as_str()?;
    (t["status_ms"] != before["status_ms"] || before["status"].is_null()).then_some(state)
}

impl Hc {
    /// Waits for the pane's shell, types `line` and waits for the agent's first status report
    /// after it: a status left by an earlier program does not count.
    fn launch(&self, s: &AgentStart, line: &str) -> Result<Launch, String> {
        let fail = |e: HcError| format!("could not start {} in {}: {e}", s.name, s.pane);
        // A new terminal's shell may still be reading its startup files.
        let since = Instant::now();
        let before = loop {
            let t = self.terminal(s.pane).map_err(fail)?;
            if shell_in_front(&t) {
                break t;
            }
            if since.elapsed() >= SHELL_WAIT {
                return Err(format!(
                    "could not start {} in {}: its shell is not in the foreground",
                    s.name, s.pane
                ));
            }
            sleep(Duration::from_millis(250));
        };
        self.call(&["send", s.pane, line]).map_err(fail)?;
        let sent = Instant::now();
        let mut seen = false;
        loop {
            let t = self.terminal(s.pane).map_err(fail)?;
            if let Some(state) = fresh_status(&t, &before) {
                return Ok(Launch::Reported(!matches!(
                    state,
                    "idle" | "done" | "working"
                )));
            }
            if shell_in_front(&t) {
                if seen || sent.elapsed() >= EXIT_GRACE {
                    return Ok(Launch::Exited);
                }
            } else {
                seen = true;
            }
            if sent.elapsed() >= READY_WAIT {
                return Ok(Launch::Silent);
            }
            sleep(Duration::from_millis(250));
        }
    }
}

impl Backend for Hc {
    fn action(&self) -> bool {
        false
    }

    fn notify(&self, _message: &str) {}

    /// `hc` starts the core when none runs, so a failure is no empty state: only a dry run
    /// (`Load::Read`) may plan as if the core were empty.
    fn load(&self, c: &Config, mode: Load) -> Result<HerdrState, LoadError> {
        let space = self.space.get_or_init(|| space_id(&c.root)).clone();
        let terminals = self
            .labeled("herdr.space", &space)
            .map_err(|e| match mode {
                Load::Start(..) => {
                    LoadError::Failed(format!("could not read the crew from herdr-core: {e}"))
                }
                Load::Read => LoadError::Unreachable {
                    denied: e.kind == "output"
                        && (e.message.contains("Operation not permitted")
                            || e.message.contains("PermissionDenied")),
                    message: e.to_string(),
                },
            })?;
        Ok(state_of(&space, &terminals))
    }

    fn foreground(&self, pane: &str) -> Foreground {
        // A kept terminal that is still recovering (just after a core restart) has no
        // foreground yet; give it a moment to come back as a shell.
        let since = Instant::now();
        let mut answer = self.terminal(pane);
        while answer
            .as_ref()
            .is_ok_and(|t| !t["foreground"]["pid"].is_u64())
            && since.elapsed() < RECOVERY_WAIT
        {
            sleep(Duration::from_millis(250));
            answer = self.terminal(pane);
        }
        match answer {
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
        // Claude and pi take a session id chosen up front, which `crew.session` records so a
        // later `up` can resume the conversation in the kept shell.
        // agy picks its own conversation id: the crew's SessionStart hook records it.
        let session = match (s.kind, s.resume) {
            // A Codex role's binding token, which its SessionStart hook records.
            (_, Some(id)) => Some(id.to_string()),
            (Kind::Claude | Kind::Pi, None) => {
                Some(new_uuid().map_err(|e| format!("could not start {}: {e}", s.name))?)
            }
            (_, None) => None,
        };
        let agent = format!("crew.agent={}", s.name);
        let kind = format!("crew.kind={}", s.kind.as_str());
        let recorded = format!("crew.session={}", session.clone().unwrap_or_default());
        self.call(&["label", s.pane, &agent, &kind, &recorded])
            .map_err(fail)?;
        // `pi --session-id` opens the session, or starts it when pi never saved one.
        let resume = s.kind != Kind::Pi && s.resume.is_some();
        let line = launch_line(s, session.as_deref(), resume);
        Ok(match self.launch(s, &line)? {
            Launch::Reported(false) => Started::Ready,
            Launch::Reported(true) | Launch::Silent => Started::Waiting,
            // `claude --resume` exits when it has no such conversation: the agent died before
            // its first message was saved. Start that conversation instead.
            Launch::Exited if resume && s.kind == Kind::Claude => {
                match self.launch(s, &launch_line(s, session.as_deref(), false))? {
                    Launch::Reported(waiting) => Started::NewConversation { waiting },
                    Launch::Silent => Started::NewConversation { waiting: true },
                    Launch::Exited => return Err(exited(s)),
                }
            }
            // agy's or Codex's resume failed (no saved conversation, or `codex-resume`
            // refused). Forget the session: retrying it would stop every later `up` here.
            Launch::Exited if s.resume.is_some() => {
                let _ = self.set_label(s.pane, "crew.session", "");
                Started::NotResumed
            }
            Launch::Exited => return Err(exited(s)),
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
            } else if e.kind == "no_hc" {
                PromptError::Rejected(e.to_string())
            } else {
                PromptError::Uncertain(e.to_string())
            }
        })
    }

    fn resumes_agents(&self) -> bool {
        true
    }

    fn codex_context(&self, pane: &str) -> Result<crate::codex::Context, String> {
        let t = self.get(pane)?;
        let label = |key: &str| {
            label(&t, key)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("terminal {pane} has no {key} label"))
        };
        let id = self
            .core_id(pane, true)?
            .ok_or_else(|| format!("terminal {pane} has no crew.core label"))?;
        Ok(crate::codex::Context::core(
            &id,
            label("herdr.space")?,
            label("herdr.tab")?,
            pane,
        ))
    }

    /// Names the crew's space and tabs and puts the tabs in the crew's order (the sessions in
    /// `roles` order, then the board), where the window shows them otherwise: tabs made while
    /// the window was closed, or made again after their terminal ended, have Herdr's names and
    /// come last.
    fn tidy(&self, c: &Config) {
        let Some(space) = self.space.get() else {
            return;
        };
        let Some(ls) = self.app_ls() else {
            return;
        };
        let Ok(terminals) = self.labeled("herdr.space", space) else {
            return;
        };
        for (command, args) in tidy_commands(c, space, &ls, &terminals) {
            let args: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
            self.app_do(command, &args);
        }
    }

    fn pane_key(&self, pane: &str) -> Result<String, String> {
        let id = self
            .core_id(pane, true)?
            .ok_or_else(|| format!("terminal {pane} has no crew.core label"))?;
        Ok(crate::backend::core_pane_key(&id))
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

    #[test]
    fn tidy_names_the_space_and_tabs_and_restores_the_crew_order() {
        let c = Config::parse(
            "version = 1\n[workspace]\nlabel = \"crew\"\n[board]\ntab = \"status\"\n\
             writer = \"lead\"\n[[roles]]\nname = \"lead\"\nprompt = \"L\"\n\
             [[roles]]\nname = \"dev\"\nprompt = \"D\"\n",
            Path::new("/repo"),
        )
        .unwrap();
        let t = |tab: &str, crew: &str| json!({"address": tab, "labels": {"herdr.space": "s", "herdr.tab": tab, "crew.tab": crew}});
        let terminals = [t("a", "lead"), t("b", "status"), t("c", "dev")];
        // dev's terminal was made again: Herdr calls it "3" and shows it after the board.
        let ls = json!({"spaces": [{"id": "s", "name": "repo", "named": false, "tabs": [
            {"id": "a", "name": "lead", "named": true},
            {"id": "x", "name": "notes", "named": true},
            {"id": "b", "name": "status", "named": true},
            {"id": "c", "name": "4", "named": false},
        ]}]});
        let commands: Vec<String> = tidy_commands(&c, "s", &ls, &terminals)
            .into_iter()
            .map(|(name, args)| {
                let args: Vec<String> = args.into_iter().map(|(_, v)| v).collect();
                format!("{name} {}", args.join(" "))
            })
            .collect();
        assert_eq!(
            commands,
            [
                "space.rename s crew",
                "tab.rename c dev",
                "tab.move a 1",
                "tab.move c 2",
                "tab.move b 3",
                "tab.move x 4",
            ]
        );
        // Named and in order: nothing to do.
        let ls = json!({"spaces": [{"id": "s", "name": "crew", "named": true, "tabs": [
            {"id": "a", "name": "lead"}, {"id": "c", "name": "dev"}, {"id": "b", "name": "status"},
        ]}]});
        assert!(tidy_commands(&c, "s", &ls, &terminals).is_empty());
        // Another space, or no window: nothing.
        assert!(tidy_commands(&c, "other", &ls, &terminals).is_empty());

        // `herdr-app ls` has no `named`: an unnamed space has the repo's name, an unnamed tab
        // its place. dev is third ("3"), so it is unnamed; "notes" was named by someone.
        let ls = json!({"spaces": [{"id": "s", "name": "repo", "tabs": [
            {"id": "a", "name": "lead"},
            {"id": "b", "name": "status"},
            {"id": "c", "name": "3"},
            {"id": "x", "name": "notes"},
        ]}]});
        let commands: Vec<String> = tidy_commands(&c, "s", &ls, &terminals)
            .into_iter()
            .map(|(name, args)| {
                let args: Vec<String> = args.into_iter().map(|(_, v)| v).collect();
                format!("{name} {}", args.join(" "))
            })
            .collect();
        assert_eq!(
            commands,
            [
                "space.rename s crew",
                "tab.rename c dev",
                "tab.move a 1",
                "tab.move c 2",
                "tab.move b 3",
                "tab.move x 4",
            ]
        );
        // A space someone named "work" keeps it.
        let ls = json!({"spaces": [{"id": "s", "name": "work", "tabs": [
            {"id": "a", "name": "lead"}, {"id": "c", "name": "dev"}, {"id": "b", "name": "status"},
        ]}]});
        assert!(tidy_commands(&c, "s", &ls, &terminals).is_empty());
    }

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
    fn a_terminal_still_recovering_has_no_agent() {
        let mut t = terminal(
            "t1",
            "s-a",
            json!({"crew.tab": "lead", "crew.agent": "lead", "crew.session": "u"}),
            11,
            Value::Null,
        );
        t.as_object_mut().unwrap().remove("foreground");
        let s = state_of("s", &[t]);
        assert!(s.agents.is_empty(), "{:?}", s.agents);
        assert_eq!(
            s.workspace.unwrap().panes[0].agent_session.as_deref(),
            Some("u")
        );
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
        for name in [
            "HC_SESSION",
            "HC_SOCKET",
            "HERDR_APP_HOME",
            "HOME",
            "PATH",
            "CODEX_HOME",
        ] {
            assert!(!host_variable(name), "{name}");
        }
    }

    #[test]
    fn session_ids_are_version_4_uuids() {
        let id = new_uuid().unwrap();
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12],
            "{id}"
        );
        assert!(parts[2].starts_with('4'), "{id}");
        assert!("89ab".contains(&parts[3][..1]), "{id}");
        assert_ne!(id, new_uuid().unwrap());
    }

    #[test]
    fn space_ids_are_stable_and_readable() {
        let a = space_id(Path::new("/nonexistent/project"));
        assert_eq!(a, space_id(Path::new("/nonexistent/project")));
        assert_ne!(a, space_id(Path::new("/nonexistent/other")));
        assert!(a.starts_with("crew-") && a.len() == 21, "{a}");
    }
}
