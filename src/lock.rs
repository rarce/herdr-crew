//! One herdr-crew command at a time changes a project (adapter over an OS file lock).
//!
//! The lock file lives in the repository's common Git directory, so it is shared by the main
//! checkout and its worktrees and never shows up in `git status`. The operating system releases
//! the lock when the process exits, even after a crash, so there is nothing stale to clean up.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::git;

const FILE: &str = "herdr-crew.lock";

/// Held while a command changes the project; dropping it releases the lock.
pub struct ProjectLock {
    _file: File,
}

#[derive(Debug)]
pub enum Busy {
    /// Another command still held the lock when the wait ended.
    Held,
    Error(String),
}

impl ProjectLock {
    /// Takes the project's lock, waiting up to `wait` while another command holds it.
    /// `on_wait` runs once, the first time the lock is found busy.
    pub fn acquire(root: &Path, wait: Duration, on_wait: impl FnOnce()) -> Result<Self, Busy> {
        let path = git::common_dir(root).map_err(Busy::Error)?.join(FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| Busy::Error(format!("cannot open {}: {e}", path.display())))?;
        let start = Instant::now();
        let mut on_wait = Some(on_wait);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(TryLockError::WouldBlock) if start.elapsed() < wait => {
                    if let Some(f) = on_wait.take() {
                        f();
                    }
                    sleep(Duration::from_millis(100));
                }
                Err(TryLockError::WouldBlock) => return Err(Busy::Held),
                Err(TryLockError::Error(e)) => {
                    return Err(Busy::Error(format!("cannot lock {}: {e}", path.display())));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn repo(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("crew-lock-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let status = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&dir)
            .status()
            .unwrap();
        assert!(status.success());
        dir
    }

    #[test]
    fn a_second_command_waits_then_gives_up_and_succeeds_once_released() {
        let root = repo("wait");
        let first = ProjectLock::acquire(&root, Duration::ZERO, || {}).unwrap();
        let mut waited = 0;
        let start = Instant::now();
        let second = ProjectLock::acquire(&root, Duration::from_millis(300), || waited += 1);
        assert!(matches!(second, Err(Busy::Held)));
        assert!(start.elapsed() >= Duration::from_millis(300));
        assert_eq!(waited, 1, "the wait is announced once");
        assert!(matches!(
            ProjectLock::acquire(&root, Duration::ZERO, || {}),
            Err(Busy::Held)
        ));
        drop(first);
        ProjectLock::acquire(&root, Duration::ZERO, || panic!("not busy")).unwrap();
        assert!(root.join(".git").join(FILE).is_file());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
