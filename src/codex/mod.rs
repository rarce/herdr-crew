//! Codex role context and configured restoration through Herdr's native session protocol.

pub mod integration;

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{agent::CodexOptions, config::Config, files, git, herdr::Herdr};

pub const RUNTIME: &str = ".herdr/codex";
pub const MAX_CONTEXT: usize = 64 * 1024;
const FORMAT: u32 = 1;

pub fn validate_runtime(root: &Path) -> Result<(), String> {
    let runtime = root.join(RUNTIME);
    files::validate(root, &runtime, files::Kind::Directory)?;
    files::validate(root, &runtime.join("lock"), files::Kind::Directory)?;
    for name in ["snapshots", "bindings", "sessions", "claims"] {
        let directory = runtime.join(name);
        files::validate(root, &directory, files::Kind::Directory)?;
        if directory.exists() {
            for entry in fs::read_dir(&directory).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                files::validate(root, &entry.path(), files::Kind::File)?;
            }
        }
    }
    Ok(())
}

pub fn pane_environment() -> Result<Vec<String>, String> {
    Ok(vec![
        "--env".into(),
        format!("CODEX_HOME={}", integration::home()?.display()),
        "--env".into(),
        "CODEX_THREAD_ID=".into(),
        "--env".into(),
        "CREW_CODEX_BINDING=".into(),
    ])
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Context {
    socket: String,
    workspace: String,
    tab: String,
    pane: String,
    terminal: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u32,
    token: String,
    root: PathBuf,
    cwd: PathBuf,
    name: String,
    workspace: String,
    prompt: String,
    options: CodexOptions,
    codex_home: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Binding {
    context: Context,
    token: String,
}

#[derive(Serialize, Deserialize)]
struct Session {
    id: String,
    context: Context,
}

fn token_valid(token: &str) -> bool {
    token.len() == 32
        && token
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn session_valid(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn path(root: &Path, directory: &str, key: &str) -> PathBuf {
    root.join(RUNTIME)
        .join(directory)
        .join(format!("{key}.json"))
}

fn read<T: for<'de> Deserialize<'de>>(root: &Path, path: &Path) -> Result<T, String> {
    files::validate(root, path, files::Kind::File)?;
    let file = File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut text = String::new();
    file.take(512 * 1024 + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() > 512 * 1024 {
        return Err("Codex runtime artifact is too large".into());
    }
    serde_json::from_str(&text)
        .map_err(|e| format!("invalid Codex artifact {}: {e}", path.display()))
}

fn write(root: &Path, path: &Path, value: &impl Serialize) -> Result<(), String> {
    files::write(
        root,
        path,
        &serde_json::to_string(value).map_err(|e| e.to_string())?,
    )
}

struct Lock(PathBuf);
impl Lock {
    fn acquire(root: &Path) -> Result<Self, String> {
        let path = root.join(RUNTIME).join("lock");
        files::validate(root, &path, files::Kind::Directory)?;
        let start = Instant::now();
        loop {
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e)
                    if e.kind() == std::io::ErrorKind::AlreadyExists
                        && start.elapsed() < Duration::from_secs(3) =>
                {
                    sleep(Duration::from_millis(50))
                }
                Err(e) => {
                    return Err(format!(
                        "Codex runtime lock {}: {e}; if a previous helper crashed, remove its empty lock directory",
                        path.display()
                    ));
                }
            }
        }
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

/// Two stable hashes produce short filenames; the stored full context is always compared.
fn context_key(context: &Context) -> String {
    let bytes = serde_json::to_vec(context).unwrap();
    let hash = |seed: u64| {
        bytes
            .iter()
            .fold(seed, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x100000001b3))
    };
    format!(
        "{:016x}{:016x}",
        hash(0xcbf29ce484222325),
        hash(0x84222325cbf29ce4)
    )
}

fn new_token() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|e| format!("cannot create Codex binding token: {e}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn text(value: &Value, key: &str) -> Result<String, String> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("Herdr did not return {key}"))
}

fn pane_context(value: &Value, socket: String) -> Result<Context, String> {
    let pane = &value["pane"];
    Ok(Context {
        socket,
        workspace: text(pane, "workspace_id")?,
        tab: text(pane, "tab_id")?,
        pane: text(pane, "pane_id")?,
        terminal: text(pane, "terminal_id")?,
    })
}

pub fn context_prompt(c: &Config, name: &str, rendered: &str) -> Result<String, String> {
    let context = format!(
        "{rendered}\nCrew runtime paths: the board is {} and its schema is {}. Only {} writes the board. Paths in the role instructions retain their documented meaning relative to the main checkout {}.\n",
        c.board_path().display(),
        c.root.join(crate::prompt::SCHEMA_FILE).display(),
        c.board.writer,
        c.root.display()
    );
    if context.len() > MAX_CONTEXT {
        return Err(format!(
            "Codex role {name}: rendered context is {} bytes; maximum is {MAX_CONTEXT}",
            context.len()
        ));
    }
    Ok(context)
}

pub fn prepare(
    herdr: &Herdr,
    c: &Config,
    name: &str,
    pane: &str,
    rendered: &str,
    options: &CodexOptions,
) -> Result<(), String> {
    let status = herdr
        .call(&["status", "server", "--json"])
        .map_err(|e| e.to_string())?;
    let version = status["version"]
        .as_str()
        .ok_or("Herdr did not return a server version")?;
    if !supported_herdr(version) {
        return Err("Codex crews require Herdr server 0.9.3 or newer".into());
    }
    let result = herdr
        .call(&["pane", "get", pane])
        .map_err(|e| e.to_string())?;
    let context = pane_context(&result, text(&status, "socket")?)?;
    let role = c.session_role(name).ok_or("unknown role")?;
    let cwd = if role.worktree {
        c.worktree_path(name).ok_or("missing worktree")?
    } else {
        c.root.clone()
    };
    git::verify_checkout(&c.root, &cwd)?;
    let mut options = options.clone();
    options.resolve_dirs(&c.root)?;
    let snapshot = Snapshot {
        version: FORMAT,
        token: new_token()?,
        root: c.root.clone(),
        cwd,
        name: name.into(),
        workspace: c.label.clone(),
        prompt: context_prompt(c, name, rendered)?,
        options,
        codex_home: integration::home()?,
    };
    write(
        &c.root,
        &path(&c.root, "snapshots", &snapshot.token),
        &snapshot,
    )?;
    let _lock = Lock::acquire(&c.root)?;
    bind(&snapshot, context)?;
    Ok(())
}

pub fn supported_herdr(value: &str) -> bool {
    let parts: Vec<_> = value.split('.').map(str::parse::<u32>).collect();
    matches!(parts.as_slice(), [Ok(a),Ok(b),Ok(c)] if (*a,*b,*c) >= (0,9,3))
}

fn bind(snapshot: &Snapshot, context: Context) -> Result<(), String> {
    write(
        &snapshot.root,
        &path(&snapshot.root, "bindings", &context_key(&context)),
        &Binding {
            context,
            token: snapshot.token.clone(),
        },
    )
}

fn snapshot(root: &Path, token: &str) -> Result<Snapshot, String> {
    if !token_valid(token) {
        return Err("invalid Codex binding token".into());
    }
    let snapshot: Snapshot = read(root, &path(root, "snapshots", token))?;
    if snapshot.version != FORMAT
        || snapshot.token != token
        || snapshot.root != root
        || snapshot.prompt.len() > MAX_CONTEXT
        || !crate::config::is_agent_name(&snapshot.name)
        || !snapshot.options.problems().is_empty()
        || !snapshot.codex_home.is_absolute()
    {
        return Err("Codex snapshot is incompatible with this checkout or crew version".into());
    }
    git::verify_checkout(root, &snapshot.cwd)?;
    Ok(snapshot)
}

#[cfg(unix)]
fn rpc(context: &Context, method: &str, params: Value) -> Result<Value, String> {
    use std::os::unix::net::UnixStream;
    let mut stream =
        UnixStream::connect(&context.socket).map_err(|e| format!("Herdr socket: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    writeln!(
        stream,
        "{}",
        json!({"id":"herdr-crew-codex","method":method,"params":params})
    )
    .map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(stream)
        .take(2 * 1024 * 1024)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let value: Value =
        serde_json::from_str(&line).map_err(|e| format!("invalid Herdr response: {e}"))?;
    if let Some(error) = value.get("error") {
        return Err(format!("{}: {}", error["code"], error["message"]));
    }
    value
        .get("result")
        .cloned()
        .ok_or("Herdr response has no result".into())
}

#[cfg(not(unix))]
fn rpc(_: &Context, _: &str, _: Value) -> Result<Value, String> {
    Err("Codex crews currently require macOS or Linux".into())
}

fn inherited_context() -> Result<Option<Context>, String> {
    if std::env::var("HERDR_ENV").ok().as_deref() != Some("1") {
        return Ok(None);
    }
    let env = |name| {
        std::env::var(name).map_err(|_| format!("missing {name} in the Codex hook environment"))
    };
    let context = Context {
        socket: env("HERDR_SOCKET_PATH")?,
        workspace: env("HERDR_WORKSPACE_ID")?,
        tab: env("HERDR_TAB_ID")?,
        pane: env("HERDR_PANE_ID")?,
        terminal: String::new(),
    };
    let pane = rpc(&context, "pane.get", json!({"pane_id":context.pane}))?;
    Ok(Some(pane_context(&pane, context.socket)?))
}

fn validate_live(snapshot: &Snapshot, context: &Context, cwd: &Path) -> Result<(), String> {
    if cwd.canonicalize().map_err(|e| e.to_string())?
        != snapshot.cwd.canonicalize().map_err(|e| e.to_string())?
    {
        return Err("Codex hook working directory does not match its role snapshot".into());
    }
    git::verify_checkout(&snapshot.root, cwd)?;
    let workspaces = rpc(context, "workspace.list", json!({}))?;
    let workspace = workspaces["workspaces"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|w| w["workspace_id"] == context.workspace)
        })
        .ok_or("crew workspace is missing")?;
    if workspace["label"] != snapshot.workspace {
        return Err("crew workspace was renamed or belongs to another project".into());
    }
    let tabs = rpc(
        context,
        "tab.list",
        json!({"workspace_id":context.workspace}),
    )?;
    let tab = tabs["tabs"]
        .as_array()
        .and_then(|items| items.iter().find(|t| t["tab_id"] == context.tab))
        .ok_or("crew tab is missing")?;
    if tab["label"] != snapshot.name {
        return Err("crew tab was renamed or belongs to another role".into());
    }
    Ok(())
}

#[derive(Deserialize)]
struct HookInput {
    hook_event_name: String,
    session_id: String,
    cwd: PathBuf,
    source: String,
}

fn process_hook(input: HookInput) -> Result<Option<String>, String> {
    if input.hook_event_name != "SessionStart" {
        return Ok(None);
    }
    if std::env::var("CODEX_THREAD_ID")
        .ok()
        .is_some_and(|id| !id.is_empty() && id != input.session_id)
    {
        return Ok(None);
    }
    if !matches!(
        input.source.as_str(),
        "startup" | "resume" | "clear" | "compact"
    ) || !session_valid(&input.session_id)
    {
        return Err("unsupported Codex session event or invalid session ID".into());
    }
    let Some(context) = inherited_context()? else {
        return Ok(None);
    };
    let root = match git::main_root(&input.cwd) {
        Ok(root) => root,
        Err(_) => return Ok(None),
    };
    let binding_path = path(&root, "bindings", &context_key(&context));
    let explicit = std::env::var("CREW_CODEX_BINDING")
        .ok()
        .filter(|token| !token.is_empty());
    if !binding_path.exists() && explicit.is_none() {
        return Ok(None);
    }
    let _lock = Lock::acquire(&root)?;
    let binding: Binding = read(&root, &binding_path)?;
    if binding.context != context
        || explicit
            .as_ref()
            .is_some_and(|token| *token != binding.token)
    {
        return Err("Codex pane binding does not match its saved context".into());
    }
    let snapshot = snapshot(&root, &binding.token)?;
    let actual_home = integration::home()?;
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if canonical(&actual_home) != canonical(&snapshot.codex_home) {
        return Err("the pane uses a different Codex home than its launch snapshot; start it with the configured CODEX_HOME".into());
    }
    validate_live(&snapshot, &context, &input.cwd)?;
    let state_path = path(&root, "sessions", &snapshot.token);
    if state_path.exists() {
        let state: Session = read(&root, &state_path)?;
        if state.context != context {
            return Err("this Codex session is already bound to another pane".into());
        }
        if state.id != input.session_id && input.source != "clear" {
            if explicit.is_none() && input.source == "startup" {
                return Ok(None);
            }
            return Err(
                "Codex resumed a different conversation than the saved crew session".into(),
            );
        }
    } else if input.source != "startup" {
        return Err("Codex has no initial session binding to recover".into());
    }
    let claim_path = path(&root, "claims", &input.session_id);
    if claim_path.exists() {
        let owner: String = read(&root, &claim_path)?;
        if owner != snapshot.token {
            return Err("Codex session is owned by another crew role".into());
        }
    }
    write(&root, &claim_path, &snapshot.token)?;
    write(
        &root,
        &state_path,
        &Session {
            id: input.session_id.clone(),
            context: context.clone(),
        },
    )?;
    // Keep native identity and crew recovery in separate report sequences. The native hook
    // runs concurrently; neither report acquires lifecycle authority over screen detection.
    let start = Instant::now();
    loop {
        let sequence = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos(),
        )
        .map_err(|_| "session sequence overflow")?;
        rpc(
            &context,
            "pane.report_agent_session",
            json!({"pane_id":context.pane,"source":"herdr:codex","agent":"codex","seq":sequence,"agent_session_id":input.session_id,"session_start_source":input.source}),
        )?;
        let pane = rpc(&context, "pane.get", json!({"pane_id":context.pane}))?;
        if pane["pane"]["agent_session"]["value"] != input.session_id {
            if start.elapsed() >= Duration::from_secs(5) {
                return Err("Herdr did not accept the Codex session identity".into());
            }
            sleep(Duration::from_millis(100));
            continue;
        }
        // No session ID on this report: Herdr's native reference was verified above.
        let response = rpc(
            &context,
            "pane.report_agent_session",
            json!({"pane_id":context.pane,"source":"herdr-crew:codex","agent":"codex","seq":sequence,"resume_argv":["herdr-crew","codex-resume",snapshot.token]}),
        );
        match response {
            Ok(_) => break,
            Err(error)
                if error.contains("resume_not_accepted")
                    && start.elapsed() < Duration::from_secs(5) =>
            {
                sleep(Duration::from_millis(100))
            }
            Err(error) => {
                return Err(format!(
                    "cannot register configured Codex recovery: {error}"
                ));
            }
        }
    }
    Ok(Some(snapshot.prompt))
}

/// Hook errors must be JSON with exit status zero; a failing command hook is advisory in Codex.
pub fn hook() {
    let result = (|| {
        let mut input = String::new();
        std::io::stdin()
            .take(32 * 1024 + 1)
            .read_to_string(&mut input)
            .map_err(|e| e.to_string())?;
        if input.len() > 32 * 1024 {
            return Err("Codex hook input exceeds 32 KiB".into());
        }
        process_hook(
            serde_json::from_str(&input).map_err(|e| format!("invalid Codex hook input: {e}"))?,
        )
    })();
    let output = match result {
        Ok(Some(prompt)) => {
            json!({"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":prompt}})
        }
        Ok(None) => json!({}),
        Err(error) => {
            json!({"continue":false,"stopReason":format!("herdr-crew: {error}"),"systemMessage":format!("herdr-crew: {error}")})
        }
    };
    println!("{output}");
}

pub fn resume(token: &str, root_flag: Option<&Path>) -> Result<(), String> {
    if !token_valid(token) {
        return Err("invalid Codex binding token".into());
    }
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let root = git::main_root(root_flag.unwrap_or(&cwd))?;
    let snapshot = snapshot(&root, token)?;
    integration::preflight_home(
        &snapshot.cwd,
        &snapshot.options,
        &root,
        &snapshot.codex_home,
    )?;
    let context = inherited_context()?.ok_or("run codex-resume inside the role's Herdr tab")?;
    validate_live(&snapshot, &context, &cwd)?;
    let id;
    {
        let _lock = Lock::acquire(&root)?;
        let mut session: Session = read(&root, &path(&root, "sessions", token))?;
        if !session_valid(&session.id) {
            return Err("saved Codex session ID is invalid".into());
        }
        let workspaces = rpc(&context, "workspace.list", json!({}))?;
        for ws in workspaces["workspaces"]
            .as_array()
            .ok_or("missing workspaces")?
        {
            let panes = rpc(
                &context,
                "pane.list",
                json!({"workspace_id":ws["workspace_id"]}),
            )?;
            for pane in panes["panes"].as_array().ok_or("missing panes")? {
                if pane["pane_id"] != context.pane
                    && pane["agent_session"]["agent"] == "codex"
                    && pane["agent_session"]["value"] == session.id
                {
                    return Err("the saved Codex session is already present in another pane".into());
                }
            }
        }
        id = session.id.clone();
        session.context = context.clone();
        write(&root, &path(&root, "sessions", token), &session)?;
        bind(&snapshot, context)?;
    }
    let mut command = Command::new("codex");
    command
        .arg("resume")
        .args(snapshot.options.args())
        .arg("-c")
        .arg("features.hooks=true")
        .arg(id)
        .current_dir(&snapshot.cwd)
        .env("CREW_CODEX_BINDING", token)
        .env("CODEX_HOME", &snapshot.codex_home)
        .env_remove("CODEX_THREAD_ID")
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_CHILD_SESSION")
        .env_remove("CLAUDE_CODE_ENTRYPOINT");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(format!("could not resume Codex: {}", command.exec()))
    }
    #[cfg(not(unix))]
    {
        command.status().map(|_| ()).map_err(|e| e.to_string())
    }
}

