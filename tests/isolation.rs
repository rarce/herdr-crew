//! End-to-end safety checks with real temporary Git repositories and an offline herdr stub.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

const CREW: &str = env!("CARGO_BIN_EXE_herdr-crew");
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    other: PathBuf,
    tools: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = PathBuf::from("/tmp").join(format!(
            "crew-isolation-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let fixture = Self {
            repo: root.join("repo"),
            other: root.join("other"),
            tools: root.join("tools"),
            root,
        };
        for path in [&fixture.repo, &fixture.other] {
            fs::create_dir(path).unwrap();
            fixture.git_at(path, &["init", "--quiet", "-b", "main"]);
            fixture.git_at(
                path,
                &[
                    "-c",
                    "user.name=Crew test",
                    "-c",
                    "user.email=crew@example.invalid",
                    "commit",
                    "--quiet",
                    "--allow-empty",
                    "-m",
                    "Initial",
                ],
            );
        }
        fs::create_dir(&fixture.tools).unwrap();
        let herdr = fixture.tools.join("herdr");
        fs::write(&herdr, r#"#!/bin/sh
printf '%s\n' "$*" >> "$CREW_TEST_STATE/calls"
case "$1 $2" in
  'workspace list') cat "$CREW_TEST_STATE/workspaces.json" ;;
  'agent list') printf '{"result":{"agents":[]}}\n' ;;
  'tab list') cat "$CREW_TEST_STATE/tabs-$4.json" ;;
  'pane list') cat "$CREW_TEST_STATE/panes-$4.json" ;;
  'pane process-info') printf '{"result":{"process_info":{"shell_pid":1,"foreground_process_group_id":2,"foreground_processes":[{"name":"herdr-crew"}]}}}\n' ;;
  'workspace create') printf '{"result":{"workspace":{"workspace_id":"new"},"tab":{"tab_id":"new-tab"},"root_pane":{"pane_id":"new-pane"}}}\n' ;;
  'tab create') printf '{"result":{"root_pane":{"pane_id":"new-pane"}}}\n' ;;
  *) printf '{"result":{}}\n' ;;
