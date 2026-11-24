//! `app-install` and `app-uninstall`: the Herdr app mod `herdr-crew` (a "Crew" dialog that
//! brings the focused space's crew up on herdr-core; see docs/herdr-core.md).
//!
//! The mod lives in `$HERDR_APP_HOME/mods/herdr-crew/`, by default under
//! `~/.config/herdr-app`. Like the pi extension, crew writes only its own folder and leaves
//! one it did not install alone.

use std::fs;
use std::path::{Path, PathBuf};

use crate::files;

const MOD_NAME: &str = "herdr-crew";
const MAIN: &str = include_str!("app/main.js");
const MANIFEST: &str = include_str!("app/mod.json");
/// In `main.js`: marks the folder as crew's.
const MARKER: &str = "HERDR_CREW_APP_MOD=1";
/// In `main.js`: replaced by the installing binary's path.
const PLACEHOLDER: &str = "const CREW = \"herdr-crew\";";

/// The Herdr app's home: `HERDR_APP_HOME`, else `~/.config/herdr-app`.
fn app_home() -> Result<PathBuf, String> {
    let home = match std::env::var_os("HERDR_APP_HOME").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => {
            let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
            PathBuf::from(home).join(".config").join("herdr-app")
        }
    };
    if !home.is_absolute() {
        return Err("HERDR_APP_HOME must be absolute".into());
    }
    Ok(home)
}

/// `main.js` with the path of the binary the mod runs.
fn main_js(crew: &Path) -> Result<String, String> {
    let path = crew
        .to_str()
        .filter(|s| !s.chars().any(char::is_control))
        .ok_or("the herdr-crew path must be UTF-8 without control characters")?;
    let literal = serde_json::to_string(path).map_err(|e| e.to_string())?;
    Ok(MAIN.replace(PLACEHOLDER, &format!("const CREW = {literal};")))
}

/// Whether `dir` holds a mod crew did not install.
fn foreign(dir: &Path) -> Result<bool, String> {
    match fs::read_to_string(dir.join("main.js")) {
        Ok(text) => Ok(!text.contains(MARKER)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(dir.exists()),
        Err(e) => Err(format!("{}: {e}", dir.join("main.js").display())),
    }
}

pub fn install(remove: bool, crew: &Path) -> Result<(), String> {
    let mods = app_home()?.join("mods");
    let dir = mods.join(MOD_NAME);
    if dir.is_symlink() {
        return Err(format!(
            "{} is a symbolic link; leaving it alone",
            dir.display()
        ));
    }
    if remove {
        if !dir.exists() {
            return Ok(());
        }
        if foreign(&dir)? {
            return Err(format!(
                "{} was not installed by herdr-crew; leaving it alone",
                dir.display()
            ));
        }
        for file in ["main.js", "mod.json"] {
            let path = dir.join(file);
            if path.exists() {
                fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            }
        }
        fs::remove_dir(&dir).map_err(|e| {
            format!(
                "{}: {e} (other files remain; remove them by hand)",
                dir.display()
            )
        })?;
        println!("herdr-crew: removed the Herdr app mod {}", dir.display());
        return Ok(());
    }
    if foreign(&dir)? {
        return Err(format!(
            "{} exists and was not installed by herdr-crew; move it away first",
            dir.display()
        ));
    }
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    files::write(&dir, &dir.join("main.js"), &main_js(crew)?)?;
    files::write(&dir, &dir.join("mod.json"), MANIFEST)?;
    println!(
        "herdr-crew: installed the Herdr app mod {} (running {}).\n\
         herdr-crew: open it with the command `mod.open herdr-crew`, or bind a key to it in \
         keys.json: \"ctrl-shift-u\": {{\"command\": \"mod.open\", \"args\": {{\"name\": \"herdr-crew\"}}}}.\n\
         herdr-crew: check it with `herdr-app check {}`; run app-install again after moving \
         herdr-crew.",
        dir.display(),
        crew.display(),
        dir.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mod_runs_the_installing_binary() {
        assert!(MAIN.contains(PLACEHOLDER), "the placeholder is in main.js");
        assert!(MAIN.contains(MARKER));
        let js = main_js(Path::new("/opt/it's \"here\"/herdr-crew")).unwrap();
        assert!(
            js.contains(r#"const CREW = "/opt/it's \"here\"/herdr-crew";"#),
            "{js}"
        );
        assert!(!js.contains(PLACEHOLDER));
        assert!(main_js(Path::new("/bad\npath")).is_err());
    }

    #[test]
    fn the_manifest_is_a_dialog() {
        let v: serde_json::Value = serde_json::from_str(MANIFEST).unwrap();
        assert_eq!(v["overlay"]["kind"], "dialog");
        assert_eq!(v["place"], "none");
    }
}
