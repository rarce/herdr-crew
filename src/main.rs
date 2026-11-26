//! herdr-crew: starts one coding agent session per role in a herdr workspace, each with its role
//! prompt, and draws the `.herdr/status.json` board. Arguments, root, dispatch and exit codes
//! (adapter). Design: docs/design.md.

mod agent;
mod agy;
mod app;
mod backend;
mod board;
mod codex;
mod config;
mod config_edit;
mod files;
mod git;
mod hc;
mod herdr;
mod init;
mod lock;
mod mcp;
mod memory;
mod models;
mod pi;
mod plan;
mod process;
mod prompt;
mod send;
mod tui;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread::sleep;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde_json::Value;

use backend::{Backend, Load, LoadError};
use config::Config;
use herdr::{Herdr, Output};
use lock::{Busy, ProjectLock};
use plan::{Candidate, Env, Foreground, HerdrState, Initial, Refusal, Tab, WorkspaceItem};

const USAGE: &str = "usage: herdr-crew [--root DIR] <command>
  init [--preset solo|review|parallel|research] [--name NAME] [--agent claude|codex|pi|agy]
       [--topology tabs|tree] [--models inherit|economy|balanced|quality]
       [--base REMOTE/BRANCH] [--shared-checkout] [--no-ignore] [--no-tui] [--yes]
       [--dry-run]
                                 configure a crew with the setup wizard
  config                         edit each role's agent, model and effort in a terminal UI
  up [--agent claude|codex|pi|agy] [--no-attach] [--dry-run]
     [--preset ...] [--topology tabs|tree] [--models ...] [--name NAME]
     [--base REMOTE/BRANCH] [--shared-checkout] [--no-ignore] [--no-tui] [--yes]
                                 start sessions; without a crew.toml, set one up first
                                 (setup options as for init); --agent on an existing
                                 crew saves a new default agent
  add <role>                     add an extra instance of a role with extra = true
  close <name>                   close the tab of an extra instance (keeps its worktree)
  send <session>                 send the message on stdin to another crew session
  send --check <session>         check the crew delivery route without sending
  board [--file PATH] [--once] [--interval S]
                                 draw the status board
  mem path                       print the project's out-of-repo operational memory directory
  mem init                       initialize or refresh SCHEMA.md and seed operational memory
  mem lint                       validate now.md, index.md orphans, wikilinks and log.md
  mem index                      print Layer 1 operational memory index (index.md)
  mem timeline [--limit N]       print Layer 2 recent log.md entries (default 20)
  mem search <query>             search operational memory pages
  mem show <page>                show a single operational memory page
  mem mcp [--write]              run a zero-daemon stdio MCP server for operational memory
                                 (read-only unless --write; give --write to the board writer)
  codex-install                  install the crew hook; review it in Codex /hooks
  codex-uninstall                remove only the crew hook
  codex-list                     list saved role bindings and Codex session IDs
  codex-peers                    JSON candidates from current Codex role bindings
  codex-resume <binding>          recover a saved Codex session in its role tab
  pi-install                     install the crew pi extension that keeps role prompts
  pi-uninstall                   remove only the crew pi extension
  agy-install                    install the crew agy SessionStart hook that delivers role prompts
  agy-uninstall                  remove only the crew agy hook
  app-install                    install the Herdr app mod Crew (herdr-core, experimental)
  app-uninstall                  remove only the crew's Herdr app mod
  check                          validate the configuration and the dependencies
  startup                        herdr's startup hook: bring up or repair the projects in herdr";

