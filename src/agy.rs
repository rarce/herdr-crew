//! Antigravity CLI (`agy`) support: the crew's `SessionStart` hook and its installation.
//!
//! `agy` has no `--append-system-prompt-file` flag, and herdr resumes it with a plain
//! `agy --conversation <id>`. Before starting an `agy` role, crew writes a one-shot pending
//! binding with the rendered prompt in private per-user state; on `SessionStart`, `herdr-crew agy-hook` resolves
//! the project root from `cwd`, atomically consumes the pending binding for its pane (the TUI's
//! `HERDR_PANE_ID`, or herdr-core's terminal, where it also records the conversation), and
//! injects the rendered role prompt as a persistent `systemMessage`. Because each binding is
//! consumed once, subagents spawned by a coordinator (`experimental_agy_tree`) and resumed
//! conversations never receive duplicate or leaked role prompts, and non-crew sessions get `{}`.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::backend::Started;
use crate::codex::integration::LAUNCHER_MARKER;
use crate::config::Config;
use crate::{files, git, process};

/// Top-level hook key owned by herdr-crew inside `hooks.json`.
pub const HOOK_NAME: &str = "herdr-crew";
/// Command invoked by `agy` on `SessionStart`.
pub const HOOK_COMMAND: &str = "herdr-crew agy-hook";
/// Largest rendered role prompt handed to the hook.
pub const MAX_PROMPT: usize = 64 * 1024;
/// Minimum `agy` version: 1.5.0+ supports base `--model` names with `--effort` and `SessionStart`.
pub const MIN_AGY: (u32, u32, u32) = (1, 5, 0);
const HOOK_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    pane: String,
    root: PathBuf,
    token: String,
    prompt: String,
}

fn home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| "agy requires an absolute HOME".into())
}

// agy 1.5.15 ignores ANTIGRAVITY_CLI_CONFIG_DIR for global hooks.
fn config_dir() -> Result<PathBuf, String> {
    Ok(home()?.join(".gemini/config"))
}

fn hooks_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("hooks.json"))
}

fn hook_entry() -> Value {
    json!({
        "SessionStart": [
            {
                "type": "command",
                "command": HOOK_COMMAND,
                "timeout": 20
            }
        ]
    })
}

/// Verify that the public `herdr-crew` launcher is on `PATH`, since `agy` invokes `HOOK_COMMAND`
/// through `sh -c`.
pub fn launcher() -> Result<(), String> {
    let mut child = process::Inspector::spawn(Command::new("herdr-crew").arg("--launcher-id"))
        .map_err(|_| {
            "the public herdr-crew launcher must be on PATH; install it with \
             scripts/build.sh --install-launcher"
                .to_string()
        })?;
    if child.line()?.trim() != LAUNCHER_MARKER {
        return Err(
            "herdr-crew on PATH is not the public plugin launcher; rebuild/install the launcher"
                .into(),
        );
    }
    Ok(())
}

/// Merge or remove only the `"herdr-crew"` hook group, leaving every other hook (including
/// herdr's `antigravity-cli` integration) untouched.
pub fn merge(value: &mut Value, remove: bool) -> Result<(), String> {
    let root = value
        .as_object_mut()
        .ok_or("hooks.json must contain a JSON object")?;
    if remove {
        root.remove(HOOK_NAME);
    } else {
        root.insert(HOOK_NAME.into(), hook_entry());
    }
    Ok(())
}

fn has_hook(value: &Value) -> bool {
    value
        .get(HOOK_NAME)
        .and_then(|entry| {
            if entry.get("enabled").and_then(Value::as_bool) == Some(false) {
                return None;
            }
            entry.get("SessionStart")?.as_array()
        })
        .is_some_and(|handlers| {
            handlers.iter().any(|h| {
                h.get("command").and_then(Value::as_str) == Some(HOOK_COMMAND)
                    && h.get("type").is_none_or(|t| t == "command")
            })
        })
}

