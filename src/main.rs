//! herdr-crew: starts one coding agent session per role in a herdr workspace, each with its role
//! prompt, and draws the `.herdr/status.json` board. Arguments, root, dispatch and exit codes
//! (adapter). Design: docs/design.md.

mod agent;
mod board;
mod codex;
mod config;
mod files;
mod git;
mod herdr;
mod init;
mod lock;
mod pi;
mod plan;
mod process;
mod prompt;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread::sleep;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde_json::Value;

use config::Config;
use herdr::{Herdr, Output};
use lock::{Busy, ProjectLock};
use plan::{Candidate, Env, Foreground, HerdrState, Initial, Refusal, Tab, WorkspaceItem};

const USAGE: &str = "usage: herdr-crew [--root DIR] <command>
  init [--preset solo|review|parallel|research] [--name NAME] [--agent claude|codex|pi]
       [--base REMOTE/BRANCH] [--shared-checkout] [--no-ignore] [--yes] [--dry-run]
                                 configure a crew with the setup wizard
  up [--no-attach] [--dry-run]   start or complete the project's sessions
  add <role>                     add an extra instance of a role with extra = true
  close <name>                   close the tab of an extra instance (keeps its worktree)
  board [--file PATH] [--once] [--interval S]
                                 draw the status board
  codex-install                  install the crew hook; review it in Codex /hooks
  codex-uninstall                remove only the crew hook
  codex-list                     list saved role bindings and Codex session IDs
  codex-resume <binding>          recover a saved Codex session in its role tab
  pi-install                     install the crew pi extension that keeps role prompts
  pi-uninstall                   remove only the crew pi extension
  check                          validate the configuration and the dependencies
  startup                        herdr's startup hook: bring up or repair the projects in herdr";

/// A failure with its exit code: 2 for an invalid command line or configuration, 1 when the
/// plan stopped or something else failed (design §4.3).
struct Fail {
    code: u8,
    lines: Vec<String>,
}

impl Fail {
    fn usage(msg: impl Into<String>) -> Fail {
        Fail {
            code: 2,
            lines: vec![format!("herdr-crew: {}", msg.into()), USAGE.into()],
        }
    }

    fn run(msg: impl Into<String>) -> Fail {
        Fail {
            code: 1,
            lines: vec![format!("herdr-crew: {}", msg.into())],
        }
    }
}

#[derive(Default)]
struct Args {
    root: Option<PathBuf>,
    command: String,
    positional: Vec<String>,
    no_attach: bool,
    dry_run: bool,
    once: bool,
    file: Option<PathBuf>,
    interval: Option<Duration>,
    init: init::Options,
}

/// The longest `board --interval`; the board ages its timestamps, so slower refreshes help no one.
const MAX_INTERVAL_SECS: u64 = 3600;

/// A `--interval` in seconds: finite, at most `MAX_INTERVAL_SECS`, and not rounding to zero.
fn interval(v: &str) -> Option<Duration> {
    let secs: f64 = v.parse().ok()?;
    Duration::try_from_secs_f64(secs)
        .ok()
        .filter(|d| !d.is_zero() && *d <= Duration::from_secs(MAX_INTERVAL_SECS))
}