/// `send`'s own usage line: its caller is usually a model, which needs this, not every command.
const SEND_USAGE: &str =
    "herdr-crew: usage: herdr-crew [--root DIR] send <session>, with the message on stdin";

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
    agent: Option<agent::Kind>,
    root: Option<PathBuf>,
    command: String,
    positional: Vec<String>,
    no_attach: bool,
    dry_run: bool,
    send_check: bool,
    once: bool,
    file: Option<PathBuf>,
    interval: Option<Duration>,
    limit: Option<usize>,
    read_only: bool,
    write: bool,
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
                a.agent = Some(
                    agent::Kind::parse(&value("--agent")?)
                        .ok_or_else(|| Fail::usage("--agent must be claude, codex, pi or agy"))?,
                )
            }
            "--topology" => {
                let topology = value("--topology")?;
                a.init.topology = Some(
                    init::Topology::parse(&topology)
                        .ok_or_else(|| Fail::usage("--topology must be tabs or tree"))?,
                );
            }
            "--models" => {
                let profile = value("--models")?;
                a.init.models = Some(models::Profile::parse(&profile).ok_or_else(|| {
                    Fail::usage("--models must be inherit, economy, balanced or quality")
                })?);
            }
            "--no-tui" => a.init.no_tui = true,
            "--name" => a.init.name = Some(value("--name")?),
            "--base" => a.init.base = Some(value("--base")?),
            "--shared-checkout" => a.init.shared_checkout = true,
            "--yes" => a.init.yes = true,
            "--no-ignore" => a.init.no_ignore = true,
            "--dry-run" => a.dry_run = true,
            "--check" => a.send_check = true,
            "--once" => a.once = true,
            "--limit" => {
                let v = value("--limit")?;
                let n: usize = v.parse().ok().filter(|n| *n > 0).ok_or_else(|| {
                    Fail::usage(format!("--limit \"{v}\": must be a positive integer"))
                })?;
                a.limit = Some(n);
            }
            "--read-only" => a.read_only = true,
            "--write" => a.write = true,
            s if s.starts_with('-') => return Err(Fail::usage(format!("unknown option {s}"))),
            _ if a.command.is_empty() => a.command = arg,
            _ => a.positional.push(arg),
        }
    }
    let allowed: &[&str] = match a.command.as_str() {
        "init" => &[
            "agent",
            "preset",
            "topology",
            "models",
            "no_tui",
            "name",
            "base",
            "shared_checkout",
            "no_ignore",
            "yes",
            "dry_run",
        ],
        // The setup options apply when `up` finds no configuration and sets one up first.
        "up" => &[
            "agent",
            "no_attach",
            "dry_run",
            "preset",
            "topology",
            "models",
            "no_tui",
            "name",
            "base",
            "shared_checkout",
            "no_ignore",
            "yes",
        ],
        "send" => &["check"],
        "add" | "close" | "check" | "config" | "startup" | "codex-install" | "codex-uninstall"
        | "codex-hook" | "codex-resume" | "codex-protocol" | "codex-list" | "codex-peers"
        | "pi-install" | "pi-uninstall" | "agy-install" | "agy-uninstall" | "agy-hook"
        | "app-install" | "app-uninstall" => &[],
        "board" => &["file", "once", "interval"],
        "mem" => &["limit", "read_only", "write"],
        "" => return Err(Fail::usage("missing command")),
        other => return Err(Fail::usage(format!("unknown command \"{other}\""))),
    };
    let used = [
        ("no_attach", a.no_attach),
        ("dry_run", a.dry_run),
        ("check", a.send_check),
        ("once", a.once),
        ("file", a.file.is_some()),
        ("interval", a.interval.is_some()),
        ("limit", a.limit.is_some()),
        ("read_only", a.read_only),
        ("write", a.write),
        ("agent", a.agent.is_some()),
        ("preset", a.init.preset.is_some()),
        ("topology", a.init.topology.is_some()),
        ("models", a.init.models.is_some()),
        ("no_tui", a.init.no_tui),
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
    if a.command == "mem" {
        let Some(sub) = a.positional.first().map(String::as_str) else {
            return Err(Fail::usage(
                "mem requires a subcommand: path, init, lint, index, timeline, search, show or mcp",
            ));
        };
        let expected = match sub {
            "path" | "init" | "lint" | "index" | "timeline" | "mcp" => 1,
            "search" | "show" => 2,
            other => {
                return Err(Fail::usage(format!(
                    "unknown mem subcommand \"{other}\" (expected path, init, lint, index, timeline, search, show or mcp)"
                )));
            }
        };
        if a.limit.is_some() && sub != "timeline" {
            return Err(Fail::usage(format!("mem {sub} does not take --limit")));
        }
        if a.read_only && sub != "mcp" {
            return Err(Fail::usage(format!("mem {sub} does not take --read-only")));
        }
        if a.write && sub != "mcp" {
            return Err(Fail::usage(format!("mem {sub} does not take --write")));
        }
        if a.read_only && a.write {
            return Err(Fail::usage(
                "mem mcp takes --read-only (the default) or --write, not both",
            ));
        }
        if a.positional.len() != expected {
            return Err(Fail::usage(format!(
                "mem {sub} takes {} argument(s)",
                expected - 1
            )));
        }
    } else {
        let arity = if matches!(
            a.command.as_str(),
            "add" | "close" | "send" | "codex-resume"
        ) {
            1
        } else {
            0
        };
        if a.command == "send" && a.positional.len() != arity {
            return Err(Fail {
                code: 2,
                lines: vec![if a.send_check {
                    "herdr-crew: usage: herdr-crew [--root DIR] send --check <session>".into()
                } else {
                    SEND_USAGE.into()
                }],
            });
        }
        if a.positional.len() != arity {
            return Err(Fail::usage(format!(
                "{} takes {arity} argument(s)",
                a.command
            )));
        }
    }
    if a.command == "init" || a.command == "up" {
        a.init.agent = a.agent;
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
    let mut config = config;
    if config.uses_wiki_memory() {
        // Resolved here, in the adapter, so the core never reads the environment. A failure is
        // kept and surfaced only by the commands that use memory (`send`, `close` still work).
        match memory::resolve(root, &memory::BaseEnv::from_process()) {
            Ok(r) => {
                config.memory.dir = Some(r.decision.dir);
                config.memory.pin = r.decision.pin;
                config.memory.note = r.env_note;
            }
            Err(e) => config.memory.unresolved = Some(e),
        }
    }
    Ok(config)
}

/// Current time as RFC 3339 with offset.
fn now_rfc3339() -> String {
    Timestamp::now()
        .to_zoned(TimeZone::system())
        .strftime("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}

/// For a command that uses wiki memory: fails when the directory is unresolved and prints the
/// resolution warning. Returns the config to use.
fn prepare_memory(c: &Config) -> Result<Config, Fail> {
    let c = c.clone();
    c.memory_ready().map_err(Fail::run)?;
    if let Some(note) = &c.memory.note {
        eprintln!("herdr-crew: warning: {note}");
    }
    Ok(c)
}

/// For a writing command (`up`, `mem init`, `startup` adopting): initializes the decided memory
/// directory and only then records the pin, so a refused directory is never pinned. When
/// another process created the pin first, its directory is initialized and used instead.
fn commit_memory(c: &mut Config, wait: Duration) -> Result<memory::InitResult, String> {
    let dir = c.memory_ready()?.expect("wiki mode").to_path_buf();
    let mut init = memory::ensure_initialized(&dir, c, &now_rfc3339(), wait)?;
    if let Some(write) = c.memory.pin.take() {
        let pinned = memory::write_pin(&c.root, &write)?;
        if pinned != dir {
            c.memory.dir = Some(pinned.clone());
            init = memory::ensure_initialized(&pinned, c, &now_rfc3339(), wait)?;
        }
    }
    Ok(init)
}

fn binary() -> PathBuf {
    std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .unwrap_or_else(|_| "herdr-crew".into())
}

fn env(
    c: &Config,
    state: &HerdrState,
    herdr: Option<&dyn Backend>,
    worktrees: BTreeSet<String>,
) -> Env {
    let board_pane = state.workspace.as_ref().and_then(|w| {
        let tab = w.tabs.iter().find(|t| t.label == c.board.tab)?;
        let pane = w.panes.iter().find(|p| p.tab_id == tab.id)?;
        herdr.map(|h| h.foreground(&pane.id))
    });
    let now = now_rfc3339();
    Env {
        agent_cmd: std::env::var("CREW_AGENT_CMD")
            .ok()
            .filter(|s| !s.is_empty()),
        binary: binary(),
        worktrees,
        board_pane,
        resume: herdr.is_some_and(|h| h.resumes_agents()),
        status_exists: c.board_path().exists(),
        current_schema: std::fs::read_to_string(c.root.join(prompt::SCHEMA_FILE)).ok(),
        now,
        windows: cfg!(windows),
    }
}

/// The backend of the host this command drives (see `backend::Host::detect`).
fn host() -> Result<Box<dyn Backend>, Fail> {
    backend::from_env().map_err(|e| Fail {
        code: 2,
        lines: vec![format!("herdr-crew: {e}")],
    })
}

fn fail_notify(herdr: &dyn Backend, msg: String) -> Fail {
    herdr.notify(&msg);
    Fail::run(msg)
}

/// How long `up`, `add` and `close` wait while another command changes the same project.
const LOCK_WAIT: Duration = Duration::from_secs(120);

/// The project's lock for a command that changes it. Taken before reading any state, so the
/// plan always starts from what the previous command left.
fn lock_project(c: &Config, herdr: &dyn Backend) -> Result<ProjectLock, Fail> {
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

/// Whether `up` sets up a project that has no `.herdr/crew.toml` yet, and how.
#[derive(Debug, PartialEq, Eq)]
enum Setup {
    /// The interactive wizard, on a terminal.
    Wizard,
    /// `--yes`: the defaults and the given options, without questions.
    Defaults,
    /// An action, or no terminal to ask in: explain how to set the crew up instead.
    Refuse,
}

fn setup_mode(action: bool, yes: bool, terminal: bool) -> Setup {
    if action {
        Setup::Refuse
    } else if yes {
        Setup::Defaults
    } else if terminal {
        Setup::Wizard
    } else {
        Setup::Refuse
    }
}

/// `up` in a project without a configuration: run the setup wizard, then bring the new crew up.
fn up_new(root: &Path, a: &Args) -> Result<(), Fail> {
    let herdr = host()?;
    let terminal = std::io::IsTerminal::is_terminal(&std::io::stdin());
    match setup_mode(herdr.action(), a.init.yes, terminal) {
        Setup::Refuse if herdr.action() => {
            return Err(fail_notify(
                herdr.as_ref(),
                format!(
                    "no crew is configured in {} ({} is missing). Set one up from a terminal in \
                     the project with `herdr-crew up`, which asks for a workflow and an agent and \
                     then starts the sessions, or `herdr-crew up --yes` for the defaults; then \
                     run this action again",
                    root.display(),
                    config::CONFIG_FILE
                ),
            ));
        }
        Setup::Refuse => {
            return Err(Fail::run(format!(
                "no crew is configured in {} ({} is missing) and there is no terminal to ask in. \
                 Run `herdr-crew up` in a terminal to choose a workflow, or pass --yes to set up \
                 the defaults (a solo crew; see --preset, --name, --agent and --base)",
                root.display(),
                config::CONFIG_FILE
            )));
        }
        Setup::Wizard | Setup::Defaults => {}
    }
    if !init::setup(root, &a.init, a.dry_run, init::Purpose::Up)? {
        return Ok(());
    }
    let c = load_config(root)?;
    println!();
    // The wizard already saved the selected agent; nothing to switch.
    up(&c, a, None)
}

/// The agents of a crew, for the opening line: `claude`, `codex + claude`…
fn agent_kinds(c: &Config) -> String {
    let mut kinds: Vec<&str> = Vec::new();
    for role in &c.roles {
        if !kinds.contains(&role.kind.as_str()) {
            kinds.push(role.kind.as_str());
        }
    }
    kinds.join(" + ")
}

/// What a verdict means once `up` has run, for the closing summary.
fn outcome(verdict: &plan::Verdict, board: bool) -> String {
    match (verdict, board) {
        (plan::Verdict::Create, false) => "started".into(),
        (plan::Verdict::Create, true) => "board tab created".into(),
        (plan::Verdict::Repair, _) => {
            "board checked (viewer restarted if its shell was idle)".into()
        }
        (plan::Verdict::Occupied(why), _) => format!("already running ({why})"),
        (plan::Verdict::NoAgent, _) => "tab without an agent: see the warning above".into(),
        (plan::Verdict::Resume(_), _) => "resumed its conversation".into(),
        (plan::Verdict::LeaveAlone(why), _) => format!("left alone ({why})"),
    }
}

/// The closing summary of `up`: one line per session and the board, then what to do next.
/// `open` says how to see the crew, when `up` does not attach to it.
fn summary(c: &Config, p: &plan::Plan, open: Option<&str>) -> String {
    let width = p.verdicts.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    let mut out = format!("herdr-crew: crew \"{}\":\n", c.label);
    for (name, verdict) in &p.verdicts {
        out.push_str(&format!(
            "  {name:<width$}  {}\n",
            outcome(verdict, *name == c.board.tab)
        ));
    }
    let started = p
        .verdicts
        .iter()
        .any(|(n, v)| *v == plan::Verdict::Create && *n != c.board.tab);
    if started {
        out.push_str(
            "herdr-crew: the new sessions wait for a task: give one in a role's tab. The first \
             time an agent runs in a folder it may ask whether you trust it; answer in its tab.\n",
        );
    }
    if let Some(open) = open {
        out.push_str(&format!("herdr-crew: to see them, {open}.\n"));
    }
    out
}

/// What to try when `up` fails, appended to its error.
fn up_hints(c: &Config, action: bool) -> Vec<String> {
    let mut hints = vec![
        "herdr-crew: troubleshooting:".to_string(),
        "  - `herdr-crew check` validates the configuration, herdr, git and the agents".into(),
        "  - `herdr-crew up --dry-run` shows each role's verdict and the steps up would take"
            .into(),
        "  - up is safe to run again: it only creates what is still missing".into(),
    ];
    if c.roles.iter().any(|r| r.kind == agent::Kind::Codex) {
        hints.push(
            "  - Codex roles need `herdr-crew codex-install` and the crew hook trusted in Codex \
             /hooks"
                .into(),
        );
    }
    if c.roles.iter().any(|r| r.kind == agent::Kind::Pi) {
        hints.push(
            "  - pi roles need `herdr-crew pi-install` (again after updating herdr-crew)".into(),
        );
    }
    if c.roles.iter().any(|r| r.kind == agent::Kind::Agy) {
        hints.push(
            "  - agy roles need `herdr-crew agy-install` (and `herdr integration install \
             antigravity-cli` for restore)"
                .into(),
        );
    }
    if c.worktrees.is_some() {
        hints.push(
            "  - new worktrees start from worktrees.base: push what the crew needs and check \
             that the remote is reachable"
                .into(),
        );
    }
    if action {
        hints.push(
            "  - this action's full output is in `herdr plugin log list --plugin herdr-crew`"
                .into(),
        );
    }
    hints
}

fn up(c: &Config, a: &Args, select: Option<agent::Kind>) -> Result<(), Fail> {
    let herdr = host()?;
    up_inner(c, a, select, herdr.as_ref()).map_err(|mut f| {
        // A usage or configuration error already says what to change.
        if f.code == 1 {
            f.lines.extend(up_hints(c, herdr.action()));
        }
        f
    })
}

fn up_inner(
    c: &Config,
    a: &Args,
    select: Option<agent::Kind>,
    herdr: &dyn Backend,
) -> Result<(), Fail> {
    let lock = if a.dry_run {
        None
    } else {
        Some(lock_project(c, herdr)?)
    };
    let selection = select
        .map(|kind| {
            let path = c.root.join(config::CONFIG_FILE);
            files::validate(&c.root, &path, files::Kind::File).map_err(Fail::run)?;
            let text = std::fs::read_to_string(&path).map_err(|e| Fail::run(e.to_string()))?;
            let (mut selected, updated) =
                Config::select_agent(&text, &c.root, kind).map_err(|errors| Fail {
                    code: 2,
                    lines: errors.iter().map(ToString::to_string).collect(),
                })?;
            // `select_agent` parses again; keep what `load_config` resolved.
            selected.memory = c.memory.clone();
            Ok::<_, Fail>((selected, (text != updated).then_some(updated)))
        })
        .transpose()?;
    let c = selection.as_ref().map_or(c, |(selected, _)| selected);
    // Memory is resolved, pinned and initialized before herdr starts, so every role and the
    // board get the directory that the pin records.
    let mut prepared;
    let mut memory_init = None;
    let c = if c.uses_wiki_memory() {
        prepared = prepare_memory(c).inspect_err(|f| herdr.notify(&f.lines.join("\n")))?;
        if !a.dry_run {
            memory_init = Some(
                commit_memory(&mut prepared, memory::LOCK_WAIT)
                    .map_err(|err| fail_notify(herdr, err))?,
            );
        }
        &prepared
    } else {
        c
    };
    let worktrees = git::existing_worktrees(c).map_err(Fail::run)?;
    let mut out = Output;
    if let Some(init) = &memory_init
        && (!init.created_files.is_empty() || !init.refreshed_files.is_empty())
    {
        out.step(&format!(
            "operational memory ready in {}",
            init.dir.display()
        ));
    }
    if !a.dry_run {
        let roles = c.roles.len();
        out.step(&format!(
            "bringing up \"{}\" from {}: {roles} role{} ({}) and the board tab \"{}\"",
            c.label,
            c.root.display(),
            if roles == 1 { "" } else { "s" },
            agent_kinds(c),
            c.board.tab
        ));
    }
    let mode = if a.dry_run {
        Load::Read
    } else {
        Load::Start(&c.root, &mut out)
    };
    let state = match herdr.load(c, mode) {
        Ok(state) => state,
        Err(LoadError::Unreachable { .. }) if a.dry_run => {
            println!("(no herdr server: the plan assumes an empty one)");
            HerdrState::default()
        }
        Err(e) => return Err(fail_notify(herdr, e.to_string())),
    };
    let e = env(c, &state, Some(herdr), worktrees);
    let mut p = plan::plan(c, &state, &e);
    if let Some((_, Some(content))) = &selection {
        p.steps.insert(
            0,
            plan::Step::SetAgent {
                path: c.root.join(config::CONFIG_FILE),
                kind: c.kind,
                content: content.clone(),
            },
        );
    }
    if a.dry_run {
        print!("{}", p.dry_run());
        if let Some(dir) = c.wiki_dir() {
            println!("memory: initialize/refresh {}", dir.display());
        }
        return Ok(());
    }
    let ws = herdr::execute(herdr, c, &state, &p, &mut out).map_err(|e| fail_notify(herdr, e))?;
    if p.actions().next().is_none() {
        out.step("everything is already up; nothing to do");
    }
    herdr.tidy(c);
    let attaching = ws.is_some() && herdr.will_attach(!a.no_attach);
    let open = (!attaching).then(|| herdr.open_hint(&c.label, &c.root));
    print!("{}", summary(c, &p, open.as_deref()));
    let started = p
        .verdicts
        .iter()
        .filter(|(n, v)| *v == plan::Verdict::Create && *n != c.board.tab)
        .count();
    if started > 0 {
        herdr.notify(&format!(
            "crew \"{}\": started {started} session{}",
            c.label,
            if started == 1 { "" } else { "s" }
        ));
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
    let mut c = load_config(root)?;
    let mut out = Output;
    let watching = adoption.is_ok();
    // On a restore the memory problem is only logged (a restore stays silent) and the board is
    // still repaired; when herdr adopts the project agents would start on it, so it is fatal.
    if let Err(why) = c.memory_ready() {
        if watching {
            return Err(Fail::run(why));
        }
        eprintln!(
            "herdr-crew: {}: warning: operational memory unavailable ({why}); leaving the \
             project to `herdr-crew up`",
            c.label
        );
        return Ok(());
    }
    // Adopting starts agents on the directory, so it is a writer like `up`: initialize, then
    // pin, before planning. A short wait covers a `mem mcp --write` writing at that moment.
    if watching && c.uses_wiki_memory() {
        commit_memory(&mut c, Duration::from_secs(1)).map_err(Fail::run)?;
    }
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
    let e = env(&c, &state, Some(herdr), worktrees);
    let p = plan::plan(&c, &state, &e);
    // What startup saw, for the plugin log.
    print!("{}", startup_verdicts(&c.label, &p));
    // A restore repairs a recorded memory directory, never waiting on its lock. Without a pin
    // there is no recorded directory, so it creates none (the next `up` chooses and pins it).
    if !watching
        && c.memory.pin.is_none()
        && let Some(dir) = c.wiki_dir()
        && let Err(why) = memory::ensure_initialized(dir, &c, &e.now, Duration::ZERO)
    {
        eprintln!("herdr-crew: {}: warning: {why}", c.label);
    }
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

fn current_state(c: &Config, herdr: &dyn Backend) -> Result<HerdrState, Fail> {
    herdr
        .load(c, Load::Read)
        .map_err(|e| Fail::run(e.to_string()))
}

fn add(c: &Config, role: &str) -> Result<(), Fail> {
    // The new instance's prompt and `--add-dir` need the memory directory.
    let prepared;
    let c = if c.uses_wiki_memory() {
        prepared = prepare_memory(c)?;
        &prepared
    } else {
        c
    };
    let herdr = host()?;
    let herdr = herdr.as_ref();
    let _lock = lock_project(c, herdr)?;
    let worktrees = git::existing_worktrees(c).map_err(Fail::run)?;
    let state = current_state(c, herdr)?;
    let (name, p) =
        plan::plan_add(c, &state, &env(c, &state, None, worktrees), role).map_err(Fail::run)?;
    let mut out = Output;
    herdr::execute(herdr, c, &state, &p, &mut out).map_err(|e| fail_notify(herdr, e))?;
    herdr.tidy(c);
    out.step(&format!("new instance {name}"));
    Ok(())
}

fn close(c: &Config, name: &str) -> Result<(), Fail> {
    let herdr = host()?;
    let herdr = herdr.as_ref();
    let _lock = lock_project(c, herdr)?;
    let state = current_state(c, herdr)?;
    let close = plan::plan_close(c, &state, name).map_err(Fail::run)?;
    herdr
        .close_tab(&close.tab_id)
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
    if a.file.is_none() {
        c.memory_ready().map_err(Fail::run)?;
    }
    let path = a.file.clone().unwrap_or_else(|| c.effective_board_path());
    let result = if a.once {
        board::tui::once(&path, c)
    } else {
        board::tui::run(&path, c, a.interval.unwrap_or(Duration::from_millis(1500)))
    };
    result.map_err(|e| Fail::run(format!("board: {e}")))
}

fn mem(c: &Config, a: &Args) -> Result<(), Fail> {
    if !c.uses_wiki_memory() {
        return Err(Fail::usage(format!(
            "mem needs [memory] mode = \"wiki\" in {}; this project uses .herdr/status.json",
            config::CONFIG_FILE
        )));
    }
    let sub = a.positional[0].as_str();
    let mut c = prepare_memory(c)?;
    // Only `mem init` records the pin; `mem mcp --write` initializes but does not choose.
    let init = if sub == "init" {
        Some(commit_memory(&mut c, memory::LOCK_WAIT).map_err(Fail::run)?)
    } else {
        None
    };
    let c = &c;
    let dir = c
        .wiki_dir()
        .expect("resolved by prepare_memory")
        .to_path_buf();
    match sub {
        "path" => {
            println!("{}", dir.display());
            Ok(())
        }
        "init" => {
            let result = init.expect("initialized above");
            println!("herdr-crew: memory directory {}", result.dir.display());
            if !result.created_files.is_empty() {
                println!("  created: {}", result.created_files.join(", "));
            }
            if !result.refreshed_files.is_empty() {
                println!("  refreshed: {}", result.refreshed_files.join(", "));
            }
            Ok(())
        }
        "lint" => {
            let report = memory::lint(&dir, c, &TimeZone::system());
            if report.is_ok() {
                println!(
                    "herdr-crew: memory lint ok ({} pages, {} links, {} log entries)",
                    report.pages_checked, report.links_checked, report.log_entries_checked
                );
                Ok(())
            } else {
                Err(Fail {
                    code: 1,
                    lines: report
                        .issues
                        .into_iter()
                        .map(|issue| format!("herdr-crew: {issue}"))
                        .collect(),
                })
            }
        }
        "index" => {
            print!("{}", memory::read_index(&dir).map_err(Fail::run)?);
            Ok(())
        }
        "timeline" => {
            let limit = a.limit.unwrap_or(20);
            print!(
                "{}",
                memory::read_timeline(&dir, limit, &TimeZone::system()).map_err(Fail::run)?
            );
            Ok(())
        }
        "search" => {
            let hits = memory::search(&dir, &a.positional[1]).map_err(Fail::run)?;
            for hit in hits {
                println!("{hit}");
            }
            Ok(())
        }
        "show" => {
            print!(
                "{}",
                memory::show(&dir, &a.positional[1]).map_err(Fail::run)?
            );
            Ok(())
        }
        "mcp" => mcp::serve(c, dir, a.write).map_err(Fail::run),
        _ => unreachable!("validated in parse_args"),
    }
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
    match c.memory_ready() {
        Ok(Some(dir)) => {
            println!("herdr-crew: operational memory in {}", dir.display());
            if let Some(note) = &c.memory.note {
                println!("herdr-crew: warning: {note}");
            }
        }
        Ok(None) => {}
        Err(why) => missing.push(format!("herdr-crew: operational memory: {why}")),
    }
    let host = backend::Host::from_env().map_err(|e| Fail {
        code: 2,
        lines: vec![format!("herdr-crew: {e}")],
    })?;
    if host == backend::Host::Core {
        check_core(&mut missing);
    }
    let herdr_bin = std::env::var_os("HERDR_BIN_PATH");
    for (program, install) in installers {
        if program == "herdr" && host == backend::Host::Core {
            continue;
        }
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
        agent::Kind::Agy => Some("agy"),
        agent::Kind::Claude => None,
    });
    if native.is_some()
        && host == backend::Host::Herdr
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
                // Same options `up` launches with, including the wiki memory `--add-dir`.
                let mut options = role.codex.clone();
                if let Some(dir) = c.wiki_dir() {
                    options.add_extra_dir(dir.to_path_buf());
                }
                if let Err(error) =
                    codex::integration::preflight(&cwd, &options, &c.root, c.codex_tree)
                {
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
    if c.roles.iter().any(|r| r.kind == agent::Kind::Agy) {
        match executable("agy", None) {
            Err(why) => missing.push(format!(
                "herdr-crew: agy is missing: {why} (install Antigravity CLI from https://antigravity.google)"
            )),
            Ok(path) => match agy::require_version(&path) {
                Ok((a, b, v)) => println!("herdr-crew: agy {a}.{b}.{v} at {}", path.display()),
                Err(e) => missing.push(format!("herdr-crew: {e}")),
            },
        }
        match agy::preflight() {
            Ok(()) => println!("herdr-crew: the crew agy hook is installed"),
            Err(error) => missing.push(format!("herdr-crew: {error}")),
        }
        let trust = match agy::TrustedWorkspaces::read() {
            Ok(trust) => Some(trust),
            Err(error) => {
                missing.push(format!("herdr-crew: {error}"));
                None
            }
        };
        for role in c.roles.iter().filter(|r| r.kind == agent::Kind::Agy) {
            if let Some(trust) = &trust
                && let Err(error) = trust.require(c, &role.name)
            {
                missing.push(format!("herdr-crew: {error}"));
            }
            let mut effective = role.agy.clone();
            if let Err(error) = effective.resolve_dirs(&c.root) {
                missing.push(format!("herdr-crew: {}: {error}", role.name));
            }
            let longest = format!("{}-99", role.name);
            let names = std::iter::once(role.name.as_str()).chain(role.extra.then_some(&*longest));
            for name in names {
                if let Some(problem) =
                    agy::prompt_problem(name, &prompt::render(c, name, &binary()))
                {
                    missing.push(format!("herdr-crew: {problem}"));
                }
            }
        }
    }
    if host == backend::Host::Herdr {
        let herdr = Herdr::from_env();
        let path = plan::shell_program(&binary().to_string_lossy(), cfg!(windows));
        match herdr.call(&["workspace", "list"]) {
            Ok(_) => {
                if let Some(kind) = native
                    && let Err(error) = herdr.require_server_version(kind)
                {
                    missing.push(format!("herdr-crew: {error}"));
                }
                println!(
                    "herdr-crew: a herdr server is already running; `herdr` only attaches and does not \
                     run plugin startup. From this project, run `{path} up --no-attach`"
                );
            }
            Err(e) if e.socket_denied() => println!(
                "herdr-crew: crew messaging unavailable from this process: Herdr access denied ({e}). \
                 Use `{path} send --check <session>` from this project in an operator terminal \
                 that already has access, or relay the message in the recipient's pane."
            ),
            Err(_) => {}
        }
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

/// `check`'s herdr-core half: `hc` runs.
fn check_core(missing: &mut Vec<String>) {
    let hc = std::env::var_os("CREW_HC_BIN").unwrap_or_else(|| "hc".into());
    match executable(&hc.to_string_lossy(), None) {
        Ok(path) => println!(
            "herdr-crew: host herdr-core ({}): crews show in the Herdr app",
            path.display()
        ),
        Err(why) => missing.push(format!(
            "herdr-crew: herdr-core is missing: {why} (install it with: curl -fsSL \
             https://releases.herdr.dev/core/install.sh | sh)"
        )),
    }
}

/// `send`: the message from stdin into another session (docs/messaging.md §4). Its refusals use
/// their own exit codes and no usage text, because the caller is usually a model.
fn send(c: &Config, target: &str, check: bool) -> Result<(), Fail> {
    let raw = if check {
        None
    } else {
        Some(send::read_input(std::io::stdin().lock()).map_err(|e| Fail {
            code: 2,
            lines: vec![format!("herdr-crew: {e}")],
        })?)
    };
    let herdr = host()?;
    // The sender's own pane: a herdr pane, or a herdr-core terminal address.
    let pane = match backend::Host::from_env() {
        Ok(backend::Host::Core) => std::env::var("HC_TERMINAL_ID").ok(),
        _ => std::env::var("HERDR_PANE_ID").ok(),
    };
    match send::run(c, herdr.as_ref(), target, raw.as_deref(), pane.as_deref()) {
        Ok(line) => {
            println!("{line}");
            Ok(())
        }
        Err(r) => Err(Fail {
            code: r.code,
            lines: vec![format!("herdr-crew: {}", r.message)],
        }),
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
        "agy-install" | "agy-uninstall" => {
            if a.root.is_some() {
                return Err(Fail::usage("agy hook setup does not take --root"));
            }
            return agy::install(a.command == "agy-uninstall").map_err(Fail::run);
        }
        "app-install" | "app-uninstall" => {
            if a.root.is_some() {
                return Err(Fail::usage("Herdr app mod setup does not take --root"));
            }
            return app::install(a.command == "app-uninstall", &binary()).map_err(Fail::run);
        }
        "agy-hook" => {
            agy::hook();
            return Ok(());
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
    if a.command == "up" {
        let path = root.join(config::CONFIG_FILE);
        if matches!(std::fs::symlink_metadata(&path), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
        {
            return up_new(&root, &a);
        }
        let i = &a.init;
        if i.preset.is_some()
            || i.topology.is_some()
            || i.models.is_some()
            || i.no_tui
            || i.name.is_some()
            || i.base.is_some()
            || i.shared_checkout
            || i.no_ignore
        {
            return Err(Fail::usage(format!(
                "--preset, --topology, --models, --name, --base, --shared-checkout, --no-ignore \
                 and --no-tui only apply when up sets up a new crew; {} already exists, so edit \
                 it instead",
                path.display()
            )));
        }
    }
    let c = load_config(&root)?;
    match a.command.as_str() {
        "up" => up(&c, &a, a.agent),
        "add" => add(&c, &a.positional[0]),
        "close" => close(&c, &a.positional[0]),
        "send" => send(&c, &a.positional[0], a.send_check),
        "board" => board(&c, &a),
        "mem" => mem(&c, &a),
        "codex-peers" => {
            println!("{}", codex::peers(&c).map_err(Fail::run)?);
            Ok(())
        }
        "check" => check(&c),
        "config" => {
            if !tui::interactive() {
                return Err(Fail::run(format!(
                    "config needs a terminal; edit {} directly",
                    c.root.join(config::CONFIG_FILE).display()
                )));
            }
            tui::editor::run(&c.root).map_err(Fail::run)
        }
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
        for kind in agent::Kind::ALL {
            let selected = args(&format!("up --agent {} --dry-run", kind.as_str())).unwrap();
            assert_eq!(selected.agent, Some(kind));
            assert_eq!(
                args(&format!("init --agent {}", kind.as_str()))
                    .unwrap()
                    .init
                    .agent,
                Some(kind)
            );
        }
        let a = args("board --file /f --once --interval 2").unwrap();
        assert!(a.once && a.interval == Some(Duration::from_secs(2)));
        // Without a configuration, up sets one up first and takes init's options for it.
        let up =
            args("up --preset review --name acme --agent codex --topology tree --yes --no-attach")
                .unwrap();
        assert_eq!(up.init.preset, Some(init::Preset::Review));
        assert_eq!(up.init.agent, Some(agent::Kind::Codex));
        assert_eq!(up.init.topology, Some(init::Topology::Tree));
        assert!(up.init.yes && up.no_attach);
        assert_eq!(
            args("init --topology tabs").unwrap().init.topology,
            Some(init::Topology::Tabs)
        );
        assert_eq!(args("mem path").unwrap().positional, ["path"]);
        assert_eq!(args("mem init").unwrap().positional, ["init"]);
        assert_eq!(args("mem lint").unwrap().positional, ["lint"]);
        assert_eq!(args("mem index").unwrap().positional, ["index"]);
        let tl = args("mem timeline --limit 5").unwrap();
        assert_eq!(tl.positional, ["timeline"]);
        assert_eq!(tl.limit, Some(5));
        assert_eq!(
            args("mem search frontmatter").unwrap().positional,
            ["search", "frontmatter"]
        );
        assert_eq!(
            args("mem show tasks/poc").unwrap().positional,
            ["show", "tasks/poc"]
        );
        let mcp_default = args("mem mcp").unwrap();
        assert_eq!(mcp_default.positional, ["mcp"]);
        assert!(!mcp_default.write, "read-only by default");
        let mcp_ro = args("mem mcp --read-only").unwrap();
        assert_eq!(mcp_ro.positional, ["mcp"]);
        assert!(mcp_ro.read_only && !mcp_ro.write);
        let mcp_rw = args("mem mcp --write").unwrap();
        assert!(mcp_rw.write);
        assert!(args("mem mcp --read-only --write").is_err());
        assert!(args("mem index --write").is_err());
    }

    #[test]
    fn up_sets_up_only_where_someone_can_answer() {
        assert_eq!(setup_mode(false, false, true), Setup::Wizard);
        assert_eq!(setup_mode(false, true, false), Setup::Defaults);
        assert_eq!(setup_mode(false, true, true), Setup::Defaults);
        // A script without --yes would hang on the questions, and an action has no terminal.
        assert_eq!(setup_mode(false, false, false), Setup::Refuse);
        assert_eq!(setup_mode(true, true, true), Setup::Refuse);
    }

    #[test]
    fn up_summarizes_what_happened_and_how_to_see_it() {
        let root = Path::new("/repo");
        let c = Config::parse(
            "version = 1\n[workspace]\nlabel = \"acme\"\n[board]\ntab = \"acme-status\"\nwriter = \"acme-lead\"\n\
             [[roles]]\nname = \"acme-lead\"\nprompt = \"Lead.\"\n[[roles]]\nname = \"acme-dev\"\nprompt = \"Dev.\"\nkind = \"codex\"\n",
            root,
        )
        .unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(agent_kinds(&c), "claude + codex");
        let p = plan::Plan {
            verdicts: vec![
                ("acme-lead".into(), plan::Verdict::Create),
                (
                    "acme-dev".into(),
                    plan::Verdict::Occupied("agent in w1:p1".into()),
                ),
                ("acme-status".into(), plan::Verdict::Create),
            ],
            steps: Vec::new(),
        };
        let text = summary(&c, &p, Some("run `herdr`"));
        assert!(
            text.starts_with("herdr-crew: crew \"acme\":\n  acme-lead    started\n"),
            "{text}"
        );
        assert!(
            text.contains("acme-dev     already running (agent in w1:p1)"),
            "{text}"
        );
        assert!(text.contains("acme-status  board tab created"), "{text}");
        assert!(text.contains("wait for a task"), "{text}");
        assert!(text.contains("to see them"), "{text}");
        let attached = summary(&c, &p, None);
        assert!(!attached.contains("to see them"), "{attached}");
        let hints = up_hints(&c, true).join("\n");
        for hint in [
            "herdr-crew check",
            "--dry-run",
            "codex-install",
            "plugin log list",
        ] {
            assert!(hints.contains(hint), "{hint}: {hints}");
        }
        assert!(!hints.contains("pi-install"), "{hints}");
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
            "up --agent",
            "up --agent unknown",
            "check --agent codex",
            "add dev --agent codex",
            "board --file",
            "startup now",
            "startup --dry-run",
            "init --preset unknown",
            "init --topology",
            "init --topology mesh",
            "up --topology unknown",
            "check --topology tree",
            "board --topology tabs",
            "init --base",
            "up --preset unknown",
            "check --name acme",
            "board --yes",
            "mem",
            "mem unknown",
            "mem path extra",
            "mem search",
            "mem show",
            "mem path --limit 5",
            "mem timeline --limit 0",
            "mem timeline --limit abc",
            "up --limit 5",
            "up --read-only",
            "board --read-only",
            "mem path --read-only",
            "mem mcp extra",
            "mem mcp --limit 5",
        ] {
            assert_eq!(args(bad).err(), Some(2), "{bad}");
        }
    }
}