pub fn list(root: &Path) -> Result<(), String> {
    let directory = root.join(RUNTIME).join("snapshots");
    files::validate(root, &directory, files::Kind::Directory)?;
    if !directory.exists() {
        println!("herdr-crew: no saved Codex crew sessions");
        return Ok(());
    }
    let mut rows = Vec::new();
    for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
        let file_path = entry.map_err(|e| e.to_string())?.path();
        let token = file_path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("invalid snapshot filename")?;
        let snapshot = snapshot(root, token)?;
        let state_path = path(root, "sessions", token);
        let id = if state_path.exists() {
            read::<Session>(root, &state_path)?.id
        } else {
            "not-yet-bound".into()
        };
        if !session_valid(&id) {
            return Err("invalid saved Codex session ID".into());
        }
        rows.push((snapshot.name, token.to_string(), id));
    }
    rows.sort();
    println!("ROLE  BINDING  SESSION");
    for (name, token, id) in rows {
        println!("{name}  {token}  {id}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binding_keys_include_terminal_and_server() {
        let mut a = Context {
            socket: "/server-a".into(),
            workspace: "w1".into(),
            tab: "w1:t1".into(),
            pane: "w1:p1".into(),
            terminal: "term1".into(),
        };
        let first = context_key(&a);
        a.socket = "/server-b".into();
        assert_ne!(first, context_key(&a));
        a.socket = "/server-a".into();
        a.terminal = "term2".into();
        assert_ne!(first, context_key(&a));
    }
    #[test]
    fn tokens_and_session_ids_cannot_escape_runtime_paths() {
        assert!(token_valid(&new_token().unwrap()));
        for value in ["", "../other", "--last", "a/b", "a\nb"] {
            assert!(!session_valid(value));
            assert!(!token_valid(value));
        }
        assert!(session_valid("1234-abcd"));
    }
    #[test]
    fn context_bound_includes_runtime_guidance() {
        let config = crate::config::tests::basic();
        assert!(
            context_prompt(&config, "acme-lead", "hello")
                .unwrap()
                .contains("/r/acme/.herdr/status.json")
        );
        assert!(context_prompt(&config, "acme-lead", &"a".repeat(MAX_CONTEXT)).is_err());
    }
}
