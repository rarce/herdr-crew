//! Antigravity CLI (`agy`) support: the crew's `SessionStart` hook and its installation.
//!
//! `agy` has no `--append-system-prompt-file` flag, and herdr resumes it with a plain
//! `agy --conversation <id>`. The crew hook injects the rendered role prompt as a `systemMessage`
//! on `SessionStart`, which `agy` persists in the conversation trajectory across turns and
//! restores. Non-crew `agy` sessions have no `CREW_AGY_PROMPT_FILE` and receive `{}`.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::{files, process};

/// Top-level hook key owned by herdr-crew inside `hooks.json`.
pub const HOOK_NAME: &str = "herdr-crew";
/// Command invoked by `agy` on `SessionStart`.
pub const HOOK_COMMAND: &str = "herdr-crew agy-hook";
/// Environment variable set on crew `agy` panes pointing to the rendered role prompt file.
pub const PROMPT_ENV: &str = "CREW_AGY_PROMPT_FILE";
/// Largest rendered role prompt handed to the hook.
pub const MAX_PROMPT: usize = 64 * 1024;
/// Minimum `agy` version: 1.5.0+ supports base `--model` names with `--effort` and `SessionStart`.
pub const MIN_AGY: (u32, u32, u32) = (1, 5, 0);

/// `agy`'s global customization directory: `ANTIGRAVITY_CLI_CONFIG_DIR` (with `~` expansion) or
/// `~/.gemini/config`.
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

/// Before an `agy` role starts: `hooks.json` must contain our active `SessionStart` hook.
pub fn preflight() -> Result<(), String> {
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
pub fn prompt_problem(name: &str, prompt: &str) -> Option<String> {
    (prompt.trim().is_empty() || prompt.len() > MAX_PROMPT).then(|| {
        format!(
            "the prompt of {name} must be nonempty and at most {MAX_PROMPT} bytes for agy \
             ({} bytes)",
            prompt.len()
        )
    })
}

/// Environment flags passed to `herdr workspace create` / `herdr tab create` for an `agy` role.
pub fn pane_environment(prompt_path: &Path) -> Vec<String> {
    vec![
        "--env".into(),
        format!("{PROMPT_ENV}={}", prompt_path.display()),
    ]
}

/// `agy-hook`: invoked by `agy` on `SessionStart`. Reads camelCase JSON on stdin and prints the
/// `injectSteps` response on stdout. Always prints valid JSON and exits cleanly so non-crew
/// sessions are never broken.
pub fn hook() {
    let mut raw = String::new();
    let _ = io::stdin()
        .take((MAX_PROMPT as u64) + 1)
        .read_to_string(&mut raw);
    let prompt_path = std::env::var_os(PROMPT_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let out = hook_response(&raw, prompt_path.as_deref());
    println!("{out}");
}

fn hook_response(stdin: &str, prompt_path: Option<&Path>) -> Value {
    let Some(path) = prompt_path else {
        return json!({});
    };
    if !path.is_absolute() {
        return json!({});
    }
    let Ok(meta) = fs::symlink_metadata(path) else {
        return json!({});
    };
    if !meta.is_file() || meta.len() > MAX_PROMPT as u64 {
        return json!({});
    }
    let Ok(prompt) = fs::read_to_string(path) else {
        return json!({});
    };
    if prompt.trim().is_empty() || prompt.len() > MAX_PROMPT {
        return json!({});
    }
    // Avoid re-injecting the role prompt if SessionStart fires again on `--conversation` resume.
    if let Ok(input) = serde_json::from_str::<Value>(stdin)
        && let Some(conversation_id) = input
            .get("conversationId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        && !claim_conversation(path, conversation_id)
    {
        return json!({});
    }
    json!({
        "injectSteps": [
            {
                "systemMessage": {
                    "systemMessage": prompt
                }
            }
        ]
    })
}

/// Records `conversation_id` next to `prompt_path` so a resumed conversation is injected at most
/// once. Returns `false` if already recorded, `true` otherwise (best-effort on I/O errors).
fn claim_conversation(prompt_path: &Path, conversation_id: &str) -> bool {
    let state_path = PathBuf::from(format!("{}.sessions", prompt_path.display()));
    let mut seen: BTreeSet<String> = fs::read_to_string(&state_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if !seen.insert(conversation_id.to_string()) {
        return false;
    }
    if let Ok(text) = serde_json::to_string(&seen) {
        let _ = fs::write(&state_path, text);
    }
    true
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
    fn hook_injects_once_per_conversation_and_ignores_non_crew() {
        let dir = std::env::temp_dir().join(format!("crew-agy-hook-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let prompt = dir.join("dev.txt");
        fs::write(&prompt, "You are dev.").unwrap();

        // Non-crew session without env var gets empty object.
        assert_eq!(hook_response(r#"{"conversationId":"c1"}"#, None), json!({}));

        // First SessionStart injects systemMessage.
        let first = hook_response(r#"{"conversationId":"c1"}"#, Some(&prompt));
        assert_eq!(
            first["injectSteps"][0]["systemMessage"]["systemMessage"].as_str(),
            Some("You are dev.")
        );

        // Second SessionStart for the same conversationId (e.g. resume) does not duplicate.
        let second = hook_response(r#"{"conversationId":"c1"}"#, Some(&prompt));
        assert_eq!(second, json!({}));

        // A new conversationId gets the prompt.
        let fresh = hook_response(r#"{"conversationId":"c2"}"#, Some(&prompt));
        assert_eq!(
            fresh["injectSteps"][0]["systemMessage"]["systemMessage"].as_str(),
            Some("You are dev.")
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prompts_are_bounded() {
        assert!(prompt_problem("dev", "You are dev.").is_none());
        assert!(prompt_problem("dev", " \n").is_some());
        assert!(prompt_problem("dev", &"x".repeat(MAX_PROMPT + 1)).is_some());
    }
}