fn parse_args(raw: Vec<String>) -> Result<Args, Fail> {
    let mut a = Args::default();
    let mut it = raw.into_iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .ok_or_else(|| Fail::usage(format!("{name} needs a value")))
        };
        match arg.as_str() {
            "--root" => a.root = Some(value("--root")?.into()),
            "--file" => a.file = Some(value("--file")?.into()),
            "--interval" => {
                let v = value("--interval")?;
                a.interval = Some(interval(&v).ok_or_else(|| {
                    Fail::usage(format!(
                        "--interval \"{v}\": must be a number of seconds above 0 and at most \
                         {MAX_INTERVAL_SECS}"
                    ))
                })?);
            }
            "--no-attach" => a.no_attach = true,
            "--preset" => {
                let preset = value("--preset")?;
                a.init.preset = Some(init::Preset::parse(&preset).ok_or_else(|| {
                    Fail::usage("--preset must be solo, review, parallel or research")
                })?);
            }
            "--agent" => {
                a.init.agent = Some(
                    agent::Kind::parse(&value("--agent")?)
                        .ok_or_else(|| Fail::usage("--agent must be claude, codex or pi"))?,
                )
            }
            "--name" => a.init.name = Some(value("--name")?),
            "--base" => a.init.base = Some(value("--base")?),
            "--shared-checkout" => a.init.shared_checkout = true,
            "--yes" => a.init.yes = true,
            "--no-ignore" => a.init.no_ignore = true,
            "--dry-run" => a.dry_run = true,
            "--once" => a.once = true,
            s if s.starts_with('-') => return Err(Fail::usage(format!("unknown option {s}"))),
            _ if a.command.is_empty() => a.command = arg,
            _ => a.positional.push(arg),
        }
    }
    let allowed: &[&str] = match a.command.as_str() {
        "init" => &[
            "agent",
            "preset",
            "name",
            "base",
            "shared_checkout",
            "no_ignore",
            "yes",
            "dry_run",
        ],
        "up" => &["no_attach", "dry_run"],
        "add" | "close" | "check" | "startup" | "codex-install" | "codex-uninstall"
        | "codex-hook" | "codex-resume" | "codex-protocol" | "codex-list" | "pi-install"
        | "pi-uninstall" => &[],
        "board" => &["file", "once", "interval"],
        "" => return Err(Fail::usage("missing command")),
        other => return Err(Fail::usage(format!("unknown command \"{other}\""))),
    };
    let used = [
        ("no_attach", a.no_attach),
        ("dry_run", a.dry_run),
        ("once", a.once),
        ("file", a.file.is_some()),
        ("interval", a.interval.is_some()),
        ("agent", a.init.agent.is_some()),
        ("preset", a.init.preset.is_some()),
        ("name", a.init.name.is_some()),
        ("base", a.init.base.is_some()),
        ("shared_checkout", a.init.shared_checkout),
        ("yes", a.init.yes),
        ("no_ignore", a.init.no_ignore),
    ];
    if let Some((name, _)) = used.iter().find(|(n, on)| *on && !allowed.contains(n)) {
        return Err(Fail::usage(format!(
            "{} does not take --{}",
            a.command,
            name.replace('_', "-")
        )));
    }
    let arity = if matches!(a.command.as_str(), "add" | "close" | "codex-resume") {
        1
    } else {
        0
    };
    if a.positional.len() != arity {
        return Err(Fail::usage(format!(
            "{} takes {arity} argument(s)",
            a.command
        )));
    }
    Ok(a)
}

/// The project root (design §2.1): `--root`, the action's `workspace_cwd` or the current
/// directory, and from there the main checkout.
fn resolve_root(flag: Option<&Path>) -> Result<PathBuf, Fail> {
    let start = if let Some(r) = flag {
        r.to_path_buf()
    } else if std::env::var_os("HERDR_PLUGIN_ID").is_some() {
        let ctx: Value = std::env::var("HERDR_PLUGIN_CONTEXT_JSON")
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null);
        // Only `workspace_cwd`: a focused pane in another repository does not change the project.
        let cwd = ctx["workspace_cwd"].as_str().ok_or_else(|| {
            Fail::run("don't know which project: focus a workspace of the project")
        })?;
        PathBuf::from(cwd)
    } else {
        std::env::current_dir().map_err(|e| Fail::run(format!("no current directory: {e}")))?
    };
    git::main_root(&start).map_err(Fail::run)
}

fn load_config(root: &Path) -> Result<Config, Fail> {
    let path = root.join(config::CONFIG_FILE);
    files::validate(root, &path, files::Kind::File).map_err(Fail::run)?;
    let text = std::fs::read_to_string(&path).map_err(|e| Fail {
        code: 2,
        lines: vec![format!("herdr-crew: cannot read {}: {e}", path.display())],
    })?;
    let config = Config::parse(&text, root).map_err(|errors| Fail {
        code: 2,
        lines: errors.iter().map(ToString::to_string).collect(),
    })?;
    files::validate_config(&config).map_err(Fail::run)?;
    Ok(config)
}

