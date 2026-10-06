//! Explicit user-hook setup and read-only trust inspection.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use crate::{agent::CodexOptions, files, process};

pub const HOOK_COMMAND: &str = "herdr-crew codex-hook";
pub const MATCHER: &str = "startup|resume|clear|compact";
pub const LAUNCHER_MARKER: &str = "herdr-crew public launcher format 1 (rarce/herdr-crew)";

pub fn home() -> Result<PathBuf, String> {
    let path = match std::env::var_os("CODEX_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        Some(_) => return Err("CODEX_HOME is empty".into()),
        None => PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?).join(".codex"),
    };
    if !path.is_absolute() {
        return Err("CODEX_HOME must be absolute".into());
    }
    if path
        .to_str()
        .is_none_or(|s| s.chars().any(char::is_control))
    {
        return Err("CODEX_HOME must be UTF-8 without control characters".into());
    }
    Ok(path)
}

fn handler() -> Value {
    json!({"type":"command", "command":HOOK_COMMAND, "timeout":20, "async":false, "additionalContextLimit":0})
}

/// Only remove our command; leave every other event, handler and unknown field alone.
pub fn merge(value: &mut Value, remove: bool) -> Result<(), String> {
    let root = value
        .as_object_mut()
        .ok_or("hooks.json must contain an object")?;
    if remove && !root.contains_key("hooks") {
        return Ok(());
    }
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("hooks must be an object")?;
    if remove && !hooks.contains_key("SessionStart") {
        return Ok(());
    }
    let groups = hooks
        .entry("SessionStart")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or("SessionStart must be an array")?;
    for group in groups.iter() {
        if group.get("hooks").and_then(Value::as_array).is_none() {
            return Err("each SessionStart group needs a hooks array".into());
        }
    }
    groups.retain_mut(|group| {
        let handlers = group["hooks"].as_array_mut().unwrap();
        let before = handlers.len();
        handlers.retain(|h| h["command"] != HOOK_COMMAND);
        before == 0 || !handlers.is_empty()
    });
    if !remove {
        groups.push(json!({"matcher":MATCHER, "hooks":[handler()]}));
    }
    Ok(())
}

pub fn launcher() -> Result<(), String> {
    let mut child = process::Inspector::spawn(Command::new("herdr-crew").arg("--launcher-id"))
        .map_err(|_| "the public herdr-crew launcher must be on PATH; install it with scripts/build.sh --install-launcher".to_string())?;
    if child.line()?.trim() != LAUNCHER_MARKER {
        return Err(
            "herdr-crew on PATH is not the public plugin launcher; rebuild/install the launcher"
                .into(),
        );
    }
    let mut protocol = process::Inspector::spawn(Command::new("herdr-crew").arg("codex-protocol"))?;
    if protocol.line()?.trim() != "1" {
        return Err("the registered crew plugin does not support Codex runtime format 1; rebuild or update it".into());
    }
    Ok(())
}

pub fn install(remove: bool) -> Result<(), String> {
    if !remove {
        launcher()?;
    }
    let home = home()?;
    if remove && !home.exists() {
        return Ok(());
    }
    fs::create_dir_all(&home).map_err(|e| format!("cannot create {}: {e}", home.display()))?;
    let home = home.canonicalize().map_err(|e| e.to_string())?;
    let path = home.join("hooks.json");
    files::validate(&home, &path, files::Kind::File)?;
    let mut value = match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && remove => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(e.to_string()),
    };
    merge(&mut value, remove)?;
    files::write(
        &home,
        &path,
        &format!(
            "{}\n",
            serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
        ),
    )?;
    if remove {
        println!(
            "herdr-crew: removed the crew Codex hook from {}",
            path.display()
        );
    } else {
        println!(
            "herdr-crew: installed the crew Codex hook in {}. Open Codex and review/trust `herdr-crew codex-hook` with /hooks, then run herdr-crew check.",
            path.display()
        );
    }
    Ok(())
}

pub fn preflight(cwd: &Path, options: &CodexOptions, root: &Path) -> Result<(), String> {
    preflight_home(cwd, options, root, &home()?)
}

pub fn preflight_home(
    cwd: &Path,
    options: &CodexOptions,
    root: &Path,
    codex_home: &Path,
) -> Result<(), String> {
    process::version("codex", (0, 160, 1))?;
    launcher()?;
    let mut effective = options.clone();
    effective.resolve_dirs(root)?;
    let problems = effective.problems();
    if !problems.is_empty() {
        return Err(problems.join("; "));
    }
    if let Some(profile) = &effective.profile {
        validate_profile(codex_home, profile)?;
    }
    let mut command = Command::new("codex");
    command
        .arg("app-server")
        .arg("--stdio")
        .arg("-c")
        .arg("features.hooks=true");
    // app-server cannot select a CLI profile. Profile hook overrides are rejected above;
    // inspect the base/project hooks with the crew's explicit runtime configuration.
    let args = effective.args();
    let bytes: usize = args
        .iter()
        .map(|arg| arg.len() + 3 * arg.matches('\'').count() + 3)
        .sum();
    if bytes > 8192 {
        return Err("Codex launch options exceed 8192 shell-encoded bytes".into());
    }
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--profile" {
            i += 2;
            continue;
        }
        command.arg(&args[i]);
        i += 1;
    }
    command
        .current_dir(root)
        .env("CODEX_HOME", codex_home)
        .env_remove("CODEX_THREAD_ID");
    let mut server = process::Inspector::spawn(&mut command)?;
    server.send(json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"herdr-crew","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}}))?;
    server.response(1)?;
    server.send(json!({"method":"initialized"}))?;
    server.send(json!({"id":2,"method":"hooks/list","params":{"cwds":[cwd]}}))?;
    validate_hooks(&server.response(2)?, cwd, &codex_home.join("hooks.json"))
}