esac
"#).unwrap();
        fs::set_permissions(&herdr, fs::Permissions::from_mode(0o755)).unwrap();
        fixture.configure(false);
        fixture.workspaces(&[("w1", "crew", &fixture.repo)]);
        fixture
    }

    fn configure(&self, worktree: bool) {
        let mut config = json!({
            "version": 1,
            "workspace": {"label": "crew"},
            "board": {"tab": "status", "writer": "dev"},
            "roles": [{"name": "dev", "prompt": "Developer {{NAME}}", "extra": true, "worktree": worktree}]
        });
        if worktree {
            config["worktrees"] = json!({"dir": ".worktrees", "base": "origin/main"});
        }
        self.save_config(&config);
    }

    fn save_config(&self, config: &Value) {
        fs::create_dir_all(self.repo.join(".herdr")).unwrap();
        fs::write(
            self.repo.join(".herdr/crew.toml"),
            toml::to_string(config).unwrap(),
        )
        .unwrap();
    }

    fn workspaces(&self, workspaces: &[(&str, &str, &Path)]) {
        let items: Vec<_> = workspaces
            .iter()
            .map(|(id, label, _)| json!({"workspace_id": id, "label": label}))
            .collect();
        self.response("workspaces.json", json!({"workspaces": items}));
        for (id, _, cwd) in workspaces {
            self.tabs(id, &["dev", "status", "dev-2"], cwd);
        }
    }

    fn tabs(&self, workspace: &str, labels: &[&str], cwd: &Path) {
        let tabs: Vec<_> = labels
            .iter()
            .enumerate()
            .map(|(i, label)| json!({"tab_id": format!("{workspace}:t{i}"), "label": label}))
            .collect();
        let panes: Vec<_> = labels.iter().enumerate().map(|(i, _)| json!({"pane_id": format!("{workspace}:p{i}"), "tab_id": format!("{workspace}:t{i}"), "cwd": cwd})).collect();
        self.response(&format!("tabs-{workspace}.json"), json!({"tabs": tabs}));
        self.response(&format!("panes-{workspace}.json"), json!({"panes": panes}));
    }

    fn response(&self, name: &str, result: Value) {
        fs::write(self.root.join(name), json!({"result": result}).to_string()).unwrap();
    }

    fn command(&self) -> Command {
        let mut command = Command::new(CREW);
        command
            .env_clear()
            .env("HOME", &self.root)
            .env("PATH", format!("{}:/usr/bin:/bin", self.tools.display()))
            .env("HERDR_BIN_PATH", self.tools.join("herdr"))
            .env("CREW_TEST_STATE", &self.root)
            .current_dir(&self.repo);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn git_at(&self, path: &Path, args: &[&str]) -> String {
        success(
            Command::new("git")
                .env("HOME", &self.root)
                .arg("-C")
                .arg(path)
                .args(args)
                .output()
                .unwrap(),
        )
    }

    fn git(&self, args: &[&str]) -> String {
        self.git_at(&self.repo, args)
    }

    fn no_mutations(&self) {
        let calls = fs::read_to_string(self.root.join("calls")).unwrap_or_default();
        for call in calls.lines() {
            assert!(
                [
                    "workspace list",
                    "agent list",
                    "tab list",
                    "pane list",
                    "pane process-info"
                ]
                .iter()
                .any(|prefix| call.starts_with(prefix)),
                "unexpected mutation: {call}"
            );
        }
        assert!(!self.repo.join(".herdr/status.schema.json").exists());
        assert!(!self.repo.join(".herdr/status.json").exists());
        assert!(!self.repo.join(".herdr/prompts").exists());
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn failure(output: Output, message: &str) {
    assert!(
        !output.status.success(),
        "unexpected success: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains(message), "expected {message:?}: {error}");
}

#[test]
fn foreign_workspace_is_rejected_by_every_mutating_command_and_dry_run() {
    let f = Fixture::new();
    // The differently labelled workspace lets startup discover this project's configuration.
    f.workspaces(&[("w1", "old-label", &f.repo), ("w2", "crew", &f.other)]);
    for args in [
        vec!["up", "--no-attach"],
        vec!["up", "--dry-run"],
        vec!["add", "dev"],
        vec!["close", "dev-2"],
        vec!["startup"],
    ] {
        failure(f.run(&args), "cannot be verified");
        f.no_mutations();
    }
}

#[test]
fn duplicate_labels_are_rejected_without_mutations() {
    let f = Fixture::new();
    f.workspaces(&[("w1", "crew", &f.repo), ("w2", "crew", &f.repo)]);
    for args in [
        vec!["up", "--no-attach"],
        vec!["add", "dev"],
        vec!["close", "dev-2"],
        vec!["startup"],
    ] {
        failure(f.run(&args), "ambiguous");
        f.no_mutations();
    }
}

#[test]
fn unknown_pane_directory_and_foreign_known_tab_fail_closed() {
    let f = Fixture::new();
    for panes in [
        json!([{"pane_id":"p1", "tab_id":"w1:t0"}]),
        json!([
            {"pane_id":"p1", "tab_id":"w1:t0", "cwd": f.repo},
            {"pane_id":"p2", "tab_id":"w1:t1", "cwd": f.other}
        ]),
    ] {
        f.response("panes-w1.json", json!({"panes": panes}));
        failure(f.run(&["up", "--no-attach"]), "cannot be verified");
        f.no_mutations();
    }
}

#[test]
fn escaping_board_and_worktree_paths_fail_before_contacting_herdr() {
    let f = Fixture::new();
    for value in [
        "../outside.json".to_string(),
        f.root.join("outside.json").display().to_string(),
        ".git/hooks/generated".into(),
    ] {
        for key in ["board", "worktrees"] {
            let mut config = json!({"version":1,"workspace":{"label":"crew"},"board":{"tab":"status","writer":"dev"},"roles":[{"name":"dev","prompt":"Developer"}]});
            if key == "board" {
                config["board"]["file"] = json!(value);
            } else {
                config["worktrees"] = json!({"dir": value, "base":"origin/main"});
            }
            f.save_config(&config);
            failure(
                f.run(&["up", "--no-attach"]),
                if value.contains(".git") {
                    "Git metadata"
                } else {
                    "without .."
                },
            );
            assert!(!f.root.join("calls").exists());
            f.no_mutations();
        }
    }
    assert!(!f.root.join("outside.json").exists());
}

#[test]
fn symlinked_output_files_and_parent_directories_are_preserved() {
    let f = Fixture::new();
    let victim = f.other.join("keep.txt");
    fs::write(&victim, "keep").unwrap();
    for relative in [
        ".herdr/status.json",
        ".herdr/status.schema.json",
        ".herdr/prompts/dev.txt",
    ] {
        let link = f.repo.join(relative);
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(&victim, &link).unwrap();
        failure(f.run(&["up", "--no-attach"]), "symlink");
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(!f.root.join("calls").exists());
        fs::remove_file(link).unwrap();
    }
    fs::remove_dir(f.repo.join(".herdr/prompts")).unwrap();
    symlink(&f.other, f.repo.join(".herdr/prompts")).unwrap();
    failure(f.run(&["up", "--no-attach"]), "symlink");
    assert!(!f.other.join("dev.txt").exists());
    assert!(!f.root.join("calls").exists());
}

#[test]
fn legacy_temporary_symlinks_and_hardlinks_do_not_overwrite_their_targets() {
    let f = Fixture::new();
    f.tabs("w1", &["status"], &f.repo);
    let victim = f.other.join("keep.txt");
    fs::write(&victim, "keep").unwrap();
    for name in ["status.json", "status.schema.json"] {
        symlink(&victim, f.repo.join(format!(".herdr/{name}.tmp"))).unwrap();
    }
    fs::create_dir(f.repo.join(".herdr/prompts")).unwrap();
    fs::hard_link(&victim, f.repo.join(".herdr/prompts/dev.txt")).unwrap();
    success(f.run(&["up", "--no-attach"]));
    assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
    assert!(
        fs::read_to_string(f.repo.join(".herdr/prompts/dev.txt"))
            .unwrap()
            .contains("Developer dev")
    );
    for name in ["status.json", "status.schema.json"] {
        assert!(
            !fs::symlink_metadata(f.repo.join(format!(".herdr/{name}")))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        serde_json::from_str::<Value>(
            &fs::read_to_string(f.repo.join(format!(".herdr/{name}"))).unwrap(),
        )
        .unwrap();
    }
    assert!(fs::read_dir(f.repo.join(".herdr")).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".herdr-crew-")
    }));
}

#[test]
fn invalid_existing_role_directories_fail_before_any_server_or_git_mutation() {
    let f = Fixture::new();
    f.configure(true);
    let directory = f.repo.join(".worktrees");
    fs::create_dir(&directory).unwrap();
    let path = directory.join("dev");
    for kind in [
        "empty",
        "file",
        "symlink",
        "foreign-repository",
        "unregistered-copy",
    ] {
        match kind {
            "empty" => fs::create_dir(&path).unwrap(),
            "file" => fs::write(&path, "keep").unwrap(),
            "symlink" => symlink(&f.other, &path).unwrap(),
            "foreign-repository" => {
                fs::create_dir(&path).unwrap();
                f.git_at(&path, &["init", "--quiet"]);
            }
            "unregistered-copy" => {
                fs::create_dir(&path).unwrap();
                fs::write(
                    path.join(".git"),
                    format!("gitdir: {}\n", f.repo.join(".git").display()),
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        for args in [
            vec!["up", "--no-attach"],
            vec!["up", "--dry-run"],
            vec!["add", "dev"],
        ] {
            let output = f.run(&args);
            assert!(!output.status.success(), "accepted {kind}");
            assert!(!f.root.join("calls").exists());
            f.no_mutations();
        }
        assert_eq!(
            f.git(&["worktree", "list", "--porcelain"])
                .matches("worktree ")
                .count(),
            1
        );
        if kind == "file" || kind == "symlink" {
            fs::remove_file(&path).unwrap();
        } else {
            fs::remove_dir_all(&path).unwrap();
        }
    }
}

#[test]
fn registered_worktrees_are_reused_and_identify_their_project() {
    let f = Fixture::new();
    f.configure(true);
    let path = f.repo.join(".worktrees/dev");
    f.git(&[
        "worktree",
        "add",
        "--quiet",
        "--detach",
        path.to_str().unwrap(),
        "HEAD",
    ]);
    // Startup and up can verify a workspace whose first role lives in a linked worktree.
    f.tabs("w1", &["dev", "status"], &path);
    let plan = success(f.run(&["up", "--dry-run"]));
    assert!(!plan.contains("worktree of"));
    let extra = f.repo.join(".worktrees/dev-2");
    f.git(&[
        "worktree",
        "add",
        "--quiet",
        "--detach",
        extra.to_str().unwrap(),
        "HEAD",
    ]);
    let head = f.git(&["rev-parse", "HEAD"]);
    success(f.run(&["add", "dev"]));
    assert_eq!(head, f.git(&["rev-parse", "HEAD"]));
    assert!(extra.join(".git").is_file());
    assert!(!f.git(&["status", "--porcelain"]).contains("unexpected"));
    let calls = fs::read_to_string(f.root.join("calls")).unwrap();
    assert!(calls.contains(&format!(
        "--cwd {}",
        extra.canonicalize().unwrap().display()
    )));
    assert!(calls.contains("agent start dev-2"));
}

#[test]
fn stale_worktree_registration_is_rejected_before_server_changes() {
    let f = Fixture::new();
    f.configure(true);
    let path = f.repo.join(".worktrees/dev");
    f.git(&[
        "worktree",
        "add",
        "--quiet",
        "--detach",
        path.to_str().unwrap(),
        "HEAD",
    ]);
    fs::remove_dir_all(&path).unwrap();
    for args in [
        vec!["up", "--no-attach"],
        vec!["up", "--dry-run"],
        vec!["add", "dev"],
    ] {
        failure(f.run(&args), "not a registered worktree");
        assert!(!f.root.join("calls").exists());
        f.no_mutations();
    }
}

#[test]
fn configuration_symlinks_and_dangling_output_links_are_rejected() {
    let f = Fixture::new();
    let config = f.repo.join(".herdr/crew.toml");
    let outside = f.other.join("crew.toml");
    fs::rename(&config, &outside).unwrap();
    symlink(&outside, &config).unwrap();
    failure(f.run(&["up", "--no-attach"]), "symlink");
    fs::remove_file(&config).unwrap();
    fs::rename(&outside, &config).unwrap();
    symlink(f.other.join("missing"), f.repo.join(".herdr/status.json")).unwrap();
    failure(f.run(&["up", "--no-attach"]), "symlink");
    assert!(!f.other.join("missing").exists());
    fs::remove_file(f.repo.join(".herdr/status.json")).unwrap();
    fs::rename(f.repo.join(".herdr"), f.other.join(".herdr")).unwrap();
    symlink(f.other.join(".herdr"), f.repo.join(".herdr")).unwrap();
    failure(f.run(&["up", "--no-attach"]), "symlink");
    assert!(!f.root.join("calls").exists());
    assert!(!f.other.join(".herdr/status.schema.json").exists());
}

#[test]
fn unrelated_tabs_do_not_change_verified_workspace_ownership() {
    let f = Fixture::new();
    f.tabs("w1", &["dev", "status", "unrelated"], &f.repo);
    f.response(
        "panes-w1.json",
        json!({"panes": [
            {"pane_id":"p1","tab_id":"w1:t0","cwd":f.repo},
            {"pane_id":"p2","tab_id":"w1:t1","cwd":f.repo},
            {"pane_id":"p3","tab_id":"w1:t2","cwd":f.other}
        ]}),
    );
    let plan = success(f.run(&["up", "--dry-run"]));
    assert!(!plan.contains("create tab"));
    f.no_mutations();
}

#[test]
fn unsafe_extra_prompt_is_rejected_before_creating_a_tab() {
    let f = Fixture::new();
    f.tabs("w1", &["dev", "status"], &f.repo);
    fs::create_dir(f.repo.join(".herdr/prompts")).unwrap();
    let victim = f.other.join("keep.txt");
    fs::write(&victim, "keep").unwrap();
    symlink(&victim, f.repo.join(".herdr/prompts/dev-2.txt")).unwrap();
    failure(f.run(&["add", "dev"]), "symlink");
    assert_eq!(fs::read_to_string(victim).unwrap(), "keep");
    let calls = fs::read_to_string(f.root.join("calls")).unwrap();
    assert!(!calls.contains("tab create"));
    assert!(!calls.contains("agent start"));
}

#[test]
fn nested_repository_does_not_inherit_its_parent_workspace_ownership() {
    let f = Fixture::new();
    let nested = f.repo.join("nested");
    fs::create_dir(&nested).unwrap();
    f.git_at(&nested, &["init", "--quiet"]);
    f.tabs("w1", &["dev", "status"], &nested);
    failure(f.run(&["up", "--no-attach"]), "cannot be verified");
    f.no_mutations();
}

#[test]
fn registered_worktrees_with_separate_main_metadata_remain_supported() {
    let f = Fixture::new();
    let metadata = f.root.join("metadata");
    f.git(&[
        "init",
        "--quiet",
        "--separate-git-dir",
        metadata.to_str().unwrap(),
    ]);
    f.configure(true);
    let path = f.repo.join(".worktrees/dev");
    f.git(&[
        "worktree",
        "add",
        "--quiet",
        "--detach",
        path.to_str().unwrap(),
        "HEAD",
    ]);
    success(f.run(&["up", "--dry-run"]));
    f.no_mutations();
    assert!(f.repo.join(".git").is_file());
    assert!(path.join(".git").is_file());
    f.tabs("w1", &["dev", "status"], &path);
    success(f.run(&["up", "--dry-run"]));
    f.no_mutations();
}

#[test]
fn registered_worktree_cannot_share_main_or_sibling_private_metadata() {
    let f = Fixture::new();
    f.configure(true);
    let path = f.repo.join(".worktrees/dev");
    let sibling = f.repo.join(".worktrees/sibling");
    for path in [&path, &sibling] {
        f.git(&[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            path.to_str().unwrap(),
            "HEAD",
        ]);
    }
    let marker = path.join(".git");
    let original = fs::read_to_string(&marker).unwrap();
    for contents in [
        format!("gitdir: {}\n", f.repo.join(".git").display()),
        fs::read_to_string(sibling.join(".git")).unwrap(),
    ] {
        fs::write(&marker, contents).unwrap();
        failure(f.run(&["up", "--no-attach"]), "not a registered worktree");
        assert!(!f.root.join("calls").exists());
        f.no_mutations();
    }
    fs::remove_file(&marker).unwrap();
    symlink(sibling.join(".git"), &marker).unwrap();
    failure(f.run(&["add", "dev"]), "not a registered worktree");
    assert!(!f.root.join("calls").exists());
    fs::remove_file(&marker).unwrap();
    fs::write(&marker, original).unwrap();
    success(f.run(&["up", "--dry-run"]));
}