fn binary() -> PathBuf {
    std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .unwrap_or_else(|_| "herdr-crew".into())
}

fn env(c: &Config, state: &HerdrState, herdr: Option<&Herdr>, worktrees: BTreeSet<String>) -> Env {
    let board_pane = state.workspace.as_ref().and_then(|w| {
        let tab = w.tabs.iter().find(|t| t.label == c.board.tab)?;
        let pane = w.panes.iter().find(|p| p.tab_id == tab.id)?;
        herdr.map(|h| h.foreground(&pane.id))
    });
    let now = Timestamp::now()
        .to_zoned(TimeZone::system())
        .strftime("%Y-%m-%dT%H:%M:%S%:z")
        .to_string();
    Env {
        agent_cmd: std::env::var("CREW_AGENT_CMD")
            .ok()
            .filter(|s| !s.is_empty()),
        binary: binary(),
        worktrees,
        board_pane,
        status_exists: c.board_path().exists(),
        current_schema: std::fs::read_to_string(c.root.join(prompt::SCHEMA_FILE)).ok(),
        now,
        windows: cfg!(windows),
    }
}

fn fail_notify(herdr: &Herdr, msg: String) -> Fail {
    herdr.notify(&msg);
    Fail::run(msg)
}

/// How long `up`, `add` and `close` wait while another command changes the same project.
const LOCK_WAIT: Duration = Duration::from_secs(120);

/// The project's lock for a command that changes it. Taken before reading any state, so the
/// plan always starts from what the previous command left.
fn lock_project(c: &Config, herdr: &Herdr) -> Result<ProjectLock, Fail> {
    ProjectLock::acquire(&c.root, LOCK_WAIT, || {
        eprintln!(
            "herdr-crew: another herdr-crew command is changing {}; waiting up to {} s",
            c.root.display(),
            LOCK_WAIT.as_secs()
        );
    })
    .map_err(|busy| match busy {
        Busy::Held => fail_notify(
            herdr,
            format!(
                "another herdr-crew command is still changing {}; run this again when it finishes",
                c.root.display()
            ),
        ),
        Busy::Error(e) => fail_notify(herdr, e),
    })
}

fn up(c: &Config, a: &Args) -> Result<(), Fail> {
    let herdr = Herdr::from_env();
    let lock = if a.dry_run {
        None
    } else {
        Some(lock_project(c, &herdr)?)
    };
    let worktrees = git::existing_worktrees(c).map_err(Fail::run)?;
    let mut out = Output;
    let workspaces = if a.dry_run {
        herdr.call(&["workspace", "list"]).ok()
    } else {
        Some(
            herdr
                .ensure_server(&c.root, &mut out)
                .map_err(|e| fail_notify(&herdr, e))?,
        )
    };
    let state = match &workspaces {
        Some(w) => herdr.state(w, c).map_err(|e| fail_notify(&herdr, e))?,
        None => {
            println!("(no herdr server: the plan assumes an empty one)");
            HerdrState::default()
        }
    };
    let e = env(c, &state, Some(&herdr), worktrees);
    let p = plan::plan(c, &state, &e);
    if a.dry_run {
        print!("{}", p.dry_run());
        return Ok(());
    }
    let ws = herdr::execute(&herdr, c, &state, &p, &mut out).map_err(|e| fail_notify(&herdr, e))?;
    if p.actions().next().is_none() {
        out.step("nothing to do");
    }
    // Attaching lasts until the user detaches; other commands may change the project meanwhile.
    drop(lock);
    if let Some(ws) = ws {
        herdr
            .focus_and_attach(&ws, &c.root, !a.no_attach)
            .map_err(Fail::run)?;
    }
    Ok(())
}

/// How long `startup` waits for the initial pane's shell to finish sourcing its rc files.
const SHELL_WAIT: Duration = Duration::from_secs(2);

