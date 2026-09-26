//! Adapter over the herdr CLI: queries, starting the server and running the steps (design §4.2
//! and §4.3).

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::IsTerminal;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::config::Config;
use crate::git;
use crate::plan::{Foreground, HerdrState, PaneRef, Plan, Step, WorkspaceRef, find_workspace};

/// A failed call: herdr's `.error.code` and `.error.message`, or the failure to run it.
#[derive(Debug, Clone)]
pub struct CallError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

pub struct Herdr {
    bin: OsString,
    /// Runs as a herdr action (`HERDR_PLUGIN_ID`): no visible terminal.
    pub action: bool,
}

impl Herdr {
    pub fn from_env() -> Herdr {
        Herdr {
            bin: std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into()),
            action: std::env::var_os("HERDR_PLUGIN_ID").is_some(),
        }
    }

    /// Runs `herdr <args>` and returns `.result` (`null` when a command prints nothing).
    pub fn call<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<Value, CallError> {
        let out = Command::new(&self.bin)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| CallError {
                code: "no_herdr".into(),
                message: format!("could not run herdr: {e}"),
            })?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let parsed: Option<Value> = serde_json::from_str(stdout.trim())
            .ok()
            .or_else(|| serde_json::from_str(stderr.trim()).ok());
        match parsed {
            Some(v) if v.get("error").is_some() => Err(CallError {
                code: v["error"]["code"].as_str().unwrap_or("error").to_string(),
                message: v["error"]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            }),
            Some(v) if v.get("result").is_some() => Ok(v["result"].clone()),
            _ if out.status.success() => Ok(Value::Null),
            _ => Err(CallError {
                code: "output".into(),
                message: format!("{} {}", stdout.trim(), stderr.trim())
                    .trim()
                    .to_string(),
            }),
        }
    }

    /// `workspace list`; when no server answers and this is not an action, starts one in the
    /// root without the Claude Code variables and waits up to 10 s (design §4.2 step 1).
    pub fn ensure_server(&self, root: &Path, out: &mut Output) -> Result<Value, String> {
        if let Ok(v) = self.call(&["workspace", "list"]) {
            return Ok(v);
        }
        if self.action {
            return Err("the herdr server does not answer".into());
        }
        out.step(&format!(
            "no herdr server; starting one in {}",
            root.display()
        ));
        let mut cmd = Command::new(&self.bin);
        cmd.arg("server")
            .current_dir(root)
            .env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_CHILD_SESSION")
            .env_remove("CLAUDE_CODE_ENTRYPOINT")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        detach(&mut cmd);
        cmd.spawn()
            .map_err(|e| format!("could not start herdr server: {e}"))?;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            sleep(Duration::from_millis(250));
            if let Ok(v) = self.call(&["workspace", "list"]) {
                return Ok(v);
            }
        }
        Err(
            "the herdr server did not answer within 10 s; open it with `herdr` and try again"
                .into(),
        )
    }

    /// The state `plan` needs, from the `.result` of `workspace list`.
    pub fn state(&self, workspaces: &Value, label: &str) -> Result<HerdrState, String> {
        let err = |e: CallError| e.to_string();
        let agents = self.call(&["agent", "list"]).map_err(err)?;
        match find_workspace(workspaces, label)? {
            None => HerdrState::from_results(None, &agents),
            Some(id) => {
                let tabs = self
                    .call(&["tab", "list", "--workspace", &id])
                    .map_err(err)?;
                let panes = self
                    .call(&["pane", "list", "--workspace", &id])
                    .map_err(err)?;
                HerdrState::from_results(Some((id, &tabs, &panes)), &agents)
            }
        }
    }

    pub fn foreground(&self, pane: &str) -> Foreground {
        match self.call(&["pane", "process-info", "--pane", pane]) {
            Ok(v) => Foreground::from_result(&v),
            Err(e) => Foreground::Unknown(e.to_string()),
        }
    }

    /// Shows a herdr notification when running as an action (no visible terminal).
    pub fn notify(&self, message: &str) {
        if self.action {
            let _ = self.call(&["notification", "show", "herdr-crew", "--body", message]);
        }
    }

    /// `workspace focus` and, from a plain terminal, opens the herdr UI (step 10).
    pub fn focus_and_attach(
        &self,
        workspace: &str,
        root: &Path,
        attach: bool,
    ) -> Result<(), String> {
        let _ = self.call(&["workspace", "focus", workspace]);
        let inside = std::env::var("HERDR_ENV").is_ok_and(|v| v == "1");
        if inside
            || !attach
            || self.action
            || !std::io::stdin().is_terminal()
            || !std::io::stdout().is_terminal()
        {
            return Ok(());
        }
        let mut cmd = Command::new(&self.bin);
        cmd.current_dir(root);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            Err(format!("could not open herdr: {}", cmd.exec()))
        }
        #[cfg(not(unix))]
        {
            cmd.status()
                .map(|_| ())
                .map_err(|e| format!("could not open herdr: {e}"))
        }
    }
}

