//! Main checkout root, fetch and worktrees (adapter over `git`).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{config::Config, files};

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if out.status.success() {
        let value = String::from_utf8_lossy(&out.stdout);
        Ok(value.strip_suffix('\n').unwrap_or(&value).to_string())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Resolve the main working checkout without treating Git metadata as a project directory.
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
    let unresolved = || {
        format!(
            "cannot locate the main checkout for {}; run crew from the main checkout",
            dir.display()
        )
    };
    let common = Path::new(&common)
        .canonicalize()
        .map_err(|_| unresolved())?;
    let git_dir = git(dir, &["rev-parse", "--path-format=absolute", "--git-dir"])?;
    let git_dir = Path::new(&git_dir)
        .canonicalize()
        .map_err(|_| unresolved())?;
    if git_dir == common {
        // Main checkouts and submodules can keep their metadata somewhere else entirely.
        return git(dir, &["rev-parse", "--show-toplevel"]).map(PathBuf::from);
    }

    // A linked worktree normally has its main checkout beside the shared .git directory.
    // Verify the candidate so relocated metadata cannot select an unrelated repository.
    let candidate = common.parent().ok_or_else(unresolved)?;
    let root = git(candidate, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .map_err(|_| unresolved())?;
    let root_git_dir = git(&root, &["rev-parse", "--path-format=absolute", "--git-dir"])
        .map_err(|_| unresolved())?;
    if Path::new(&root_git_dir)
        .canonicalize()
        .map_err(|_| unresolved())?
        != common
    {
        return Err(unresolved());
    }
    Ok(root)
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

fn registered_worktrees(root: &Path) -> Result<Vec<PathBuf>, String> {
    Ok(git(root, &["worktree", "list", "--porcelain", "-z"])?
        .split('\0')
        .filter_map(|field| field.strip_prefix("worktree ").map(PathBuf::from))
        .collect())
}

/// An existing role directory must be an exact registered worktree of this repository, not
/// merely a directory inside a checkout. Git metadata may live outside the main checkout.
pub fn validate_worktree(root: &Path, path: &Path) -> Result<(), String> {
    let path = files::validate(root, path, files::Kind::Directory)?;
    verify_linked_worktree(root, &path)
}

/// Workspace panes may be in subdirectories or registered linked checkouts, including ones
/// whose main Git metadata is relocated. The requested main root is already known here.
pub fn verify_checkout(root: &Path, directory: &Path) -> Result<(), String> {
    let top = git(directory, &["rev-parse", "--show-toplevel"])?;
    let top = Path::new(&top).canonicalize().map_err(|e| e.to_string())?;
    if top == root.canonicalize().map_err(|e| e.to_string())? {
        Ok(())
    } else {
        verify_linked_worktree(root, &top)
    }
}

fn verify_linked_worktree(root: &Path, path: &Path) -> Result<(), String> {
    let fail = || {
        format!(
            "{} is not a registered worktree of this project",
            path.display()
        )
    };
    let canonical = path.canonicalize().map_err(|_| fail())?;
    let marker = path.join(".git");
    if !std::fs::symlink_metadata(&marker).is_ok_and(|m| m.is_file()) {
        return Err(fail());
    }
    let registrations = registered_worktrees(root)?
        .iter()
        .filter(|registered| registered.canonicalize().is_ok_and(|p| p == canonical))
        .count();
    let top = git(path, &["rev-parse", "--show-toplevel"])
        .ok()
        .and_then(|p| Path::new(&p).canonicalize().ok());
    let common = |dir: &Path| -> Result<PathBuf, String> {
        let metadata = git(
            dir,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;
        Path::new(&metadata)
            .canonicalize()
            .map_err(|e| e.to_string())
    };
    if registrations != 1 || top.as_ref() != Some(&canonical) || common(path)? != common(root)? {
        return Err(fail());
    }
    // A registered path alone does not prove private metadata ownership: a modified .git
    // file can point at the main checkout or a sibling's index while sharing the same common
    // directory. Its metadata must point back to this exact .git file (absolute or relative).
    let metadata = git(path, &["rev-parse", "--path-format=absolute", "--git-dir"])?;
    let metadata = Path::new(&metadata);
    let backlink = std::fs::read_to_string(metadata.join("gitdir")).map_err(|_| fail())?;
    let backlink = backlink.strip_suffix('\n').unwrap_or(&backlink);
    if metadata.join(backlink).canonicalize().map_err(|_| fail())?
        != marker.canonicalize().map_err(|_| fail())?
    {
        return Err(fail());
    }
    Ok(())
}

/// Preflight existing base and extra role directories without creating or fetching anything.
pub fn existing_worktrees(c: &Config) -> Result<BTreeSet<String>, String> {
    let mut existing = BTreeSet::new();
    let Some(worktrees) = &c.worktrees else {
        return Ok(existing);
    };
    let directory = files::validate(
        &c.root,
        &c.root.join(&worktrees.dir),
        files::Kind::Directory,
    )?;
    // A missing directory can still have a stale Git registration. Reject it before creating
    // a workspace or asking Git to add the same path again.
    for registered in registered_worktrees(&c.root)? {
        if registered.parent() == Some(directory.as_path())
            && let Some(name) = registered.file_name().and_then(|n| n.to_str())
            && c.session_role(name).is_some_and(|r| r.worktree)
        {
            validate_worktree(&c.root, &c.root.join(&worktrees.dir).join(name))?;
        }
    }
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(existing),
        Err(e) => return Err(format!("could not read {}: {e}", directory.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if c.session_role(&name).is_some_and(|r| r.worktree) {
            validate_worktree(&c.root, &c.root.join(&worktrees.dir).join(&name))?;
            existing.insert(name);
        }
    }
    Ok(existing)
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

/// Whether `<remote>/<branch>` is already known locally, without fetching.
pub fn knows_remote_branch(root: &Path, remote: &str, branch: &str) -> bool {
    git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/remotes/{remote}/{branch}^{{commit}}"),
        ],
    )
    .is_ok()
}

pub fn has_remote(root: &Path) -> Result<bool, String> {
    git(root, &["remote"]).map(|remotes| !remotes.is_empty())
}