/// `startup`, run by herdr's `[[startup]]` hook (design §4.4). It adopts herdr's initial
/// workspace when it is a fresh one in a project's root, and otherwise only repairs the
/// projects whose workspace already exists. It never notifies, never attaches, and focuses only
/// an adopted workspace.
fn startup() -> Result<(), Fail> {
    let mut herdr = Herdr::from_env();
    herdr.silent = true;
    let workspaces = herdr
        .call(&["workspace", "list"])
        .map_err(|e| Fail::run(format!("the herdr server does not answer: {e}")))?;
    let items = plan::workspace_items(&workspaces).map_err(Fail::run)?;
    let context: Option<String> = std::env::var("HERDR_PLUGIN_CONTEXT_JSON")
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v["workspace_id"].as_str().map(str::to_string));
    // The roots of the projects in herdr: only the directory of each workspace's first pane.
    let mut roots: Vec<PathBuf> = Vec::new();
    for w in &items {
        let Ok(panes) = panes_of(&herdr, &w.workspace_id) else {
            continue;
        };
        let Some(root) = panes
            .first()
            .and_then(|p| p.cwd.as_deref())
            .and_then(|d| git::main_root(d).ok())
            .filter(|r| r.join(config::CONFIG_FILE).is_file())
        else {
            continue;
        };
        let root = root.canonicalize().unwrap_or(root);
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    let mut failed = false;
    for root in &roots {
        if let Err(f) = startup_root(&herdr, root, context.as_deref()) {
            for line in f.lines {
                eprintln!("{line}");
            }
            failed = true;
        }
    }
    if failed {
        Err(Fail {
            code: 1,
            lines: Vec::new(),
        })
    } else {
        Ok(())
    }
}

/// One project for `startup`. On the adoption path the user is watching herdr open, so an error
/// is also shown as a notification; warnings, and everything on a restore, stay in the log.
fn startup_root(herdr: &Herdr, root: &Path, context: Option<&str>) -> Result<(), Fail> {
    // Never wait: a cold `up` starts the server that runs this hook while holding the lock, and
    // whichever command holds it is already bringing the project up.
    let _lock = match ProjectLock::acquire(root, Duration::ZERO, || {}) {
        Ok(lock) => lock,
        Err(Busy::Held) => {
            println!(
                "herdr-crew: {}: another herdr-crew command is changing it; startup leaves it \
                 to that command",
                root.display()
            );
            return Ok(());
        }
        Err(Busy::Error(e)) => return Err(Fail::run(e)),
    };
    // Read again for every root: an earlier one may have renamed a workspace.
    let workspaces = herdr
        .call(&["workspace", "list"])
        .map_err(|e| Fail::run(e.to_string()))?;
    let items = plan::workspace_items(&workspaces).map_err(Fail::run)?;
    let adoption = adoption(herdr, root, &items, context)?;
    let watching = adoption.is_ok();
    let result = bring_up(herdr, root, &workspaces, adoption);
    if let (true, Err(f)) = (watching, &result) {
        herdr.show(&f.lines.join("\n"));
    }
    result
}

fn bring_up(
    herdr: &Herdr,
    root: &Path,
    workspaces: &Value,
    adoption: Result<Initial, Refusal>,
) -> Result<(), Fail> {
    let c = load_config(root)?;
    let mut out = Output;
    let state = herdr.state(workspaces, &c).map_err(Fail::run)?;
    let state = match plan::startup_state(state, adoption) {
        Ok(s) => s,
        Err(why) => {
            out.step(&format!("{}: nothing to do ({why})", c.label));
            return Ok(());
        }
    };
    let adopted = state.adopt.is_some();
    let worktrees = git::existing_worktrees(&c).map_err(Fail::run)?;
    let p = plan::plan(&c, &state, &env(&c, &state, Some(herdr), worktrees));
    // What startup saw, for the plugin log.
    print!("{}", startup_verdicts(&c.label, &p));
    let ws = herdr::execute(herdr, &c, &state, &p, &mut out).map_err(Fail::run)?;
    if p.actions().next().is_none() {
        out.step(&format!("{}: nothing to do", c.label));
    }
    if let (true, Some(ws)) = (adopted, ws) {
        let _ = herdr.call(&["workspace", "focus", &ws]);
    }
    Ok(())
}