#[cfg(unix)]
fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(windows)]
fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(not(any(unix, windows)))]
fn detach(_: &mut Command) {}

/// Step output: stdout for what was done, stderr for warnings and errors (design §4.3).
#[derive(Default)]
pub struct Output;

impl Output {
    pub fn step(&mut self, msg: &str) {
        println!("herdr-crew: {msg}");
    }

    pub fn warn(&mut self, herdr: &Herdr, msg: &str) {
        eprintln!("herdr-crew: {msg}");
        herdr.notify(msg);
    }
}

/// Runs the steps in order, resolves the references with the real answers and returns the
/// project's workspace. An error stops the plan (`up` is re-entrant).
pub fn execute(
    herdr: &Herdr,
    c: &Config,
    state: &HerdrState,
    plan: &Plan,
    out: &mut Output,
) -> Result<Option<String>, String> {
    let mut workspace = state.workspace.as_ref().map(|w| w.id.clone());
    let mut initial: Option<(String, String)> = None; // initial tab and pane of a new workspace
    let mut panes: HashMap<String, String> = HashMap::new(); // label of a created tab -> pane
    let pane_of = |p: &PaneRef, panes: &HashMap<String, String>| match p {
        PaneRef::Existing(id) => Ok(id.clone()),
        PaneRef::OfTab(label) => panes
            .get(label)
            .cloned()
            .ok_or_else(|| format!("no pane for \"{label}\"")),
    };
    for step in &plan.steps {
        match step {
            Step::CreateWorkspace { label, cwd } => {
                let cwd = cwd.to_string_lossy();
                let r = herdr
                    .call(&[
                        "workspace",
                        "create",
                        "--cwd",
                        &cwd,
                        "--label",
                        label,
                        "--no-focus",
                    ])
                    .map_err(|e| format!("could not create workspace \"{label}\": {e}"))?;
                let id = str_at(&r, &["workspace", "workspace_id"])?;
                initial = Some((
                    str_at(&r, &["tab", "tab_id"])?,
                    str_at(&r, &["root_pane", "pane_id"])?,
                ));
                out.step(&format!("workspace \"{label}\" created ({id})"));
                workspace = Some(id);
            }
            Step::RenameTab { label } => {
                let (tab, pane) = initial.clone().ok_or("there is no initial tab to rename")?;
                herdr
                    .call(&["tab", "rename", &tab, label])
                    .map_err(|e| format!("could not rename {tab}: {e}"))?;
                panes.insert(label.clone(), pane);
            }
            Step::CreateWorktree {
                name,
                path,
                remote,
                branch,
            } => {
                if let Err(e) = git::fetch(&c.root, remote, branch) {
                    out.warn(
                        herdr,
                        &format!("could not fetch {remote}/{branch}; using the local ref ({e})"),
                    );
                }
                git::worktree_add(&c.root, path, &format!("{remote}/{branch}"))?;
                out.step(&format!("worktree of {name} in {}", path.display()));
            }
            Step::CreateTab {
                workspace: wref,
                label,
                cwd,
            } => {
                let ws = match wref {
                    WorkspaceRef::Existing(id) => id.clone(),
                    WorkspaceRef::Created => {
                        workspace.clone().ok_or("the workspace was not created")?
                    }
                };
                let cwd = cwd.to_string_lossy();
                let r = herdr
                    .call(&[
                        "tab",
                        "create",
                        "--workspace",
                        &ws,
                        "--cwd",
                        &cwd,
                        "--label",
                        label,
                        "--no-focus",
                    ])
                    .map_err(|e| format!("could not create tab \"{label}\": {e}"))?;
                panes.insert(label.clone(), str_at(&r, &["root_pane", "pane_id"])?);
            }
            Step::WritePrompt { path, content, .. } => write_file(path, content, false)?,
            Step::StartAgent { name, pane, prompt } => {
                let pane = pane_of(pane, &panes)?;
                start_agent(herdr, name, &pane, prompt, out)?;
            }
            Step::RunInPane {
                pane,
                command,
                new_tab,
            } => {
                let pane = pane_of(pane, &panes)?;
                if *new_tab {
                    sleep(Duration::from_secs(1)); // let the new shell reach its prompt
                }
                herdr
                    .call(&["pane", "run", &pane, command])
                    .map_err(|e| format!("could not type in {pane}: {e}"))?;
                out.step(&format!("typed in {pane}: {command}"));
            }
            Step::WriteSchema { path, content } => write_file(path, content, true)?,
            Step::SeedStatus { path, content } => {
                write_file(path, content, true)?;
                out.step(&format!("board seeded in {}", path.display()));
            }
            Step::Warn(msg) => out.warn(herdr, msg),
        }
    }
    Ok(workspace)
}

