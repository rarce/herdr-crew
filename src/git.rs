//! Main checkout root, fetch and worktrees (adapter over `git`).

use std::collections::BTreeSet;
use std::io::Write;
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

/// The repository's common Git directory, shared by the main checkout and its worktrees.
pub fn common_dir(root: &Path) -> Result<PathBuf, String> {
    git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .map(PathBuf::from)
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
///
/// The base is saved in a versioned `crew.toml`, so it prefers a default branch over the
/// current branch's upstream: a feature branch would become the base of every future
/// worktree. The upstream wins only when it is its remote's recorded default branch, or is
/// named `main` or `master` on a remote without one (a fork's `upstream/main`, say), and
/// otherwise is just a fallback before an arbitrary branch.
pub fn default_base(root: &Path) -> Option<String> {
    let refs = git(
        root,
        &[
            "for-each-ref",
            "--format=%(refname)%09%(symref)",
            "refs/remotes",
        ],
    )
    .ok()?;
    let mut branches = Vec::new();
    // (remote, its default branch) for each remote with a recorded `<remote>/HEAD`.
    let mut heads: Vec<(String, String)> = Vec::new();
    for line in refs.lines() {
        let (name, target) = line.split_once('\t').unwrap_or((line, ""));
        let Some(name) = name.strip_prefix("refs/remotes/") else {
            continue;
        };
        if let Some(remote) = name.strip_suffix("/HEAD") {
            if let Some(target) = target.strip_prefix("refs/remotes/") {
                heads.push((remote.to_string(), target.to_string()));
            }
        } else {
            branches.push(name.to_string());
        }
    }
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
    let looks_default = |base: &String| {
        match heads.iter().find(|(remote, _)| {
            base.strip_prefix(remote.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
        }) {
            // A recorded default, such as `develop`, beats a stale or release `master`.
            Some((_, default)) => default == base,
            None => base
                .split_once('/')
                .is_some_and(|(_, branch)| matches!(branch, "main" | "master")),
        }
    };
    let origin_head = heads
        .iter()
        .find(|(remote, _)| remote == "origin")
        .map(|(_, default)| default.clone());
    upstream
        .iter()
        .filter(|u| looks_default(u))
        .cloned()
        .chain(origin_head)
        .chain(["origin/main".into(), "origin/master".into()])
        .chain(upstream.clone())
        .find(|candidate| branches.contains(candidate))
        .or_else(|| branches.first().cloned())
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

/// The ignore rule that decides a path, as Git reports it.
#[derive(Debug, PartialEq, Eq)]
pub struct IgnoreRule {
    /// The file holding the rule: relative to the root for the repository's own files,
    /// absolute for a global excludes file.
    pub source: String,
    pub line: String,
    pub pattern: String,
}

impl IgnoreRule {
    /// A negated pattern (`!rule`) matches a path in order to keep it versioned.
    pub fn ignores(&self) -> bool {
        !self.pattern.starts_with('!')
    }

    /// Whether the rule lives in a `.gitignore` that the repository shares, as opposed to
    /// `.git/info/exclude` or an excludes file that other clones do not have. Git reports a
    /// relative `core.excludesFile` relative too, so only the root `.gitignore`, which setup
    /// itself writes, or a tracked `.gitignore` in a subdirectory counts.
    pub fn shared(&self, root: &Path) -> bool {
        let source = Path::new(&self.source);
        if source.file_name() != Some(".gitignore".as_ref())
            || !source
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
        {
            return false;
        }
        source == Path::new(".gitignore")
            || git(root, &["ls-files", "--error-unmatch", "--", &self.source]).is_ok()
    }
}

/// The rule deciding each of `paths` (relative to `root`), or `None` when no rule matches.
/// Tracked files are judged by the rules too, and nothing needs to exist on disk.
pub fn ignore_rules(root: &Path, paths: &[&str]) -> Result<Vec<Option<IgnoreRule>>, String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "check-ignore",
            "--no-index",
            "--verbose",
            "--non-matching",
            "--stdin",
            "-z",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run git: {e}"))?;
    let input: String = paths.iter().map(|path| format!("{path}\0")).collect();
    // A few short paths fit in the pipe buffer, so writing before reading cannot block. If
    // git exits early the pipe breaks; its exit status and stderr below explain why.
    match child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(input.as_bytes())
    {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => {
            return Err(format!("could not run git check-ignore: {e}"));
        }
        _ => {}
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("could not run git: {e}"))?;
    // Exit status 1 only means that no path is ignored.
    if !matches!(out.status.code(), Some(0 | 1)) {
        return Err(format!(
            "git check-ignore failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let fields: Vec<&str> = text.split('\0').collect();
    let mut rules: Vec<(&str, Option<IgnoreRule>)> = Vec::new();
    for [source, line, pattern, path] in fields.as_chunks::<4>().0 {
        rules.push((
            path,
            (!source.is_empty()).then(|| IgnoreRule {
                source: source.to_string(),
                line: line.to_string(),
                pattern: pattern.to_string(),
            }),
        ));
    }
    paths
        .iter()
        .map(|path| {
            let i = rules
                .iter()
                .position(|(p, _)| p == path)
                .ok_or_else(|| format!("git check-ignore did not report {path}"))?;
            Ok(rules.swap_remove(i).1)
        })
        .collect()
}

/// A throwaway repository with one commit, a configured `origin` and no network access.
#[cfg(test)]
pub fn test_repo(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("crew-git-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for args in [
        &["init", "--quiet"][..],
        &[
            "-c",
            "user.name=crew",
            "-c",
            "user.email=crew@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "start",
        ],
        &["checkout", "--quiet", "-b", "work"],
        // Keep the developer's global excludes out of ignore assertions.
        &["config", "core.excludesFile", "/dev/null"],
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/repo.git",
        ],
    ] {
        git(&dir, args).unwrap();
    }
    dir.canonicalize().unwrap()
}

/// Record `refs/remotes/<base>` at HEAD, as a fetch would.
#[cfg(test)]
pub fn test_remote_branch(root: &Path, base: &str) {
    git(
        root,
        &["update-ref", &format!("refs/remotes/{base}"), "HEAD"],
    )
    .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_base_prefers_the_default_branch_over_a_feature_upstream() {
        let root = test_repo("base-feature");
        test_remote_branch(&root, "origin/main");
        test_remote_branch(&root, "origin/feature/x");
        git(
            &root,
            &["branch", "--quiet", "--set-upstream-to", "origin/feature/x"],
        )
        .unwrap();
        assert_eq!(default_base(&root).as_deref(), Some("origin/main"));

        // origin's recorded default branch wins over the main/master guesses.
        test_remote_branch(&root, "origin/trunk");
        git(
            &root,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/trunk",
            ],
        )
        .unwrap();
        assert_eq!(default_base(&root).as_deref(), Some("origin/trunk"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn default_base_follows_an_upstream_that_looks_like_a_default_branch() {
        let root = test_repo("base-fork");
        git(
            &root,
            &[
                "remote",
                "add",
                "upstream",
                "https://example.invalid/up.git",
            ],
        )
        .unwrap();
        test_remote_branch(&root, "origin/main");
        test_remote_branch(&root, "upstream/main");
        git(
            &root,
            &["branch", "--quiet", "--set-upstream-to", "upstream/main"],
        )
        .unwrap();
        assert_eq!(default_base(&root).as_deref(), Some("upstream/main"));

        // Without a default-looking candidate, the upstream beats an arbitrary branch.
        let root2 = test_repo("base-fallback");
        test_remote_branch(&root2, "origin/aaa");
        test_remote_branch(&root2, "origin/feature/x");
        git(
            &root2,
            &["branch", "--quiet", "--set-upstream-to", "origin/feature/x"],
        )
        .unwrap();
        assert_eq!(default_base(&root2).as_deref(), Some("origin/feature/x"));
        for dir in [root, root2] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn a_recorded_default_branch_beats_an_upstream_named_master() {
        let root = test_repo("base-gitflow");
        test_remote_branch(&root, "origin/develop");
        test_remote_branch(&root, "origin/master");
        git(
            &root,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/develop",
            ],
        )
        .unwrap();
        git(
            &root,
            &["branch", "--quiet", "--set-upstream-to", "origin/master"],
        )
        .unwrap();
        assert_eq!(default_base(&root).as_deref(), Some("origin/develop"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn only_the_root_or_a_tracked_gitignore_is_shared() {
        let root = test_repo("ignore-shared");
        let rule = |source: &str| IgnoreRule {
            source: source.into(),
            line: "1".into(),
            pattern: ".herdr/".into(),
        };
        assert!(rule(".gitignore").shared(&root));
        // A relative core.excludesFile is reported relative to the root.
        assert!(!rule("../.gitignore").shared(&root));
        assert!(!rule(".git/info/exclude").shared(&root));
        assert!(!rule("/home/user/.gitignore").shared(&root));
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/.gitignore"), "*\n").unwrap();
        assert!(!rule("sub/.gitignore").shared(&root));
        git(&root, &["add", "--force", "sub/.gitignore"]).unwrap();
        assert!(rule("sub/.gitignore").shared(&root));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn ignore_rules_report_the_deciding_rule_and_where_it_lives() {
        let root = test_repo("ignore");
        std::fs::write(root.join(".gitignore"), "/.herdr/*\n!/.herdr/crew.toml\n").unwrap();
        std::fs::write(root.join(".git/info/exclude"), "/notes/\n").unwrap();
        let rules = ignore_rules(
            &root,
            &[
                ".herdr/status.json",
                ".herdr/crew.toml",
                "notes/",
                "src/main.rs",
            ],
        )
        .unwrap();
        let [status, config, notes, source] = &rules[..] else {
            panic!("{rules:?}")
        };
        let status = status.as_ref().unwrap();
        assert!(status.ignores() && status.shared(&root));
        assert_eq!(
            (status.line.as_str(), status.pattern.as_str()),
            ("1", "/.herdr/*")
        );
        let config = config.as_ref().unwrap();
        assert!(!config.ignores() && config.shared(&root));
        let notes = notes.as_ref().unwrap();
        assert!(notes.ignores() && !notes.shared(&root), "{notes:?}");
        assert_eq!(source, &None);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