/// The verdicts of a project in `startup`'s log: a header, then the `--dry-run` lines.
fn startup_verdicts(label: &str, p: &plan::Plan) -> String {
    format!("herdr-crew: {label}\n{}", p.verdict_lines())
}

/// Whether `startup` may adopt the workspace of its context (design §4.4). It waits up to
/// `SHELL_WAIT` for the pane's shell to come to the foreground.
fn adoption(
    herdr: &Herdr,
    root: &Path,
    workspaces: &[WorkspaceItem],
    context: Option<&str>,
) -> Result<Result<Initial, Refusal>, Fail> {
    let Some(id) = context else {
        return Ok(Err(Refusal::NoContext));
    };
    let tabs: Vec<Tab> = herdr
        .call(&["tab", "list", "--workspace", id])
        .map_err(|e| Fail::run(e.to_string()))
        .and_then(|v| {
            serde_json::from_value(v["tabs"].clone()).map_err(|e| Fail::run(e.to_string()))
        })?;
    let panes = panes_of(herdr, id)?;
    let pane_dir = panes
        .first()
        .and_then(|p| p.cwd.as_deref())
        .and_then(|d| d.canonicalize().ok());
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let start = Instant::now();
    loop {
        let foreground = match panes.as_slice() {
            [p] => herdr.foreground(&p.id),
            _ => Foreground::Unknown("more than one pane".into()),
        };
        let verdict = plan::adoption(&Candidate {
            context: Some(id),
            workspaces,
            tabs: &tabs,
            panes: &panes,
            pane_dir: pane_dir.as_deref(),
            root: &root,
            foreground: &foreground,
        });
        match verdict {
            Err(Refusal::Foreground(_)) if start.elapsed() < SHELL_WAIT => {
                sleep(Duration::from_millis(200));
            }
            v => return Ok(v),
        }
    }
}

fn panes_of(herdr: &Herdr, workspace: &str) -> Result<Vec<plan::Pane>, Fail> {
    herdr
        .call(&["pane", "list", "--workspace", workspace])
        .map_err(|e| Fail::run(e.to_string()))
        .and_then(|v| plan::pane_items(&v).map_err(Fail::run))
}

fn current_state(c: &Config, herdr: &Herdr) -> Result<HerdrState, Fail> {
    let workspaces = herdr
        .call(&["workspace", "list"])
        .map_err(|e| Fail::run(format!("the herdr server does not answer: {e}")))?;
    herdr.state(&workspaces, c).map_err(Fail::run)
}

fn add(c: &Config, role: &str) -> Result<(), Fail> {
    let herdr = Herdr::from_env();
    let _lock = lock_project(c, &herdr)?;
    let worktrees = git::existing_worktrees(c).map_err(Fail::run)?;
    let state = current_state(c, &herdr)?;
    let (name, p) =
        plan::plan_add(c, &state, &env(c, &state, None, worktrees), role).map_err(Fail::run)?;
    let mut out = Output;
    herdr::execute(&herdr, c, &state, &p, &mut out).map_err(|e| fail_notify(&herdr, e))?;
    out.step(&format!("new instance {name}"));
    Ok(())
}

fn close(c: &Config, name: &str) -> Result<(), Fail> {
    let herdr = Herdr::from_env();
    let _lock = lock_project(c, &herdr)?;
    let state = current_state(c, &herdr)?;
    let close = plan::plan_close(c, &state, name).map_err(Fail::run)?;
    herdr
        .call(&["tab", "close", &close.tab_id])
        .map_err(|e| Fail::run(format!("could not close {name}: {e}")))?;
    let mut out = Output;
    match close.worktree.filter(|p| p.exists()) {
        Some(p) => out.step(&format!(
            "{name} closed; its worktree stays in {} and a later add that gives this number again \
             reuses it (git worktree remove deletes it)",
            p.display()
        )),
        None => out.step(&format!("{name} closed")),
    }
    Ok(())
}

fn board(c: &Config, a: &Args) -> Result<(), Fail> {
    let path = a.file.clone().unwrap_or_else(|| c.board_path());
    let result = if a.once {
        board::tui::once(&path, c)
    } else {
        board::tui::run(&path, c, a.interval.unwrap_or(Duration::from_millis(1500)))
    };
    result.map_err(|e| Fail::run(format!("board: {e}")))
}

