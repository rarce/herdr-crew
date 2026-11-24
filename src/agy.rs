//! Antigravity CLI (`agy`) support: the crew's `SessionStart` hook and its installation.
//!
//! `agy` has no `--append-system-prompt-file` flag, and herdr resumes it with a plain
//! `agy --conversation <id>`. Before starting an `agy` role, crew writes a one-shot pending
//! binding for that pane in `.herdr/prompts/`; on `SessionStart`, `herdr-crew agy-hook` resolves
//! the project root from `cwd`, atomically consumes the pending binding for `HERDR_PANE_ID`, and
//! injects the rendered role prompt as a persistent `systemMessage`. Because each binding is
//! consumed once, subagents spawned by a coordinator (`experimental_agy_tree`) and resumed
//! conversations never receive duplicate or leaked role prompts, and non-crew sessions get `{}`.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::codex::integration::LAUNCHER_MARKER;
use crate::{files, git, process, prompt};

/// Top-level hook key owned by herdr-crew inside `hooks.json`.
pub const HOOK_NAME: &str = "herdr-crew";
/// Command invoked by `agy` on `SessionStart`.
pub const HOOK_COMMAND: &str = "herdr-crew agy-hook";
/// Largest rendered role prompt handed to the hook.
pub const MAX_PROMPT: usize = 64 * 1024;
/// Minimum `agy` version: 1.5.0+ supports base `--model` names with `--effort` and `SessionStart`.
pub const MIN_AGY: (u32, u32, u32) = (1, 5, 0);

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    pane: String,
    prompt: PathBuf,
}

/// `agy`'s global customization directory: `ANTIGRAVITY_CLI_CONFIG_DIR` (with `~` expansion) or
/// `~/.gemini/config`. Note: `agy 1.5.15` reads global hooks from `~/.gemini/config/hooks.json`;
/// the env override is kept for parity with `herdr integration` and offline tests.
pub fn config_dir() -> Result<PathBuf, String> {
    let home = || {
        std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| "HOME is not set".to_string())
    };
    let path = match std::env::var("ANTIGRAVITY_CLI_CONFIG_DIR") {
        Ok(value) if value == "~" => home()?,
        Ok(value) if value.starts_with("~/") => home()?.join(&value[2..]),
        Ok(value) if !value.is_empty() => PathBuf::from(value),
        Ok(_) | Err(std::env::VarError::NotPresent) => home()?.join(".gemini").join("config"),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err("ANTIGRAVITY_CLI_CONFIG_DIR must be UTF-8".into());
        }
    };
    if !path.is_absolute() {
        return Err("ANTIGRAVITY_CLI_CONFIG_DIR must be absolute".into());
    }
    if path
        .to_str()
        .is_none_or(|s| s.chars().any(char::is_control))
    {
        return Err("the agy config directory must be UTF-8 without control characters".into());
    }
    Ok(path)
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
            handlers
                .iter()
                .any(|h| h.get("command").and_then(Value::as_str) == Some(HOOK_COMMAND))
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

fn pending_path(root: &Path, pane: &str) -> PathBuf {
    let key = files::fnv64(0xcbf29ce484222325, pane.as_bytes());
    root.join(prompt::PROMPTS_DIR)
        .join(format!("agy-pending-{key:016x}.json"))
}

/// Writes the one-shot pending binding for `pane` before `agent start` launches `agy`.
/// Works identically for `CreateWorkspace`, `AdoptWorkspace`, `CreateTab`, and `add`.
pub fn prepare(root: &Path, pane: &str, prompt_path: &Path) -> Result<(), String> {
    let relative = prompt_path
        .strip_prefix(root)
        .map_err(|_| format!("{} is outside {}", prompt_path.display(), root.display()))?;
    let path = pending_path(root, pane);
    let record = Pending {
        pane: pane.to_string(),
        prompt: relative.to_path_buf(),
    };
    let text = serde_json::to_string(&record).map_err(|e| e.to_string())?;
    files::write(root, &path, &text)
}

/// `agy-hook`: invoked by `agy` on `SessionStart`. Reads camelCase JSON on stdin and prints the
/// `injectSteps` response on stdout. Always prints valid JSON and exits cleanly so non-crew
/// sessions are never broken.
pub fn hook() {
    let mut raw = String::new();
    let _ = io::stdin()
        .take((MAX_PROMPT as u64) + 1)
        .read_to_string(&mut raw);
    let pane = std::env::var("HERDR_PANE_ID")
        .ok()
        .filter(|v| !v.is_empty());
    let out = hook_response(&raw, pane.as_deref());
    println!("{out}");
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
    let path = pending_path(&root, pane);
    let Ok(validated_pending) = files::validate(&root, &path, files::Kind::File) else {
        return json!({});
    };
    let Ok(text) = fs::read_to_string(&validated_pending) else {
        return json!({});
    };
    // Consume the pending binding atomically so only this initial session gets the role prompt,
    // never subagents spawned by a coordinator or resumed conversations.
    if fs::remove_file(&validated_pending).is_err() {
        return json!({});
    }
    let Ok(pending) = serde_json::from_str::<Pending>(&text) else {
        return json!({});
    };
    if pending.pane != pane {
        return json!({});
    }
    let prompt_file = root.join(&pending.prompt);
    let Ok(validated_prompt) = files::validate(&root, &prompt_file, files::Kind::File) else {
        return json!({});
    };
    let Ok(meta) = fs::symlink_metadata(&validated_prompt) else {
        return json!({});
    };
    if !meta.is_file() || meta.len() > MAX_PROMPT as u64 {
        return json!({});
    }
    let Ok(prompt_text) = fs::read_to_string(&validated_prompt) else {
        return json!({});
    };
    if prompt_text.trim().is_empty() || prompt_text.len() > MAX_PROMPT {
        return json!({});
    }
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
    fn hook_consumes_pending_binding_once_and_ignores_subagents_and_non_crew() {
        let root = git::test_repo("agy-hook");
        let prompt_file = root.join(prompt::PROMPTS_DIR).join("lead.txt");
        files::write(&root, &prompt_file, "You are lead.").unwrap();
        prepare(&root, "w1:p1", &prompt_file).unwrap();

        let stdin = json!({"conversationId": "c1", "cwd": root}).to_string();

        // Non-Herdr session without HERDR_PANE_ID gets empty object and does not consume binding.
        assert_eq!(hook_response(&stdin, None), json!({}));
        // Unrelated pane gets empty object and does not consume binding.
        assert_eq!(hook_response(&stdin, Some("w1:p9")), json!({}));

        // First SessionStart for w1:p1 consumes binding and injects systemMessage.
        let first = hook_response(&stdin, Some("w1:p1"));
        assert_eq!(
            first["injectSteps"][0]["systemMessage"]["systemMessage"].as_str(),
            Some("You are lead.")
        );

        // Second SessionStart (subagent or resume) gets empty object; no prompt leakage.
        let subagent_stdin = json!({"conversationId": "sub-1", "cwd": root}).to_string();
        assert_eq!(hook_response(&subagent_stdin, Some("w1:p1")), json!({}));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn prompts_are_bounded() {
        assert!(prompt_problem("dev", "You are dev.").is_none());
        assert!(prompt_problem("dev", " \n").is_some());
        assert!(prompt_problem("dev", &"x".repeat(MAX_PROMPT + 1)).is_some());
    }
}
