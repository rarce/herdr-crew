//! Main checkout root, fetch and worktrees (adapter over `git`).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// The main checkout that contains `dir`: the parent of the `git-common-dir`, also from a
/// worktree (design §2.1).
pub fn main_root(dir: &Path) -> Result<PathBuf, String> {
    let common = git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .map_err(|_| format!("{} is not inside a git repository", dir.display()))?;
    Path::new(&common)
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("git-common-dir without a parent: {common}"))
}

pub fn fetch(root: &Path, remote: &str, branch: &str) -> Result<(), String> {
    git(root, &["fetch", "--quiet", remote, branch]).map(|_| ())
}

pub fn worktree_add(root: &Path, path: &Path, base: &str) -> Result<(), String> {
    let path = path.to_str().ok_or("non-UTF-8 worktree path")?;
    git(
        root,
        &["worktree", "add", "--quiet", "--detach", path, base],
    )
    .map(|_| ())
}