/// Where `program` runs from: `configured` (a path or a bare name, like `HERDR_BIN_PATH`)
/// or else the `PATH`. Only an executable file counts.
fn executable(program: &str, configured: Option<&std::ffi::OsStr>) -> Result<PathBuf, String> {
    let wanted = configured.unwrap_or(program.as_ref());
    if Path::new(wanted).components().count() > 1 {
        let path = PathBuf::from(wanted);
        return if is_executable(&path) {
            Ok(path)
        } else {
            Err(format!("{} is not an executable file", path.display()))
        };
    }
    let wanted = wanted.to_string_lossy();
    let names: Vec<String> = if cfg!(windows) {
        vec![
            format!("{wanted}.exe"),
            format!("{wanted}.cmd"),
            wanted.to_string(),
        ]
    } else {
        vec![wanted.to_string()]
    };
    let found: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
                .filter(|path| path.is_file())
                .collect()
        })
        .unwrap_or_default();
    match found.iter().find(|path| is_executable(path)) {
        Some(path) => Ok(path.clone()),
        None if found.is_empty() => Err(format!("{wanted} is not on the PATH")),
        None => Err(format!(
            "{} is on the PATH but not executable",
            found[0].display()
        )),
    }
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    path.is_file()
}

/// The lowest herdr that runs any crew; Codex and pi roles need more (README, Requirements).
const MIN_HERDR: (u32, u32, u32) = (0, 9, 1);

