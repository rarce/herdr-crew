//! pi support: the crew's pi extension and its explicit installation.
//!
//! herdr resumes pi with a plain `pi --session <file>`, and pi rebuilds its system prompt on every
//! start, so a prompt passed with `--append-system-prompt` would not survive a restore. The crew
//! extension (`src/pi/herdr-crew.ts`) saves the role prompt in the session file the first time and
//! appends it to the system prompt on every turn, so the conversation carries its own role.

use std::fs;
use std::path::PathBuf;

use crate::{files, process};

/// The extension, installed verbatim; `check` and `up` require this exact content.
pub const EXTENSION: &str = include_str!("pi/herdr-crew.ts");
const EXTENSION_FILE: &str = "herdr-crew.ts";
/// Marks a file this program wrote; uninstall removes nothing else.
const MARKER: &str = "// HERDR_CREW_PI_EXTENSION=";
/// The largest rendered role prompt; the extension enforces the same bound.
pub const MAX_PROMPT: usize = 64 * 1024;
/// herdr 0.9.3's pi integration reports state and sessions only when the extension context has
/// `mode`, which pi 0.73.1 lacks; without those reports herdr cannot resume pi. 1.1.0 is tested.
pub const MIN_PI: (u32, u32, u32) = (1, 1, 0);

/// pi's configuration directory: `PI_CODING_AGENT_DIR` (with pi's `~` expansion) or `~/.pi/agent`.
pub fn agent_dir() -> Result<PathBuf, String> {
    let home = || {
        std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .ok_or("HOME is not set")
    };
    let path = match std::env::var("PI_CODING_AGENT_DIR") {
        Ok(value) if value == "~" => home()?,
        Ok(value) if value.starts_with("~/") => home()?.join(&value[2..]),
        Ok(value) if !value.is_empty() => PathBuf::from(value),
        Ok(_) | Err(std::env::VarError::NotPresent) => home()?.join(".pi").join("agent"),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err("PI_CODING_AGENT_DIR must be UTF-8".into());
        }
    };
    if !path.is_absolute() {
        return Err("PI_CODING_AGENT_DIR must be absolute".into());
    }
    if path
        .to_str()
        .is_none_or(|s| s.chars().any(char::is_control))
    {
        return Err("the pi agent directory must be UTF-8 without control characters".into());
    }
    Ok(path)
}

fn extension_path() -> Result<PathBuf, String> {
    Ok(agent_dir()?.join("extensions").join(EXTENSION_FILE))
}

/// `pi-install` and `pi-uninstall`: write or remove only the crew's own extension file.
pub fn install(remove: bool) -> Result<(), String> {
    let path = extension_path()?;
    if remove {
        match fs::read_to_string(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
            Ok(text) if !text.contains(MARKER) => {
                return Err(format!(
                    "{} was not installed by herdr-crew; leaving it alone",
                    path.display()
                ));
            }
            Ok(_) => {}
        }
        fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        println!(
            "herdr-crew: removed the crew pi extension {}",
            path.display()
        );
        return Ok(());
    }
    let dir = path.parent().unwrap();
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    // The extensions directory may be a symlink (a dotfile manager's); pi follows it, and so does
    // this, but the file itself must not be one.
    let dir = dir
        .canonicalize()
        .map_err(|e| format!("pi extensions directory: {e}"))?;
    let path = dir.join(EXTENSION_FILE);
    match fs::read(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", path.display())),
        Ok(bytes) if !String::from_utf8_lossy(&bytes).contains(MARKER) => {
            return Err(format!(
                "{} exists and was not installed by herdr-crew; move it away first",
                path.display()
            ));
        }
        Ok(_) => {}
    }
    files::write(&dir, &path, EXTENSION)?;
    println!(
        "herdr-crew: installed the crew pi extension in {}. pi loads it on its next start; run \
         herdr-crew check.",
        path.display()
    );
    Ok(())
}

/// Before a pi role starts: the installed extension must be exactly this version's.
pub fn preflight() -> Result<(), String> {
    let path = extension_path()?;
    match fs::read_to_string(&path) {
        Ok(text) if text == EXTENSION => Ok(()),
        Ok(_) => Err(format!(
            "the crew pi extension {} is outdated or modified; run `herdr-crew pi-install`",
            path.display()
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(format!(
            "pi roles need the crew pi extension, which keeps the role prompt across herdr \
             restores; run `herdr-crew pi-install` (it writes {})",
            path.display()
        )),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// `program`, the `pi` that herdr starts, must be recent enough for herdr to resume it.
pub fn require_version(program: impl AsRef<std::ffi::OsStr>) -> Result<(u32, u32, u32), String> {
    let found = process::detect(program).map_err(|e| format!("pi --version: {e}"))?;
    if found < MIN_PI {
        let (a, b, c) = found;
        let (x, y, z) = MIN_PI;
        return Err(format!(
            "pi {a}.{b}.{c} is too old: {x}.{y}.{z} or newer is required, or herdr cannot resume \
             pi sessions (update: pi update, or npm install -g {PACKAGE})"
        ));
    }
    Ok(found)
}

/// The npm package that ships pi.
pub const PACKAGE: &str = "@earendil-works/pi-coding-agent";

/// Why a rendered role prompt cannot be handed to the extension, if it cannot.
pub fn prompt_problem(name: &str, prompt: &str) -> Option<String> {
    (prompt.trim().is_empty() || prompt.len() > MAX_PROMPT).then(|| {
        format!(
            "the prompt of {name} must be nonempty and at most {MAX_PROMPT} bytes for pi \
             ({} bytes)",
            prompt.len()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_extension_carries_its_marker_and_the_shared_bound() {
        assert!(EXTENSION.contains(MARKER));
        assert!(EXTENSION.contains(&format!("MAX_PROMPT = {} * 1024", MAX_PROMPT / 1024)));
        assert!(EXTENSION.contains("registerFlag(\"herdr-crew-prompt\""));
    }

    #[test]
    fn prompts_are_bounded() {
        assert!(prompt_problem("dev", "You are dev.").is_none());
        assert!(prompt_problem("dev", " \n").is_some());
        assert!(prompt_problem("dev", &"x".repeat(MAX_PROMPT + 1)).is_some());
    }
}
