//! Public CLI entry point. Resolves the registered plugin on every call; never starts herdr.
//! Installation deliberately copies this launcher, not a link into herdr's temporary checkout.

use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use serde_json::Value;

/// Embedded in our executable so installation never needs to execute an existing command.
const OWNER_MARKER: &str = "herdr-crew public launcher format 1 (rarce/herdr-crew)";
const HELP: &str = "herdr-crew forwards commands to the registered herdr-crew plugin.
  --install-launcher [--bin-dir DIR]    install this launcher as herdr-crew
  --uninstall-launcher [--bin-dir DIR]  remove an installed launcher owned by herdr-crew
  --launcher-help                      show this help without requiring the plugin
The install directory is CREW_BIN_DIR, or ~/.local/bin. The launcher never edits shell profiles.";

fn main() -> ExitCode {
    match run(std::env::args_os().skip(1).collect()) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("herdr-crew launcher: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<OsString>) -> Result<ExitCode, String> {
    let first = args.first().and_then(|s| s.to_str());
    match first {
        Some("--launcher-help") if args.len() == 1 => {
            println!("{HELP}");
            return Ok(ExitCode::SUCCESS);
        }
        Some("--launcher-id") if args.len() == 1 => {
            println!("{OWNER_MARKER}");
            return Ok(ExitCode::SUCCESS);
        }
        Some("--install-launcher" | "--uninstall-launcher") => {
            let uninstall = first == Some("--uninstall-launcher");
            let directory = bin_directory(&args[1..], uninstall)?;
            let destination = directory.join("herdr-crew");
            if uninstall {
                if remove_launcher(&destination)? {
                    println!("Removed {}", destination.display());
                } else {
                    println!("No launcher at {}", destination.display());
                }
            } else {
                let source = std::env::current_exe()
                    .map_err(|e| format!("cannot locate this launcher: {e}"))?;
                install_launcher(&source, &destination)?;
                println!("Installed {}", destination.display());
                if !directory_on_path(&directory) {
                    println!(
                        "Add {} to your PATH to run herdr-crew.",
                        directory.display()
                    );
                }
            }
            return Ok(ExitCode::SUCCESS);
        }
        _ => {}
    }

    let herdr = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let root = registered_root(&herdr)?;
    let binary = plugin_binary(&root)?;
    if let Ok(current) = std::env::current_exe().and_then(fs::canonicalize)
        && binary.canonicalize().is_ok_and(|target| current == target)
    {
        return Err("the registered plugin points back to this launcher".into());
    }
    let mut command = Command::new(&binary);
    command.args(args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(format!(
            "cannot execute {}: {}",
            binary.display(),
            command.exec()
        ))
    }
    #[cfg(not(unix))]
    {
        let status = command
            .status()
            .map_err(|e| format!("cannot execute {}: {e}", binary.display()))?;
        Ok(ExitCode::from(
            status
                .code()
                .and_then(|c| u8::try_from(c).ok())
                .unwrap_or(1),
        ))
    }
}

fn registered_root(herdr: &OsStr) -> Result<PathBuf, String> {
    let output = Command::new(herdr)
        .args(["plugin", "list", "--plugin", "herdr-crew", "--json"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run herdr to locate the plugin: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "herdr plugin list failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("invalid response from herdr plugin list: {e}"))?;
    if let Some(error) = value.get("error") {
        return Err(format!("herdr plugin list failed: {error}"));
    }
    let plugins = value["result"]["plugins"]
        .as_array()
        .ok_or("herdr plugin list did not return a plugins array")?;
    let plugin = plugins
        .iter()
        .find(|plugin| plugin["plugin_id"] == "herdr-crew")
        .ok_or("plugin not installed; run herdr plugin install rarce/herdr-crew")?;
    let root = plugin["plugin_root"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("herdr-crew has no plugin_root in herdr's response")?;
    let path = PathBuf::from(root);
    if !path.is_absolute() {
        return Err("herdr returned a relative plugin_root".into());
    }
    Ok(path)
}

fn plugin_binary(root: &Path) -> Result<PathBuf, String> {
    for relative in ["bin/herdr-crew", "target/release/herdr-crew"] {
        let path = root.join(relative);
        if path.is_file() {
            return Ok(path);
        }
    }
    Err(format!(
        "no crew binary in {}; reinstall the plugin or run sh scripts/build.sh in its linked checkout",
        root.display()
    ))
}

fn bin_directory(args: &[OsString], uninstall: bool) -> Result<PathBuf, String> {
    let explicit = match args {
        [] => None,
        [flag, path] if flag == "--bin-dir" && !path.is_empty() => Some(PathBuf::from(path)),
        _ => return Err(HELP.into()),
    };
    let directory = if let Some(path) = explicit {
        path
    } else if let Some(path) = std::env::var_os("CREW_BIN_DIR") {
        if path.is_empty() {
            return Err("CREW_BIN_DIR is empty".into());
        }
        PathBuf::from(path)
    } else if let Some(path) = uninstall
        .then(|| std::env::current_exe().ok())
        .flatten()
        .filter(|path| path.file_name() == Some(OsStr::new("herdr-crew")))
        .and_then(|path| path.parent().map(Path::to_path_buf))
    {
        path
    } else {
        PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set; use --bin-dir DIR")?)
            .join(".local/bin")
    };
    if directory.is_absolute() {
        Ok(directory)
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(directory))
            .map_err(|e| format!("cannot resolve install directory: {e}"))
    }
}

fn directory_on_path(directory: &Path) -> bool {
    let canonical = directory.canonicalize().ok();
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|path| {
            path == directory || canonical.is_some() && path.canonicalize().ok() == canonical
        })
    })
}

/// Return false only for an absent path. Do not follow or replace a user's symlink.
fn owned_launcher(destination: &Path) -> Result<bool, String> {
    let metadata = match fs::symlink_metadata(destination) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("cannot inspect {}: {error}", destination.display())),
    };
    if metadata.is_file() {
        let contents = fs::read(destination)
            .map_err(|e| format!("cannot read {}: {e}", destination.display()))?;
        if contents
            .windows(OWNER_MARKER.len())
            .any(|bytes| bytes == OWNER_MARKER.as_bytes())
        {
            return Ok(true);
        }
    }
    Err(format!(
        "refusing to replace or remove {}; it is not a herdr-crew launcher (choose another --bin-dir)",
        destination.display()
    ))
}

fn install_launcher(source: &Path, destination: &Path) -> Result<(), String> {
    owned_launcher(destination)?;
    let directory = destination.parent().ok_or("install path has no parent")?;
    fs::create_dir_all(directory)
        .map_err(|e| format!("cannot create {}: {e}", directory.display()))?;
    let temporary = directory.join(format!(".herdr-crew-launcher.{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|e| format!("cannot create {}: {e}", temporary.display()))?;
    let result = (|| -> io::Result<()> {
        let mut incoming = fs::File::open(source)?;
        io::copy(&mut incoming, &mut file)?;
        file.flush()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o755))?;
        }
        file.sync_all()?;
        // Check again before replacement in case another command appeared while we copied.
        owned_launcher(destination).map_err(io::Error::other)?;
        fs::rename(&temporary, destination)
    })();
    let _ = fs::remove_file(&temporary);
    result.map_err(|e| format!("cannot install {}: {e}", destination.display()))
}

fn remove_launcher(destination: &Path) -> Result<bool, String> {
    if !owned_launcher(destination)? {
        return Ok(false);
    }
    fs::remove_file(destination)
        .map_err(|e| format!("cannot remove {}: {e}", destination.display()))?;
    Ok(true)
}
