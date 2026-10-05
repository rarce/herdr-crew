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
    if git(dir, &["rev-parse", "--is-bare-repository"]).as_deref() == Ok("true") {
        return Err(format!(
            "{} is a bare repository; crew needs a working checkout",
            dir.display()
        ));
    }
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

/// Suggest a locally known remote branch without fetching or changing Git state.
pub fn default_base(root: &Path) -> Option<String> {
    let refs = git(
        root,
        &["for-each-ref", "--format=%(refname:short)", "refs/remotes"],
    )
    .ok()?;
    let branches: Vec<&str> = refs.lines().filter(|r| !r.ends_with("/HEAD")).collect();
    let upstream = git(
        root,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )
    .ok();
    let origin_head = git(
        root,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
    )
    .ok()
    .and_then(|r| r.strip_prefix("refs/remotes/").map(String::from));
    upstream
        .into_iter()
        .chain(origin_head)
        .chain(["origin/main".into(), "origin/master".into()])
        .find(|candidate| branches.contains(&candidate.as_str()))
        .or_else(|| branches.first().map(|r| r.to_string()))
}

/// Validate the remote and branch syntax used by the existing worktree planner, offline.
pub fn validate_base(root: &Path, base: &str) -> Result<(), String> {
    let (remote, branch) = base
        .split_once('/')
        .filter(|(remote, branch)| !remote.is_empty() && !branch.is_empty())
        .ok_or("use a configured remote and branch, such as origin/main")?;
    if remote.starts_with('-') || branch.starts_with('-') {
        return Err("remote and branch names cannot start with -".into());
    }
    let remotes = git(root, &["remote"])?;
    if !remotes.lines().any(|name| name == remote) {
        return Err(format!(
            "remote {remote:?} is not configured in this repository"
        ));
    }
    git(root, &["check-ref-format", &format!("refs/heads/{branch}")])
        .map_err(|_| format!("{branch:?} is not a valid Git branch name"))?;
    Ok(())
}

pub fn has_remote(root: &Path) -> Result<bool, String> {
    git(root, &["remote"]).map(|remotes| !remotes.is_empty())
}
