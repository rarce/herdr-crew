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

use crate::agent::{AgentOptions, Kind};
use crate::config::Config;
use crate::plan::{Foreground, HerdrState, PaneRef, Plan, Step, WorkspaceRef, find_workspace};
use crate::{codex, files, git};

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
    /// Warnings and errors go only to the plugin log, never to a notification (`startup`).
    pub silent: bool,
}

impl Herdr {
    pub fn from_env() -> Herdr {
        Herdr {
            bin: std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into()),
            action: std::env::var_os("HERDR_PLUGIN_ID").is_some(),
            silent: false,
        }
    }

    pub fn require_native_version(&self) -> Result<(), String> {
        crate::process::version(
            self.bin
                .to_str()
                .ok_or("Herdr executable path must be UTF-8")?,
            (0, 9, 3),
        )
    }

    /// Codex and pi roles rely on herdr 0.9.3's native integrations, in both the executable and
    /// the running server.
    pub fn require_native_server(&self, kind: &str) -> Result<(), String> {
        self.require_native_version()?;
        self.require_server_version(kind)
    }

    /// The running server's half of [`Herdr::require_native_server`].
    pub fn require_server_version(&self, kind: &str) -> Result<(), String> {
        let status = self
            .call(&["status", "server", "--json"])
            .map_err(|e| e.to_string())?;
        if status["version"]
            .as_str()
            .is_none_or(|v| !codex::supported_herdr(v))
        {
            return Err(format!("{kind} crews require Herdr server 0.9.3 or newer"));
        }
        Ok(())
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
            Some(v) if out.status.success() => Ok(v),
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
            .env_remove("CODEX_THREAD_ID")
            .env_remove("CREW_CODEX_BINDING")
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
    pub fn state(&self, workspaces: &Value, c: &Config) -> Result<HerdrState, String> {
        let err = |e: CallError| e.to_string();
        let agents = self.call(&["agent", "list"]).map_err(err)?;
        match find_workspace(workspaces, &c.label)? {
            None => HerdrState::from_results(None, &agents),
            Some(id) => {
                let tabs = self
                    .call(&["tab", "list", "--workspace", &id])
                    .map_err(err)?;
                let panes = self
                    .call(&["pane", "list", "--workspace", &id])
                    .map_err(err)?;
                let state = HerdrState::from_results(Some((id, &tabs, &panes)), &agents)?;
                verify_workspace(c, &state)?;
                Ok(state)
            }
        }
    }

    pub fn foreground(&self, pane: &str) -> Foreground {
        match self.call(&["pane", "process-info", "--pane", pane]) {
            Ok(v) => Foreground::from_result(&v),
            Err(e) => Foreground::Unknown(e.to_string()),
        }
    }

    /// Whether warnings and errors also become a herdr notification: only in an action, which
    /// has no visible terminal, and never in `startup`, whose output goes to the plugin log.
    pub fn notifies(&self) -> bool {
        self.action && !self.silent
    }

    /// Shows a herdr notification when `notifies`.
    pub fn notify(&self, message: &str) {
        if self.notifies() {
            self.show(message);
        }
    }

    /// Shows a herdr notification.
    pub fn show(&self, message: &str) {
        let _ = self.call(&["notification", "show", "herdr-crew", "--body", message]);
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

/// The first pane anchors the workspace's project, just as in startup discovery. Known crew
/// tabs must also belong to that repository; unrelated tabs are left alone. Use Git roots,
/// rather than string prefixes, to distinguish nested repositories and linked worktrees.
fn verify_workspace(c: &Config, state: &HerdrState) -> Result<(), String> {
    let Some(workspace) = &state.workspace else {
        return Ok(());
    };
    let root = c.root.canonicalize().map_err(|e| e.to_string())?;
    let verify = |pane: &crate::plan::Pane| -> Result<(), String> {
        let verified = pane
            .cwd
            .as_deref()
            .is_some_and(|cwd| git::verify_checkout(&root, cwd).is_ok());
        if !verified {
            return Err(format!(
                "workspace {:?} ({}) cannot be verified for {}: pane {} is in {}; rename the workspace or restore its project directory",
                c.label,
                workspace.id,
                root.display(),
                pane.id,
                pane.cwd.as_deref().map_or_else(
                    || "an unknown directory".into(),
                    |p| p.display().to_string()
                )
            ));
        }
        Ok(())
    };
    verify(workspace.panes.first().ok_or_else(|| {
        format!(
            "workspace {:?} has no panes; cannot verify its project",
            c.label
        )
    })?)?;
    for pane in &workspace.panes {
        if workspace
            .tabs
            .iter()
            .any(|tab| tab.id == pane.tab_id && c.is_known_tab(&tab.label))
        {
            verify(pane)?;
        }
    }
    Ok(())
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
    files::validate_config(c)?;
    verify_workspace(c, state)?;
    // Check every planned output, including extra prompts, before the first mutation.
    for step in &plan.steps {
        match step {
            Step::WritePrompt { path, .. }
            | Step::WriteSchema { path, .. }
            | Step::SeedStatus { path, .. } => {
                files::validate(&c.root, path, files::Kind::File)?;
            }
            Step::CreateWorktree {
                path,
                remote,
                branch,
                ..
            } => {
                files::validate(&c.root, path, files::Kind::Directory)?;
                git::validate_base(&c.root, &format!("{remote}/{branch}"))
                    .map_err(|e| format!("worktrees.base \"{remote}/{branch}\": {e}"))?;
            }
            Step::CreateTab { label, cwd, .. }
                if c.session_role(label).is_some_and(|r| r.worktree) =>
            {
                files::validate(&c.root, cwd, files::Kind::Directory)?;
                if cwd.exists() {
                    git::validate_worktree(&c.root, cwd)?;
                }
            }
            _ => {}
        }
    }
    // The prompts this plan writes, as the agents will read them.
    let prompts: HashMap<&str, &str> = plan
        .steps
        .iter()
        .filter_map(|step| match step {
            Step::WritePrompt { name, content, .. } => Some((name.as_str(), content.as_str())),
            _ => None,
        })
        .collect();
    let prompt_of = |name: &str| {
        prompts
            .get(name)
            .copied()
            .ok_or_else(|| format!("the plan writes no prompt for {name}"))
    };
    let mut pi_roles = Vec::new();
    let mut codex_roles = Vec::new();
    for step in &plan.steps {
        match step {
            Step::StartAgent {
                name,
                agent: AgentOptions::Pi(_),
                ..
            } => pi_roles.push(name.as_str()),
            Step::StartAgent {
                name,
                agent: AgentOptions::Codex(options),
                ..
            } => codex_roles.push((name.as_str(), options)),
            _ => {}
        }
    }
    if !pi_roles.is_empty() {
        crate::pi::preflight()?;
        crate::pi::require_version("pi")?;
    }
    if !codex_roles.is_empty() {
        codex::validate_runtime(&c.root)?;
    }
    // Every Codex and pi role needs the same herdr.
    let native = if !codex_roles.is_empty() {
        Some("Codex")
    } else if !pi_roles.is_empty() {
        Some("pi")
    } else {
        None
    };
    if let Some(kind) = native {
        herdr.require_native_server(kind)?;
    }
    for name in pi_roles {
        if let Some(problem) = crate::pi::prompt_problem(name, prompt_of(name)?) {
            return Err(problem);
        }
    }
    for (name, options) in codex_roles {
        codex::context_prompt(c, name, prompt_of(name)?)?;
        let cwd = if c.session_role(name).unwrap().worktree {
            c.worktree_path(name).unwrap()
        } else {
            c.root.clone()
        };
        codex::integration::preflight(&cwd, options, &c.root)?;
    }
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
                let mut args: Vec<String> = [
                    "workspace",
                    "create",
                    "--cwd",
                    &cwd,
                    "--label",
                    label,
                    "--no-focus",
                ]
                .into_iter()
                .map(str::to_string)
                .collect();
                if c.roles.first().is_some_and(|role| role.kind == Kind::Codex) {
                    args.extend(codex::pane_environment()?);
                }
                let r = herdr
                    .call(&args)
                    .map_err(|e| format!("could not create workspace \"{label}\": {e}"))?;
                let id = str_at(&r, &["workspace", "workspace_id"])?;
                initial = Some((
                    str_at(&r, &["tab", "tab_id"])?,
                    str_at(&r, &["root_pane", "pane_id"])?,
                ));
                out.step(&format!("workspace \"{label}\" created ({id})"));
                workspace = Some(id);
            }
            Step::AdoptWorkspace { initial: i, label } => {
                if i.label != *label {
                    herdr
                        .call(&["workspace", "rename", &i.workspace, label])
                        .map_err(|e| format!("could not rename {}: {e}", i.workspace))?;
                }
                out.step(&format!("workspace {} adopted as \"{label}\"", i.workspace));
                initial = Some((i.tab.clone(), i.pane.clone()));
                workspace = Some(i.workspace.clone());
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
                files::validate(&c.root, path, files::Kind::Directory)?;
                if let Err(e) = git::fetch(&c.root, remote, branch) {
                    out.warn(
                        herdr,
                        &format!("could not fetch {remote}/{branch}; using the local ref ({e})"),
                    );
                }
                git::worktree_add(&c.root, path, &format!("{remote}/{branch}"))?;
                git::validate_worktree(&c.root, path)?;
                out.step(&format!("worktree of {name} in {}", path.display()));
            }
            Step::CreateTab {
                workspace: wref,
                label,
                cwd,
            } => {
                if c.session_role(label).is_some_and(|r| r.worktree) {
                    git::validate_worktree(&c.root, cwd)?;
                }
                let ws = match wref {
                    WorkspaceRef::Existing(id) => id.clone(),
                    WorkspaceRef::Created => {
                        workspace.clone().ok_or("the workspace was not created")?
                    }
                };
                let cwd = cwd.to_string_lossy();
                let mut args: Vec<String> = [
                    "tab",
                    "create",
                    "--workspace",
                    &ws,
                    "--cwd",
                    &cwd,
                    "--label",
                    label,
                    "--no-focus",
                ]
                .into_iter()
                .map(str::to_string)
                .collect();
                if c.session_role(label)
                    .is_some_and(|role| role.kind == Kind::Codex)
                {
                    args.extend(codex::pane_environment()?);
                }
                let r = herdr
                    .call(&args)
                    .map_err(|e| format!("could not create tab \"{label}\": {e}"))?;
                panes.insert(label.clone(), str_at(&r, &["root_pane", "pane_id"])?);
            }
            Step::WritePrompt { path, content, .. } => files::write(&c.root, path, content)?,
            Step::StartAgent {
                name,
                pane,
                prompt,
                message,
                agent,
            } => {
                let pane = pane_of(pane, &panes)?;
                if let AgentOptions::Codex(options) = agent {
                    codex::prepare(herdr, c, name, &pane, prompt_of(name)?, options)?;
                }
                let native = match agent {
                    AgentOptions::Claude => vec![
                        "-n".into(),
                        name.clone(),
                        "--append-system-prompt-file".into(),
                        prompt.display().to_string(),
                    ],
                    AgentOptions::Codex(options) => {
                        let mut effective = options.clone();
                        effective.resolve_dirs(&c.root)?;
                        let mut args = effective.args();
                        args.extend(["-c".into(), "features.hooks=true".into()]);
                        args
                    }
                    // The crew's pi extension reads these flags and keeps the prompt in the session.
                    AgentOptions::Pi(pi) => {
                        let mut args = vec![
                            "--herdr-crew-role".into(),
                            name.clone(),
                            "--herdr-crew-prompt".into(),
                            prompt.display().to_string(),
                        ];
                        args.extend(pi.args());
                        args
                    }
                };
                start_agent(
                    herdr,
                    name,
                    &pane,
                    agent.kind(),
                    &native,
                    message.as_deref(),
                    out,
                )?;
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
            Step::WriteSchema { path, content } => files::write(&c.root, path, content)?,
            Step::SeedStatus { path, content } => {
                files::write(&c.root, path, content)?;
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
    kind: Kind,
    native: &[String],
    message: Option<&str>,
    out: &mut Output,
) -> Result<(), String> {
    let mut args: Vec<String> = [
        "agent",
        "start",
        name,
        "--kind",
        kind.as_str(),
        "--pane",
        pane,
        "--timeout",
        "60000",
        "--",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    args.extend_from_slice(native);
    args.extend(message.map(str::to_string));
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
            // The agent runs but waits for an answer (e.g. trusting the folder).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_warnings_go_only_to_the_log() {
        let h = |action, silent| Herdr {
            bin: "herdr".into(),
            action,
            silent,
        };
        assert!(h(true, false).notifies(), "an action notifies");
        assert!(!h(true, true).notifies(), "startup never notifies");
        assert!(!h(false, false).notifies(), "a terminal prints");
    }
}
