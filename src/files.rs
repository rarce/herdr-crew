//! Project-owned paths and file replacement. Never follow project-local symlinks when writing.

use std::fs;
#[cfg(not(unix))]
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{config::Config, prompt};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
pub enum Kind {
    File,
    Directory,
}

/// Filesystem preflight for every generated-file location, before contacting the server.
pub fn validate_config(c: &Config) -> Result<(), String> {
    validate(&c.root, &c.board_path(), Kind::File)?;
    validate(&c.root, &c.root.join(prompt::SCHEMA_FILE), Kind::File)?;
    validate(&c.root, &c.root.join(prompt::PROMPTS_DIR), Kind::Directory)?;
    for role in &c.roles {
        validate(&c.root, &prompt::prompt_path(c, &role.name), Kind::File)?;
    }
    if let Some(worktrees) = &c.worktrees {
        validate(&c.root, &c.root.join(&worktrees.dir), Kind::Directory)?;
    }
    Ok(())
}

/// Pure configuration check; a path must name something strictly inside the checkout.
pub fn relative_path_problem(path: &str) -> Option<&'static str> {
    let path = Path::new(path);
    if path.is_absolute()
        || path.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        Some("must be relative to the project, without ..")
    } else if !path.components().any(|c| matches!(c, Component::Normal(_))) {
        Some("must name a path inside the project")
    } else if path
        .components()
        .any(|c| matches!(c, Component::Normal(name) if name.eq_ignore_ascii_case(".git")))
    {
        Some("must not point inside Git metadata (.git)")
    } else {
        None
    }
}

/// Inspect every existing component below the root, including dangling links. Missing paths
/// are allowed so preflight does not create anything. Canonicalizing the root permits aliases
/// such as /tmp on macOS without permitting links inside the project.
pub fn validate(root: &Path, path: &Path, kind: Kind) -> Result<PathBuf, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| format!("{} is outside the project", path.display()))?;
    let text = relative.to_str().ok_or("non-UTF-8 project path")?;
    if let Some(why) = relative_path_problem(text) {
        return Err(format!("{}: {why}", path.display()));
    }
    let mut current = root.canonicalize().map_err(|e| e.to_string())?;
    let components: Vec<_> = relative
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .collect();
    for (i, component) in components.iter().enumerate() {
        current.push(component.as_os_str());
        let directory = i + 1 < components.len() || matches!(kind, Kind::Directory);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(format!("{} must not be a symlink", current.display()));
                }
                if directory && !metadata.is_dir() || !directory && !metadata.is_file() {
                    return Err(format!(
                        "{} must be a regular {}",
                        current.display(),
                        if directory { "directory" } else { "file" }
                    ));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("could not inspect {}: {e}", current.display())),
        }
    }
    Ok(current)
}

#[cfg(not(unix))]
fn create_parents(root: &Path, path: &Path) -> Result<(), String> {
    let parent = path.parent().ok_or("file has no parent")?;
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let mut current = root.clone();
    for component in parent
        .strip_prefix(&root)
        .map_err(|e| e.to_string())?
        .components()
    {
        current.push(component.as_os_str());
        match fs::create_dir(&current) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("could not create {}: {e}", current.display())),
        }
        validate(&root, &current, Kind::Directory)?;
    }
    Ok(())
}

/// Replace all generated files atomically, including prompts. A fresh exclusive temporary
/// avoids following .tmp links and avoids truncating existing files or hardlink targets.
pub fn write(root: &Path, path: &Path, content: &str) -> Result<(), String> {
    let destination = validate(root, path, Kind::File)?;
    #[cfg(unix)]
    {
        let parent = open_parent(root, &destination)
            .map_err(|e| format!("could not open directory for {}: {e}", path.display()))?;
        write_in_directory(
            &parent,
            destination.file_name().ok_or("file has no name")?,
            content,
        )
        .map_err(|e| format!("could not write {}: {e}", path.display()))
    }
    #[cfg(not(unix))]
    write_by_path(root, path, &destination, content)
}