fn check(c: &Config) -> Result<(), Fail> {
    println!(
        "herdr-crew {} ({})",
        env!("CARGO_PKG_VERSION"),
        binary().display()
    );
    println!(
        "herdr-crew: valid configuration in {}",
        c.root.join(config::CONFIG_FILE).display()
    );
    let installers: [(&str, &str); 3] = if cfg!(windows) {
        [
            ("git", "winget install --id Git.Git -e --source winget"),
            (
                "herdr",
                "powershell -ExecutionPolicy Bypass -c \"irm https://herdr.dev/install.ps1 | iex\"",
            ),
            ("claude", "irm https://claude.ai/install.ps1 | iex"),
        ]
    } else if cfg!(target_os = "macos") {
        [
            ("git", "xcode-select --install"),
            ("herdr", "curl -fsSL https://herdr.dev/install.sh | sh"),
            ("claude", "curl -fsSL https://claude.ai/install.sh | bash"),
        ]
    } else {
        [
            (
                "git",
                "the system package manager (e.g. sudo apt-get install -y git)",
            ),
            ("herdr", "curl -fsSL https://herdr.dev/install.sh | sh"),
            ("claude", "curl -fsSL https://claude.ai/install.sh | bash"),
        ]
    };
    let mut missing = Vec::new();
    let herdr_bin = std::env::var_os("HERDR_BIN_PATH");
    for (program, install) in installers {
        if program == "claude" && !c.roles.iter().any(|r| r.kind == agent::Kind::Claude) {
            continue;
        }
        let configured = herdr_bin.as_deref().filter(|_| program == "herdr");
        let path = match executable(program, configured) {
            Ok(path) => path,
            Err(why) if configured.is_some() => {
                missing.push(format!("herdr-crew: HERDR_BIN_PATH: {why}"));
                continue;
            }
            Err(why) => {
                missing.push(format!(
                    "herdr-crew: {program} is missing: {why} (install it with: {install})"
                ));
                continue;
            }
        };
        match process::detect(&path) {
            Ok((a, b, v)) if program == "herdr" && (a, b, v) < MIN_HERDR => {
                missing.push(format!(
                    "herdr-crew: herdr {a}.{b}.{v} at {} is too old: {}.{}.{} or newer is required",
                    path.display(),
                    MIN_HERDR.0,
                    MIN_HERDR.1,
                    MIN_HERDR.2
                ));
            }
            Ok((a, b, v)) => println!("herdr-crew: {program} {a}.{b}.{v} at {}", path.display()),
            Err(e) => missing.push(format!(
                "herdr-crew: {program} at {} does not run `--version`: {e}",
                path.display()
            )),
        }
    }
    if let Some(w) = &c.worktrees {
        let base = format!("{}/{}", w.remote, w.branch);
        match git::validate_base(&c.root, &base) {
            Err(e) => missing.push(format!("herdr-crew: worktrees.base \"{base}\": {e}")),
            Ok(()) if git::knows_remote_branch(&c.root, &w.remote, &w.branch) => {
                println!("herdr-crew: worktrees.base {base} is known locally");
            }
            Ok(()) => println!(
                "herdr-crew: worktrees.base {base} is not fetched yet; `up` fetches it before \
                 creating a worktree"
            ),
        }
        match git::existing_worktrees(c) {
            Ok(existing) => println!(
                "herdr-crew: {} existing role worktree(s) are valid",
                existing.len()
            ),
            Err(e) => missing.push(format!("herdr-crew: {e}")),
        }
    }
    let native = c.roles.iter().find_map(|r| match r.kind {
        agent::Kind::Codex => Some("Codex"),
        agent::Kind::Pi => Some("pi"),
        agent::Kind::Claude => None,
    });
    if native.is_some()
        && let Err(error) = Herdr::from_env().require_native_version()
    {
        missing.push(format!("herdr-crew: {error}"));
    }
    if c.roles.iter().any(|r| r.kind == agent::Kind::Codex) {
        if let Err(why) = executable("codex", None) {
            missing.push(format!(
                "herdr-crew: codex is missing: {why} (install: npm install -g @openai/codex)"
            ));
        } else {
            println!("herdr-crew: codex is on the PATH");
            for role in c.roles.iter().filter(|r| r.kind == agent::Kind::Codex) {
                let cwd = if role.worktree {
                    c.worktree_path(&role.name).unwrap()
                } else {
                    c.root.clone()
                };
                if let Err(error) = codex::integration::preflight(&cwd, &role.codex, &c.root) {
                    missing.push(format!("herdr-crew: {}: {error}", role.name));
                }
            }
        }
    }
    if c.roles.iter().any(|r| r.kind == agent::Kind::Pi) {
        match executable("pi", None) {
            Err(why) => missing.push(format!(
                "herdr-crew: pi is missing: {why} (install: npm install -g {})",
                pi::PACKAGE
            )),
            Ok(path) => match pi::require_version(&path) {
                Ok((a, b, v)) => println!("herdr-crew: pi {a}.{b}.{v} at {}", path.display()),
                Err(e) => missing.push(format!("herdr-crew: {e}")),
            },
        }
        match pi::preflight() {
            Ok(()) => println!("herdr-crew: the crew pi extension is current"),
            Err(error) => missing.push(format!("herdr-crew: {error}")),
        }
        // An extra's prompt names its instance; `-99` is the longest name `add` gives one.
        for role in c.roles.iter().filter(|r| r.kind == agent::Kind::Pi) {
            let longest = format!("{}-99", role.name);
            let names = std::iter::once(role.name.as_str()).chain(role.extra.then_some(&*longest));
            for name in names {
                if let Some(problem) = pi::prompt_problem(name, &prompt::render(c, name, &binary()))
                {
                    missing.push(format!("herdr-crew: {problem}"));
                }
            }
        }
    }
    if Herdr::from_env().call(&["workspace", "list"]).is_ok() {
        if let Some(kind) = native
            && let Err(error) = Herdr::from_env().require_server_version(kind)
        {
            missing.push(format!("herdr-crew: {error}"));
        }
        let path = plan::shell_quote(&binary().to_string_lossy());
        println!(
            "herdr-crew: a herdr server is already running; `herdr` only attaches and does not \
             run plugin startup. From this project, run `{path} up --no-attach`"
        );
    }
    let remote = c
        .worktrees
        .as_ref()
        .map(|w| format!("network access to {}, ", w.remote))
        .unwrap_or_default();
    println!("herdr-crew: not checked: {remote}agent sign-in and model access");
    if missing.is_empty() {
        Ok(())
    } else {
        Err(Fail {
            code: 1,
            lines: missing,
        })
    }
}