fn validate_profile(home: &Path, name: &str) -> Result<(), String> {
    let path = home.join(format!("{name}.config.toml"));
    let mut text = String::new();
    fs::File::open(&path)
        .and_then(|file| file.take(512 * 1024 + 1).read_to_string(&mut text))
        .map_err(|e| format!("Codex profile {name}: cannot read {}: {e}", path.display()))?;
    if text.len() > 512 * 1024 {
        return Err(format!("Codex profile {name} exceeds 512 KiB"));
    }
    let profile: toml::Value =
        toml::from_str(&text).map_err(|e| format!("invalid Codex profile {name}: {e}"))?;
    // Codex 0.160.1 rejects --profile on app-server. Until hooks/list can inspect a
    // profile, require hook configuration/trust to live in the inspectable base layer.
    if profile.get("hooks").is_some() {
        return Err(format!(
            "Codex profile {name} configures hooks; Codex app-server cannot inspect profile hooks. Move hook configuration and trust to the base config.toml, then review /hooks"
        ));
    }
    if profile.get("profile").is_some() || profile.get("profiles").is_some() {
        return Err(format!(
            "Codex profile {name} contains legacy profile selectors; use top-level configuration keys"
        ));
    }
    Ok(())
}

pub fn validate_hooks(value: &Value, cwd: &Path, source: &Path) -> Result<(), String> {
    let fail = || {
        "crew Codex hook is missing, disabled, modified, or untrusted; run herdr-crew codex-install and review it in Codex /hooks".to_string()
    };
    let data = value["data"]
        .as_array()
        .ok_or("Codex hooks/list returned no data")?;
    let entry = data
        .iter()
        .find(|entry| {
            entry["cwd"]
                .as_str()
                .is_some_and(|dir| Path::new(dir) == cwd)
        })
        .ok_or("Codex did not inspect the requested working directory")?;
    if entry["errors"]
        .as_array()
        .is_none_or(|errors| !errors.is_empty())
    {
        return Err(format!(
            "Codex hook configuration has errors: {}",
            entry["errors"]
        ));
    }
    let hooks = entry["hooks"].as_array().ok_or_else(fail)?;
    let matching: Vec<_> = hooks
        .iter()
        .filter(|h| h["command"] == HOOK_COMMAND && h["eventName"] == "sessionStart")
        .collect();
    let [hook] = matching.as_slice() else {
        return Err(fail());
    };
    if hook["sourcePath"].as_str().is_none_or(|path| {
        let path = Path::new(path);
        path != source
            && path
                .canonicalize()
                .ok()
                .zip(source.canonicalize().ok())
                .is_none_or(|(a, b)| a != b)
    }) || hook["enabled"] != true
        || !matches!(hook["trustStatus"].as_str(), Some("trusted" | "managed"))
        || hook["matcher"] != MATCHER
        || hook["async"] == true
        || hook["additionalContextLimit"] != 0
        || hook["currentHash"].as_str().is_none_or(str::is_empty)
    {
        return Err(fail());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn install_and_remove_preserve_native_and_user_hooks() {
        let original = json!({"future":42,"hooks":{"Stop":[{"hooks":[{"command":"user"}]}],"SessionStart":[{"matcher":"resume","custom":true,"hooks":[{"command":"native","future":7}]},{"hooks":[],"future":true}]}});
        let mut value = original.clone();
        merge(&mut value, false).unwrap();
        let installed = value.clone();
        merge(&mut value, false).unwrap();
        assert_eq!(value, installed);
        merge(&mut value, true).unwrap();
        assert_eq!(value, original);
        for original in [json!({"future":1}), json!({"hooks":{"Stop":[]}})] {
            let mut value = original.clone();
            merge(&mut value, true).unwrap();
            assert_eq!(value, original);
        }
        merge(&mut value, true).unwrap();
        assert_eq!(value, original);
    }
    #[test]
    fn trust_and_exact_handler_are_required() {
        let mut value = json!({"data":[{"cwd":"/repo","errors":[],"hooks":[{"command":HOOK_COMMAND,"eventName":"sessionStart","sourcePath":"/home/hooks.json","enabled":true,"trustStatus":"trusted","currentHash":"abc","matcher":MATCHER,"async":false,"additionalContextLimit":0}]}]});
        assert!(validate_hooks(&value, Path::new("/repo"), Path::new("/home/hooks.json")).is_ok());
        for state in ["untrusted", "modified"] {
            value["data"][0]["hooks"][0]["trustStatus"] = json!(state);
            assert!(
                validate_hooks(&value, Path::new("/repo"), Path::new("/home/hooks.json")).is_err()
            );
        }
        value["data"][0]["hooks"][0]["trustStatus"] = json!("trusted");
        value["data"][0]["hooks"][0]["additionalContextLimit"] = json!(2500);
        assert!(validate_hooks(&value, Path::new("/repo"), Path::new("/home/hooks.json")).is_err());
    }
}