fn start_agent(
    herdr: &Herdr,
    name: &str,
    pane: &str,
    prompt: &Path,
    out: &mut Output,
) -> Result<(), String> {
    let prompt = prompt.to_string_lossy();
    let args = [
        "agent",
        "start",
        name,
        "--kind",
        "claude",
        "--pane",
        pane,
        "--timeout",
        "60000",
        "--",
        "-n",
        name,
        "--append-system-prompt-file",
        &prompt,
    ];
    let start = Instant::now();
    loop {
        match herdr.call(&args) {
            Ok(_) => {
                out.step(&format!("{name} in {pane}"));
                return Ok(());
            }
            // The pane's shell has not reached its prompt yet.
            Err(e) if e.code == "agent_pane_busy" && start.elapsed() < Duration::from_secs(30) => {
                sleep(Duration::from_secs(1));
            }
            // Claude runs but waits for an answer (e.g. trusting the folder).
            Err(e) if e.code == "agent_not_ready" => {
                out.warn(
                    herdr,
                    &format!(
                        "{name} in {pane} is waiting for an answer at startup; check that tab"
                    ),
                );
                return Ok(());
            }
            Err(e) => return Err(format!("could not start {name} in {pane}: {e}")),
        }
    }
}

fn str_at(v: &Value, path: &[&str]) -> Result<String, String> {
    path.iter()
        .try_fold(v, |v, k| v.get(k))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("herdr response without {}", path.join(".")))
}

/// Writes a file, creating its directory; `atomic` writes to `.tmp` and renames.
pub fn write_file(path: &Path, content: &str, atomic: bool) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("could not write {}: {e}", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(fail)?;
    }
    if atomic {
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        std::fs::write(&tmp, content).map_err(fail)?;
        std::fs::rename(&tmp, path).map_err(fail)
    } else {
        std::fs::write(path, content).map_err(fail)
    }
}