fn run() -> Result<(), Fail> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.iter().any(|a| a == "--version" || a == "-V") {
        println!("herdr-crew {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if raw.is_empty() || raw.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    let a = parse_args(raw)?;
    match a.command.as_str() {
        "codex-install" | "codex-uninstall" => {
            if a.root.is_some() {
                return Err(Fail::usage("Codex integration setup does not take --root"));
            }
            return codex::integration::install(a.command == "codex-uninstall").map_err(Fail::run);
        }
        "pi-install" | "pi-uninstall" => {
            if a.root.is_some() {
                return Err(Fail::usage("pi extension setup does not take --root"));
            }
            return pi::install(a.command == "pi-uninstall").map_err(Fail::run);
        }
        "codex-protocol" => {
            println!("1");
            return Ok(());
        }
        "codex-hook" => {
            codex::hook();
            return Ok(());
        }
        "codex-resume" => {
            return codex::resume(&a.positional[0], a.root.as_deref()).map_err(Fail::run);
        }
        _ => {}
    }
    if a.command == "startup" {
        if a.root.is_some() {
            return Err(Fail::usage("startup does not take --root"));
        }
        return startup();
    }
    let root = resolve_root(a.root.as_deref())?;
    if a.command == "codex-list" {
        return codex::list(&root).map_err(Fail::run);
    }
    if a.command == "init" {
        return init::run(&root, &a.init, a.dry_run);
    }
    let c = load_config(&root)?;
    match a.command.as_str() {
        "up" => up(&c, &a),
        "add" => add(&c, &a.positional[0]),
        "close" => close(&c, &a.positional[0]),
        "board" => board(&c, &a),
        "check" => check(&c),
        _ => unreachable!("validated in parse_args"),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(f) => {
            for line in f.lines {
                eprintln!("{line}");
            }
            ExitCode::from(f.code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Result<Args, u8> {
        parse_args(s.split_whitespace().map(String::from).collect()).map_err(|f| f.code)
    }

    #[test]
    fn parses_commands_and_global_root() {
        let a = args("up --dry-run --root /x").unwrap();
        assert!(a.dry_run && a.command == "up" && a.root.as_deref() == Some(Path::new("/x")));
        assert_eq!(args("add globex-dev").unwrap().positional, ["globex-dev"]);
        assert_eq!(args("startup").unwrap().command, "startup");
        let init = args(
            "init --preset parallel --name acme --base upstream/trunk --yes --no-ignore --dry-run",
        )
        .unwrap();
        assert_eq!(init.init.preset, Some(init::Preset::Parallel));
        assert_eq!(init.init.name.as_deref(), Some("acme"));
        assert_eq!(init.init.base.as_deref(), Some("upstream/trunk"));
        assert!(init.init.yes && init.init.no_ignore && init.dry_run);
        let a = args("board --file /f --once --interval 2").unwrap();
        assert!(a.once && a.interval == Some(Duration::from_secs(2)));
    }

    #[test]
    fn startup_prints_its_verdicts() {
        let p = plan::Plan {
            verdicts: vec![
                ("acme-lead".into(), plan::Verdict::NoAgent),
                ("acme-status".into(), plan::Verdict::Repair),
            ],
            steps: Vec::new(),
        };
        assert_eq!(
            startup_verdicts("acme", &p),
            "herdr-crew: acme\nacme-lead      no agent\nacme-status    repair\n"
        );
    }

    #[test]
    fn bad_command_lines_exit_with_2() {
        for bad in [
            "",
            "go",
            "up --once",
            "add",
            "close a b",
            "board --interval 0",
            "board --interval -1",
            "board --interval inf",
            "board --interval NaN",
            "board --interval 1e30",
            "board --interval 3600.5",
            "board --interval 1e-300",
            "board --interval two",
            "up --x",
            "board --file",
            "startup now",
            "startup --dry-run",
            "init --preset unknown",
            "init --base",
            "up --preset solo",
            "check --name acme",
            "board --yes",
        ] {
            assert_eq!(args(bad).err(), Some(2), "{bad}");
        }
    }
}