/// `agy-install` and `agy-uninstall`: update only the `"herdr-crew"` key in `hooks.json`.
pub fn install(remove: bool) -> Result<(), String> {
    if !remove {
        launcher()?;
    }
    let dir = config_dir()?;
    if remove && !dir.exists() {
        return Ok(());
    }
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let dir = dir
        .canonicalize()
        .map_err(|e| format!("agy config directory: {e}"))?;
    let path = dir.join("hooks.json");
    files::validate(&dir, &path, files::Kind::File)?;
    let mut value = match fs::read_to_string(&path) {
        Ok(text) if text.trim().is_empty() => json!({}),
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?,
        Err(e) if e.kind() == io::ErrorKind::NotFound && remove => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    merge(&mut value, remove)?;
    files::write(
        &dir,
        &path,
        &format!(
            "{}\n",
            serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
        ),
    )?;
    if remove {
        println!(
            "herdr-crew: removed the crew agy hook from {}",
            path.display()
        );
    } else {
        println!(
            "herdr-crew: installed the crew agy hook in {}. Run `herdr integration install \
             antigravity-cli` for herdr session restore, then run herdr-crew check.",
            path.display()
        );
    }
    Ok(())
}

/// Before an `agy` role starts: the launcher must be on `PATH` and `hooks.json` must contain
/// our active `SessionStart` hook.
pub fn preflight() -> Result<(), String> {
    launcher()?;
    preflight_path(&hooks_path()?)
}

fn preflight_path(path: &Path) -> Result<(), String> {
    match fs::read_to_string(path) {
        Ok(text) => {
            let value: Value =
                serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            if has_hook(&value) {
                Ok(())
            } else {
                Err(format!(
                    "the crew agy hook is missing or disabled in {}; run `herdr-crew agy-install`",
                    path.display()
                ))
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(format!(
            "agy roles need the crew SessionStart hook to deliver their role prompt; run \
             `herdr-crew agy-install` (it updates {})",
            path.display()
        )),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Antigravity persists explicit folder approvals in its user settings. Compare exact
/// canonical directories so a trusted main checkout does not authorize its worktrees.
pub struct TrustedWorkspaces(HashSet<PathBuf>);

impl TrustedWorkspaces {
    pub fn read() -> Result<Self, String> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Settings {
            #[serde(default)]
            trusted_workspaces: Vec<PathBuf>,
        }

        let path = home()?.join(".gemini/antigravity-cli/settings.json");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self(HashSet::new())),
            Err(e) => {
                return Err(format!(
                    "cannot read agy workspace trust from {}: {e}",
                    path.display()
                ));
            }
        };
        let settings: Settings = serde_json::from_str(&text).map_err(|e| {
            format!(
                "cannot read agy workspace trust from {}: {e}",
                path.display()
            )
        })?;
        Ok(Self(
            settings
                .trusted_workspaces
                .into_iter()
                .filter(|path| path.is_absolute())
                .filter_map(|path| path.canonicalize().ok())
                .collect(),
        ))
    }

    pub fn require(&self, c: &Config, name: &str) -> Result<(), String> {
        let role = c.session_role(name).expect("a session of a known role");
        let path = if role.worktree {
            c.worktree_path(name)
                .expect("worktree settings for this role")
        } else {
            c.root.clone()
        };
        match path.canonicalize() {
            Ok(path) if self.0.contains(&path) => return Ok(()),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound && role.worktree => {
                let base = c
                    .worktrees
                    .as_ref()
                    .expect("worktree settings for this role");
                let quote = |text| crate::plan::shell_word(text, false);
                return Err(format!(
                    "agy role {name} needs a trusted checkout at {}. From the main checkout, \
                     create it with `git worktree add --detach {} {}`, then open `agy` in \
                     that worktree, approve folder trust and exit before retrying",
                    path.display(),
                    quote(&path.to_string_lossy()),
                    quote(&format!("{}/{}", base.remote, base.branch)),
                ));
            }
            Err(e) => {
                return Err(format!(
                    "cannot resolve the agy checkout for {name} at {}: {e}",
                    path.display()
                ));
            }
        }
        Err(format!(
            "agy role {name} needs a trusted checkout at {}. Open `agy` in that directory \
             without a prompt, approve folder trust and exit before retrying",
            path.display()
        ))
    }
}

/// `program`, the `agy` executable that herdr starts, must meet `MIN_AGY`.
pub fn require_version(program: impl AsRef<std::ffi::OsStr>) -> Result<(u32, u32, u32), String> {
    let found = process::detect(program).map_err(|e| format!("agy --version: {e}"))?;
    if found < MIN_AGY {
        let (a, b, c) = found;
        let (x, y, z) = MIN_AGY;
        return Err(format!(
            "agy {a}.{b}.{c} is too old: {x}.{y}.{z} or newer is required (update: agy update)"
        ));
    }
    Ok(found)
}

/// Why a rendered role prompt cannot be handed to the hook, if it cannot.
pub fn prompt_problem(name: &str, prompt_text: &str) -> Option<String> {
    (prompt_text.trim().is_empty() || prompt_text.len() > MAX_PROMPT).then(|| {
        format!(
            "the prompt of {name} must be nonempty and at most {MAX_PROMPT} bytes for agy \
             ({} bytes)",
            prompt_text.len()
        )
    })
}

struct Slot {
    dir: PathBuf,
    path: PathBuf,
}

impl Slot {
    fn new(home: &Path, root: &Path, pane: &str, create: bool) -> Result<Self, String> {
        let dir = home
            .canonicalize()
            .map_err(|e| e.to_string())?
            .join(".herdr-crew-agy");
        if dir.starts_with(root) {
            return Err("agy runtime state must be outside the checkout".into());
        }
        if create {
            match fs::DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        let meta = fs::symlink_metadata(&dir).map_err(|e| e.to_string())?;
        if !meta.is_dir() || meta.permissions().mode() & 0o077 != 0 {
            return Err(format!(
                "{} must be a private directory (0700)",
                dir.display()
            ));
        }
        let project = files::fnv64(0xcbf29ce484222325, root.as_os_str().as_encoded_bytes());
        let key = files::fnv64(0xcbf29ce484222325, pane.as_bytes());
        let path = dir.join(format!("{project:016x}-{key:016x}.json"));
        Ok(Self { dir, path })
    }

    // Serialize publication, consumption and rollback, including overlapping launches.
    fn lock(&self) -> Result<fs::File, String> {
        let file = fs::File::open(&self.dir).map_err(|e| e.to_string())?;
        file.lock().map_err(|e| e.to_string())?;
        Ok(file)
    }

    fn read(&self) -> Result<Option<Pending>, String> {
        match fs::read(&self.path) {
            Ok(data) => serde_json::from_slice(&data)
                .map(Some)
                .map_err(|e| e.to_string()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn consume(&self, root: &Path, pane: &str) -> Option<String> {
        let _lock = self.lock().ok()?;
        let pending = self.read().ok()??;
        if pending.root != root
            || pending.pane != pane
            || prompt_problem(pane, &pending.prompt).is_some()
        {
            return None;
        }
        self.remove().ok()?;
        Some(pending.prompt)
    }

    fn cancel(&self, token: &str) -> Result<(), String> {
        let _lock = self.lock()?;
        if self.read()?.is_some_and(|pending| pending.token == token) {
            self.remove()?;
        }
        Ok(())
    }

    /// On timeout, remove only this launch's pending binding under the consumption lock.
    fn wait_for_hook(&self, token: &str) -> Result<bool, String> {
        let deadline = Instant::now() + HOOK_WAIT;
        loop {
            {
                let _lock = self.lock()?;
                if !self.read()?.is_some_and(|pending| pending.token == token) {
                    return Ok(true);
                }
                if Instant::now() >= deadline {
                    self.remove()?;
                    return Ok(false);
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn remove(&self) -> Result<(), String> {
        fs::remove_file(&self.path).map_err(|e| format!("{}: {e}", self.path.display()))
    }
}

pub fn launch(
    root: &Path,
    name: &str,
    pane: &str,
    prompt: &str,
    has_start_message: bool,
    start: impl FnOnce() -> Result<Started, String>,
) -> Result<Started, String> {
    let slot = Slot::new(&home()?, root, pane, true)?;
    let record = Pending {
        pane: pane.into(),
        root: root.into(),
        prompt: prompt.into(),
        token: files::random_hex(16).map_err(|e| e.to_string())?,
    };
    {
        let _lock = slot.lock()?;
        if slot.read()?.is_some() {
            return Err(format!(
                "an agy launch is already pending for {pane}. If the previous agent has exited \
                 and its launch was abandoned, remove only {} and retry",
                slot.path.display()
            ));
        }
        files::write(
            &slot.dir,
            &slot.path,
            &serde_json::to_string(&record).map_err(|e| e.to_string())?,
        )?;
    }
    let started = match start() {
        Ok(started) => started,
        Err(error) => {
            slot.cancel(&record.token)
                .map_err(|e| format!("{error}; could not clear pending launch: {e}"))?;
            return Err(error);
        }
    };
    // A startup dialog can delay SessionStart, as can waiting for the first user message.
    if started == Started::Ready
        && has_start_message
        && !slot
            .wait_for_hook(&record.token)
            .map_err(|e| format!("cannot verify the agy role prompt for {name} in {pane}: {e}"))?
    {
        return Err(format!(
            "agy role {name} in {pane} did not consume its SessionStart role prompt. \
             Close that tab, verify folder trust and the crew hook, then retry"
        ));
    }
    Ok(started)
}

/// `agy-hook`: invoked by `agy` on `SessionStart`. Reads camelCase JSON on stdin and prints the
/// `injectSteps` response on stdout. Always prints valid JSON and exits cleanly so non-crew
/// sessions are never broken.
pub fn hook() {
    let mut raw = String::new();
    let _ = io::stdin()
        .take((MAX_PROMPT as u64) + 1)
        .read_to_string(&mut raw);
    let mut out = json!({});
    for key in crate::backend::hook_pane_keys() {
        out = hook_response(&raw, Some(&key));
        if out.get("injectSteps").is_some() {
            // Under herdr-core nothing records the conversation for a restore: the hook does,
            // so that `up` can resume it in the kept shell (`agy --conversation <id>`).
            if key.starts_with("hc:") {
                record_conversation(&raw);
            }
            break;
        }
    }
    println!("{out}");
}

/// Labels this herdr-core terminal with the conversation agy started (`crew.session`). Best
/// effort: a failure only costs the resume.
fn record_conversation(stdin: &str) {
    let Some(id) = serde_json::from_str::<Value>(stdin)
        .ok()
        .and_then(|v| v.get("conversationId")?.as_str().map(str::to_string))
        .filter(|id| conversation_id(id))
    else {
        return;
    };
    let Some(address) = std::env::var("HC_TERMINAL_ID")
        .ok()
        .filter(|v| !v.is_empty())
    else {
        return;
    };
    let _ = crate::hc::Hc::from_env().set_label(&address, "crew.session", &id);
}

/// An id `up` may type after `agy --conversation`: letters, digits, `-` and `_`.
fn conversation_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn hook_response(stdin: &str, pane: Option<&str>) -> Value {
    let Some(pane) = pane else {
        return json!({});
    };
    let Ok(input) = serde_json::from_str::<Value>(stdin) else {
        return json!({});
    };
    let Some(cwd) = input
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return json!({});
    };
    let Ok(root) = git::main_root(Path::new(cwd)) else {
        return json!({});
    };
    let Ok(slot) = home().and_then(|home| Slot::new(&home, &root, pane, false)) else {
        return json!({});
    };
    let Some(prompt_text) = slot.consume(&root, pane) else {
        return json!({});
    };
    json!({
        "injectSteps": [
            {
                "systemMessage": {
                    "systemMessage": prompt_text
                }
            }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_preserves_other_hooks_and_removes_only_crew() {
        let mut value = json!({
            "herdr-antigravity": {
                "PreInvocation": [{"type": "command", "command": "/home/u/.gemini/config/hooks/herdr-agent-state.sh"}]
            }
        });
        merge(&mut value, false).unwrap();
        assert!(has_hook(&value));
        assert!(value.get("herdr-antigravity").is_some());

        merge(&mut value, true).unwrap();
        assert!(!has_hook(&value));
        assert!(value.get("herdr-antigravity").is_some());
    }

    #[test]
    fn handler_type_defaults_to_command() {
        for (kind, valid) in [
            (None, true),
            (Some(json!("command")), true),
            (Some(json!("other")), false),
            (Some(Value::Null), false),
        ] {
            let mut h = json!({"command": HOOK_COMMAND});
            if let Some(kind) = kind {
                h["type"] = kind;
            }
            assert_eq!(has_hook(&json!({HOOK_NAME: {"SessionStart": [h]}})), valid);
        }
    }

    #[test]
    fn prompts_are_bounded() {
        assert!(prompt_problem("dev", "You are dev.").is_none());
        assert!(prompt_problem("dev", " \n").is_some());
        assert!(prompt_problem("dev", &"x".repeat(MAX_PROMPT + 1)).is_some());
    }
}
