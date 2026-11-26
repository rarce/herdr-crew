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
use crate::backend::{
    AgentStart, AgentState, Backend, Created, Load, LoadError, PaneEnv, PromptError, Started,
};
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

impl CallError {
    pub fn socket_denied(&self) -> bool {
        self.code == "output"
            && (self.message.contains("Operation not permitted")
                || self.message.contains("PermissionDenied")
                || self.message.contains("Permission denied"))
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

    /// Whether `focus_and_attach` opens the herdr UI: only from a plain terminal outside herdr,
    /// not in an action, and not with `--no-attach`.
    pub fn will_attach(&self, attach: bool) -> bool {
        attach
            && !self.action
            && !inside_herdr()
            && std::io::stdin().is_terminal()
            && std::io::stdout().is_terminal()
    }

    /// `workspace focus` and, from a plain terminal, opens the herdr UI (step 10).
    pub fn focus_and_attach(
        &self,
        workspace: &str,
        root: &Path,
        attach: bool,
    ) -> Result<(), String> {
        let _ = self.call(&["workspace", "focus", workspace]);
        if !self.will_attach(attach) {
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

impl Backend for Herdr {
    fn action(&self) -> bool {
        self.action
    }

    fn open_hint(&self, label: &str, root: &Path) -> String {
        if inside_herdr() {
            format!("switch to the workspace \"{label}\"")
        } else {
            format!(
                "run `herdr` from {} to open the workspace \"{label}\"",
                root.display()
            )
        }
    }

    fn notify(&self, message: &str) {
        Herdr::notify(self, message);
    }

    fn load(&self, c: &Config, mode: Load) -> Result<HerdrState, LoadError> {
        let workspaces = match mode {
            Load::Start(root, out) => self.ensure_server(root, out).map_err(LoadError::Failed)?,
            Load::Read => {
                self.call(&["workspace", "list"])
                    .map_err(|e| LoadError::Unreachable {
                        denied: e.socket_denied(),
                        message: e.to_string(),
                    })?
            }
        };
        self.state(&workspaces, c).map_err(LoadError::Failed)
    }

    fn foreground(&self, pane: &str) -> Foreground {
        Herdr::foreground(self, pane)
    }

    fn require_native(&self, kind: &str) -> Result<(), String> {
        self.require_native_server(kind)
    }

    fn create_workspace(&self, label: &str, cwd: &Path, env: &PaneEnv) -> Result<Created, String> {
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
        args.extend(env_args(env));
        let r = self
            .call(&args)
            .map_err(|e| format!("could not create workspace \"{label}\": {e}"))?;
        Ok(Created {
            workspace: str_at(&r, &["workspace", "workspace_id"])?,
            tab: str_at(&r, &["tab", "tab_id"])?,
            pane: str_at(&r, &["root_pane", "pane_id"])?,
        })
    }

    fn rename_workspace(&self, workspace: &str, label: &str) -> Result<(), String> {
        self.call(&["workspace", "rename", workspace, label])
            .map(drop)
            .map_err(|e| format!("could not rename {workspace}: {e}"))
    }

    fn rename_tab(&self, tab: &str, label: &str) -> Result<(), String> {
        self.call(&["tab", "rename", tab, label])
            .map(drop)
            .map_err(|e| format!("could not rename {tab}: {e}"))
    }

    fn create_tab(
        &self,
        workspace: &str,
        label: &str,
        cwd: &Path,
        env: &PaneEnv,
    ) -> Result<String, String> {
        let cwd = cwd.to_string_lossy();
        let mut args: Vec<String> = [
            "tab",
            "create",
            "--workspace",
            workspace,
            "--cwd",
            &cwd,
            "--label",
            label,
            "--no-focus",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        args.extend(env_args(env));
        let r = self
            .call(&args)
            .map_err(|e| format!("could not create tab \"{label}\": {e}"))?;
        str_at(&r, &["root_pane", "pane_id"])
    }

    fn start_agent(&self, s: &AgentStart) -> Result<Started, String> {
        if s.resume.is_some() {
            return Err(format!(
                "{}: the Herdr TUI resumes its agents itself",
                s.name
            ));
        }
        let mut args: Vec<String> = [
            "agent",
            "start",
            s.name,
            "--kind",
            s.kind.as_str(),
            "--pane",
            s.pane,
            "--timeout",
            "60000",
            "--",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        args.extend_from_slice(s.args);
        if let Some(msg) = s.message {
            if s.kind == Kind::Agy {
                // `agy` rejects positional prompts; pass initial interactive messages via flag.
                args.extend(["--prompt-interactive".into(), msg.to_string()]);
            } else {
                args.push(msg.to_string());
            }
        }
        let start = Instant::now();
        loop {
            match self.call(&args) {
                Ok(_) => return Ok(Started::Ready),
                // The pane's shell has not reached its prompt yet.
                Err(e)
                    if e.code == "agent_pane_busy" && start.elapsed() < Duration::from_secs(30) =>
                {
                    sleep(Duration::from_secs(1));
                }
                Err(e) if e.code == "agent_not_ready" => return Ok(Started::Waiting),
                Err(e) => return Err(format!("could not start {} in {}: {e}", s.name, s.pane)),
            }
        }
    }

    fn run_in_pane(&self, pane: &str, command: &str) -> Result<(), String> {
        self.call(&["pane", "run", pane, command])
            .map(drop)
            .map_err(|e| format!("could not type in {pane}: {e}"))
    }

    fn close_tab(&self, tab: &str) -> Result<(), String> {
        self.call(&["tab", "close", tab])
            .map(drop)
            .map_err(|e| e.to_string())
    }

    fn agent_state(&self, pane: &str) -> Result<AgentState, String> {
        let info = self
            .call(&["agent", "get", pane])
            .map_err(|e| e.to_string())?;
        Ok(
            match info["agent"]["agent_status"].as_str().unwrap_or("unknown") {
                "idle" | "done" | "working" => AgentState::Available,
                "blocked" => AgentState::Blocked,
                other => AgentState::Other(other.to_string()),
            },
        )
    }

    fn prompt(&self, pane: &str, text: &str) -> Result<(), PromptError> {
        self.call(&["agent", "prompt", pane, text])
            .map(drop)
            .map_err(|e| match e.code.as_str() {
                "agent_blocked" => PromptError::Blocked,
                "agent_not_found" | "agent_not_ready" | "no_herdr" => {
                    PromptError::Rejected(e.to_string())
                }
                _ => PromptError::Uncertain(e.to_string()),
            })
    }

    fn will_attach(&self, attach: bool) -> bool {
        Herdr::will_attach(self, attach)
    }

    fn focus_and_attach(&self, workspace: &str, root: &Path, attach: bool) -> Result<(), String> {
        Herdr::focus_and_attach(self, workspace, root, attach)
    }

    fn codex_context(&self, pane: &str) -> Result<crate::codex::Context, String> {
        codex::tui_context(self, pane)
    }

    fn tui(&self) -> Option<&Herdr> {
        Some(self)
    }
}

/// `--env K=V` for each variable of a new pane.
fn env_args(env: &PaneEnv) -> impl Iterator<Item = String> + '_ {
    env.iter()
        .flat_map(|(k, v)| ["--env".to_string(), format!("{k}={v}")])
}

/// Whether this process runs in a herdr pane (`HERDR_ENV=1`).
pub fn inside_herdr() -> bool {
    std::env::var("HERDR_ENV").is_ok_and(|v| v == "1")
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

    pub fn warn(&mut self, herdr: &dyn Backend, msg: &str) {
        eprintln!("herdr-crew: {msg}");
        herdr.notify(msg);
    }
}

/// Runs the steps in order, resolves the references with the real answers and returns the
/// project's workspace. An error stops the plan (`up` is re-entrant).
pub fn execute(
    herdr: &dyn Backend,
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
            | Step::SetAgent { path, .. }
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
    let mut agy_roles = Vec::new();
    for step in &plan.steps {
        match step {
            Step::StartAgent {
                name,
                agent: AgentOptions::Pi(_),
                ..
            }
            | Step::ResumeAgent {
                name,
                agent: AgentOptions::Pi(_),
                ..
            } => pi_roles.push(name.as_str()),
            Step::StartAgent {
                name,
                agent: AgentOptions::Codex(options),
                ..
            } => codex_roles.push((name.as_str(), options)),
            Step::StartAgent {
                name,
                agent: AgentOptions::Agy(options),
                ..
            }
            | Step::ResumeAgent {
                name,
                agent: AgentOptions::Agy(options),
                ..
            } => agy_roles.push((name.as_str(), options)),
            _ => {}
        }
    }
    if !pi_roles.is_empty() {
        crate::pi::preflight()?;
        crate::pi::require_version("pi")?;
    }
    if !agy_roles.is_empty() {
        crate::agy::preflight()?;
        crate::agy::require_version("agy")?;
    }
    let mut agy_starts = plan.steps.iter().filter_map(|step| match step {
        Step::StartAgent {
            name,
            agent: AgentOptions::Agy(_),
            ..
        } => Some(name.as_str()),
        _ => None,
    });
    if let Some(first) = agy_starts.next() {
        let trust = crate::agy::TrustedWorkspaces::read()?;
        for name in std::iter::once(first).chain(agy_starts) {
            trust.require(c, name)?;
        }
    }
    if !codex_roles.is_empty() {
        codex::validate_runtime(&c.root)?;
    }
    // Every Codex, pi and agy role needs herdr's native integrations.
    let native = if !codex_roles.is_empty() {
        Some("Codex")
    } else if !pi_roles.is_empty() {
        Some("pi")
    } else if !agy_roles.is_empty() {
        Some("agy")
    } else {
        None
    };
    if let Some(kind) = native {
        herdr.require_native(kind)?;
    }
    for name in pi_roles {
        if let Some(problem) = crate::pi::prompt_problem(name, prompt_of(name)?) {
            return Err(problem);
        }
    }
    for (name, options) in agy_roles {
        if let Some(problem) = crate::agy::prompt_problem(name, prompt_of(name)?) {
            return Err(problem);
        }
        let mut effective = options.clone();
        effective.resolve_dirs(&c.root)?;
    }
    // `codex-resume` repeats this check in the pane, but a refusal there looks like a missing
    // conversation and herdr-core then forgets the session; refuse here instead.
    if plan.steps.iter().any(|step| {
        matches!(
            step,
            Step::ResumeAgent {
                agent: AgentOptions::Codex(_),
                ..
            }
        )
    }) {
        codex::integration::require_version(c.codex_tree)?;
    }
    for (name, options) in codex_roles {
        codex::context_prompt(c, name, prompt_of(name)?)?;
        let cwd = if c.session_role(name).unwrap().worktree {
            c.worktree_path(name).unwrap()
        } else {
            c.root.clone()
        };
        codex::integration::preflight(&cwd, options, &c.root, c.codex_tree)?;
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
    // The variables of a new pane: Codex roles start with the crew's Codex home.
    let pane_env = |label: &str| -> Result<Vec<(String, String)>, String> {
        if c.session_role(label)
            .is_some_and(|role| role.kind == Kind::Codex)
        {
            codex::pane_environment()
        } else {
            Ok(Vec::new())
        }
    };
    for step in &plan.steps {
        match step {
            Step::SetAgent {
                path,
                kind,
                content,
            } => {
                files::write(&c.root, path, content)?;
                out.step(&format!(
                    "default agent set to {} in {}",
                    kind.as_str(),
                    path.display()
                ));
            }
            Step::CreateWorkspace { label, cwd } => {
                // The workspace's first pane hosts the tab the plan renames next.
                let first = plan.steps.iter().find_map(|step| match step {
                    Step::RenameTab { label } => Some(label.as_str()),
                    _ => None,
                });
                let env = match first {
                    Some(first) => pane_env(first)?,
                    None => Vec::new(),
                };
                let created = herdr.create_workspace(label, cwd, &env)?;
                out.step(&format!(
                    "workspace \"{label}\" created ({})",
                    created.workspace
                ));
                initial = Some((created.tab, created.pane));
                workspace = Some(created.workspace);
            }
            Step::AdoptWorkspace { initial: i, label } => {
                if i.label != *label {
                    herdr.rename_workspace(&i.workspace, label)?;
                }
                out.step(&format!("workspace {} adopted as \"{label}\"", i.workspace));
                initial = Some((i.tab.clone(), i.pane.clone()));
                workspace = Some(i.workspace.clone());
            }
            Step::RenameTab { label } => {
                let (tab, pane) = initial.clone().ok_or("there is no initial tab to rename")?;
                herdr.rename_tab(&tab, label)?;
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
                let pane = herdr.create_tab(&ws, label, cwd, &pane_env(label)?)?;
                panes.insert(label.clone(), pane);
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
                    let context = herdr.codex_context(&pane)?;
                    codex::prepare(context, c, name, prompt_of(name)?, options)?;
                }
                let native = agent_args(c, name, prompt, agent)?;
                let request = AgentStart {
                    name,
                    pane: &pane,
                    kind: agent.kind(),
                    args: &native,
                    message: message.as_deref(),
                    resume: None,
                };
                let started = if matches!(agent, AgentOptions::Agy(_)) {
                    let key = herdr.pane_key(&pane)?;
                    crate::agy::launch(
                        &c.root,
                        name,
                        &key,
                        prompt_of(name)?,
                        message.is_some(),
                        || herdr.start_agent(&request),
                    )?
                } else {
                    herdr.start_agent(&request)?
                };
                match started {
                    Started::Ready => out.step(&format!("{name} in {pane}")),
                    Started::Waiting | Started::NewConversation { .. } | Started::NotResumed => {
                        out.warn(
                            herdr,
                            &format!(
                                "{name} in {pane} is waiting for an answer at startup; check that tab"
                            ),
                        )
                    }
                }
            }
            Step::ResumeAgent {
                agent,
                name,
                pane,
                prompt,
                session,
            } => {
                let native = agent_args(c, name, prompt, agent)?;
                let request = AgentStart {
                    name,
                    pane,
                    kind: agent.kind(),
                    args: &native,
                    message: None,
                    resume: Some(session),
                };
                match herdr.start_agent(&request)? {
                    Started::Ready => out.step(&format!("{name} resumed in {pane}")),
                    Started::Waiting => out.warn(
                        herdr,
                        &format!(
                            "{name} resumed in {pane} but waits for an answer; check that tab"
                        ),
                    ),
                    Started::NotResumed => out.warn(
                        herdr,
                        &format!(
                            "{name}: could not resume session {session} in {pane} (see that tab); \
                             {}",
                            match agent.kind() {
                                Kind::Codex => format!(
                                    "retry there with `herdr-crew codex-resume {session}`, or close \
                                     the tab and run `herdr-crew up` to start it anew"
                                ),
                                Kind::Agy => format!(
                                    "retry there with `agy --conversation {session}`, or close the \
                                     tab and run `herdr-crew up` to start it anew"
                                ),
                                _ => "close the tab and run `herdr-crew up` to start it anew"
                                    .to_string(),
                            }
                        ),
                    ),
                    Started::NewConversation { waiting } => out.warn(
                        herdr,
                        &format!(
                            "{name}: session {session} held no conversation to resume (the agent \
                             stopped before saving one); started it anew in {pane}{}",
                            if waiting {
                                ", where it waits for an answer"
                            } else {
                                ""
                            }
                        ),
                    ),
                }
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
                herdr.run_in_pane(&pane, command)?;
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

/// The agent's own arguments for a role. A resumed pi session already holds its role prompt
/// (the crew's extension restores it), so it gets only its model options.
fn agent_args(
    c: &Config,
    name: &str,
    prompt: &Path,
    agent: &AgentOptions,
) -> Result<Vec<String>, String> {
    Ok(match agent {
        AgentOptions::Claude(options) => {
            let mut args = vec![
                "-n".into(),
                name.to_string(),
                "--append-system-prompt-file".into(),
                prompt.display().to_string(),
            ];
            args.extend(options.args());
            args
        }
        AgentOptions::Codex(options) => {
            let mut effective = options.clone();
            effective.resolve_dirs(&c.root)?;
            let mut args = effective.args();
            args.extend(codex::crew_overrides(c.codex_tree));
            args
        }
        // The crew's pi extension reads these flags and keeps the prompt in the session. A
        // resumed session that already holds it ignores them; one pi never saved gets it.
        AgentOptions::Pi(pi) => {
            let mut args = vec![
                "--herdr-crew-role".into(),
                name.to_string(),
                "--herdr-crew-prompt".into(),
                prompt.display().to_string(),
            ];
            args.extend(pi.args());
            args
        }
        // The crew's agy SessionStart hook consumes this one-shot pane binding.
        AgentOptions::Agy(options) => {
            let mut effective = options.clone();
            effective.resolve_dirs(&c.root)?;
            effective.args()
        }
    })
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