/// Open one directory component at a time relative to the checkout. The returned descriptor
/// remains anchored even if another process replaces a parent path with a symlink.
#[cfg(unix)]
fn open_parent(root: &Path, destination: &Path) -> io::Result<std::os::fd::OwnedFd> {
    use rustix::fs::{Mode, OFlags, mkdirat, open, openat};
    use rustix::io::Errno;

    let root = root.canonicalize()?;
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("file has no parent"))?;
    let relative = parent.strip_prefix(&root).map_err(io::Error::other)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = open(&root, flags, Mode::empty())?;
    for component in relative.components() {
        let name = component.as_os_str();
        directory = match openat(&directory, name, flags, Mode::empty()) {
            Ok(child) => child,
            Err(Errno::NOENT) => {
                match mkdirat(&directory, name, Mode::from_raw_mode(0o755)) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(e) => return Err(e.into()),
                }
                openat(&directory, name, flags, Mode::empty())?
            }
            Err(e) => return Err(e.into()),
        };
    }
    Ok(directory)
}

#[cfg(unix)]
fn write_in_directory(
    directory: &std::os::fd::OwnedFd,
    name: &std::ffi::OsStr,
    content: &str,
) -> io::Result<()> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags, openat, renameat, statat, unlinkat};
    use rustix::io::Errno;

    let (temporary, fd) = loop {
        let temporary = format!(
            ".herdr-crew-{}-{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        );
        match openat(
            directory,
            &temporary,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o644),
        ) {
            Ok(fd) => break (temporary, fd),
            Err(Errno::EXIST) => continue,
            Err(e) => return Err(e.into()),
        }
    };
    let mut file = fs::File::from(fd);
    let result = (|| -> io::Result<()> {
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        match statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile => {}
            Err(Errno::NOENT) => {}
            Ok(_) => {
                return Err(io::Error::other(
                    "destination must be a regular file, without symlinks",
                ));
            }
            Err(e) => return Err(e.into()),
        }
        renameat(directory, &temporary, directory, name)?;
        Ok(())
    })();
    let _ = unlinkat(directory, &temporary, AtFlags::empty());
    result
}

#[cfg(not(unix))]
fn write_by_path(
    root: &Path,
    path: &Path,
    destination: &Path,
    content: &str,
) -> Result<(), String> {
    create_parents(root, destination)?;
    let temporary = destination
        .parent()
        .ok_or("file has no parent")?
        .join(format!(
            ".herdr-crew-{}-{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|e| format!("could not create {}: {e}", temporary.display()))?;
    let result = (|| -> Result<(), String> {
        file.write_all(content.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string())?;
        validate(root, path, Kind::File)?;
        fs::rename(&temporary, &destination).map_err(|e| e.to_string())
    })();
    let _ = fs::remove_file(&temporary);
    result.map_err(|e| format!("could not write {}: {e}", path.display()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "crew-files-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn replacing_parent_directory_does_not_redirect_writes_or_cleanup() {
        let f = Fixture::new();
        let parent = f.0.join("generated");
        let destination = parent.join("status.json");
        let directory = open_parent(&f.0, &destination).unwrap();
        let outside = f.0.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("status.json"), "keep").unwrap();
        let moved = f.0.join("moved");
        fs::rename(&parent, &moved).unwrap();
        symlink(&outside, &parent).unwrap();
        write_in_directory(&directory, "status.json".as_ref(), "new").unwrap();
        assert_eq!(
            fs::read_to_string(outside.join("status.json")).unwrap(),
            "keep"
        );
        assert_eq!(
            fs::read_to_string(moved.join("status.json")).unwrap(),
            "new"
        );
        assert_eq!(fs::read_dir(&moved).unwrap().count(), 1);
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
        assert!(
            write(&f.0, &destination, "again")
                .unwrap_err()
                .contains("symlink")
        );
    }

    #[test]
    fn failed_replacement_preserves_destination_and_removes_temporary_file() {
        let f = Fixture::new();
        let destination = f.0.join("generated/status.json");
        let directory = open_parent(&f.0, &destination).unwrap();
        fs::create_dir(&destination).unwrap();
        assert!(write_in_directory(&directory, "status.json".as_ref(), "new").is_err());
        assert!(destination.is_dir());
        assert_eq!(
            fs::read_dir(destination.parent().unwrap()).unwrap().count(),
            1
        );
    }
}
