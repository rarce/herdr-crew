//! Setup uses temporary Git repositories and never contacts herdr or launches agents.
#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const CREW: &str = env!("CARGO_BIN_EXE_herdr-crew");
static NEXT: AtomicUsize = AtomicUsize::new(0);

#[test]
fn codex_setup_selects_agent_without_installing_hooks_or_starting_it() {
    let repo = Repo::new(false);
    success(
        repo.command()
            .args(["init", "--agent", "codex", "--yes"])
            .output()
            .unwrap(),
    );
    let text = fs::read_to_string(repo.root.join(".herdr/crew.toml")).unwrap();
    let config: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(config["kind"].as_str(), Some("codex"));
    assert!(!repo.root.join(".codex").exists());
    assert!(!repo.root.join("unexpected-agent-call").exists());
    assert!(
        fs::read_to_string(repo.root.join(".gitignore"))
            .unwrap()
            .contains("/.herdr/codex/")
    );
}

#[test]
fn pi_setup_selects_agent_without_installing_the_extension() {
    let repo = Repo::new(false);
    let output = repo
        .command()
        .args(["init", "--agent", "pi", "--yes"])
        .output()
        .unwrap();
    success(output.clone());
    assert!(String::from_utf8_lossy(&output.stdout).contains("herdr-crew pi-install"));
    let text = fs::read_to_string(repo.root.join(".herdr/crew.toml")).unwrap();
    let config: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(config["kind"].as_str(), Some("pi"));
    assert!(!repo.root.join(".pi").exists());
    assert!(!repo.root.join("unexpected-agent-call").exists());
}

#[test]
fn agy_setup_selects_agent_without_installing_the_hook() {
    let repo = Repo::new(false);
    let output = repo
        .command()
        .args(["init", "--agent", "agy", "--yes"])
        .output()
        .unwrap();
    success(output.clone());
    assert!(String::from_utf8_lossy(&output.stdout).contains("herdr-crew agy-install"));
    let text = fs::read_to_string(repo.root.join(".herdr/crew.toml")).unwrap();
    let config: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(config["kind"].as_str(), Some("agy"));
    assert!(config.get("agy_tree").is_none());
    assert!(!repo.root.join(".gemini").exists());
    assert!(!repo.root.join(".agents").exists());
    assert!(!repo.root.join("unexpected-agent-call").exists());
}

#[test]
fn agy_multi_role_presets_use_tree_mode_without_remote() {
    for (preset, extra_args, expected_manifest) in [
        (
            "review",
            vec![],
            vec![
                "- dev (TypeName: \"self\", Workspace: \"share\")",
                "- reviewer (TypeName: \"research\", Workspace: \"inherit\")",
            ],
        ),
        (
            "review",
            vec!["--shared-checkout"],
            vec![
                "- dev (TypeName: \"self\", Workspace: \"inherit\")",
                "- reviewer (TypeName: \"research\", Workspace: \"inherit\")",
            ],
        ),
        (
            "parallel",
            vec![],
            vec![
                "- dev-a (TypeName: \"self\", Workspace: \"share\")",
                "- dev-b (TypeName: \"self\", Workspace: \"share\")",
                "- reviewer (TypeName: \"research\", Workspace: \"inherit\")",
            ],
        ),
        (
            "research",
            vec![],
            vec![
                "- research-a (TypeName: \"research\", Workspace: \"inherit\")",
                "- research-b (TypeName: \"research\", Workspace: \"inherit\")",
            ],
        ),
    ] {
        let repo = Repo::new(false);
        let mut args = vec![
            "--agent", "agy", "--preset", preset, "--name", "acme", "--yes",
        ];
        args.extend(extra_args);
        success(repo.init(&args));
        let config = repo.config();
        assert_eq!(config["kind"].as_str(), Some("agy"));
        assert_eq!(config["agy_tree"].as_bool(), Some(true));
        assert!(config.get("worktrees").is_none());
        let roles = config["roles"].as_array().unwrap();
        assert_eq!(roles.len(), 1);
        assert_eq!(roles[0]["name"].as_str(), Some("acme-lead"));
        assert_eq!(roles[0]["worktree"].as_bool(), Some(false));
        assert_eq!(roles[0]["extra"].as_bool(), Some(false));
        assert_eq!(config["board"]["writer"].as_str(), Some("acme-lead"));
        let prompt = roles[0]["prompt"].as_str().unwrap();
        for needle in expected_manifest {
            assert!(prompt.contains(needle), "missing {needle} in:\n{prompt}");
        }
        assert!(
            !fs::read_to_string(repo.root.join(".gitignore"))
                .unwrap()
                .contains("/.worktrees/")
        );
        let plan = success(repo.command().args(["up", "--dry-run"]).output().unwrap());
        assert!(plan.contains("acme-lead"), "{plan}");
    }
}

#[test]
fn agy_tree_rejects_topology_flag_and_base_without_writing() {
    let repo = Repo::new(true);
    for options in [
        vec!["--topology", "tree", "--yes"],
        vec![
            "--topology",
            "tabs",
            "--agent",
            "agy",
            "--preset",
            "review",
            "--yes",
        ],
        vec![
            "--agent",
            "agy",
            "--preset",
            "review",
            "--base",
            "upstream/trunk",
            "--yes",
        ],
        vec![
            "--agent",
            "agy",
            "--preset",
            "parallel",
            "--base",
            "upstream/trunk",
            "--yes",
        ],
    ] {
        let output = repo.init(&options);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{:?}: {}",
            options,
            String::from_utf8_lossy(&output.stderr)
        );
        repo.unchanged();
    }

    // `--topology` is rejected as an unknown option on both `init` and `up`.
    let unknown_init = repo.init(&["--topology", "tree"]);
    assert_eq!(unknown_init.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&unknown_init.stderr).contains("unknown option --topology"));
    let unknown_up = repo
        .command()
        .args(["up", "--topology", "tree", "--dry-run"])
        .output()
        .unwrap();
    assert_eq!(unknown_up.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&unknown_up.stderr).contains("unknown option --topology"));
    repo.unchanged();

    // Without --yes, --agent agy --base fails immediately before asking.
    for args in [
        vec![
            "--agent",
            "agy",
            "--preset",
            "review",
            "--base",
            "upstream/trunk",
            "--no-tui",
        ],
        vec![
            "--agent",
            "agy",
            "--preset",
            "parallel",
            "--base",
            "upstream/trunk",
            "--no-tui",
        ],
        vec!["--agent", "agy", "--base", "upstream/trunk", "--no-tui"],
    ] {
        let out = repo.interactive(&args, "");
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(
            !String::from_utf8_lossy(&out.stdout).contains("Project name"),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("native tree mode without [worktrees]"),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        repo.unchanged();
    }

    // Without a remote, --base fails fast before asking or opening a wizard.
    let local = Repo::new(false);
    for args in [
        vec![
            "--agent",
            "agy",
            "--preset",
            "parallel",
            "--base",
            "upstream/trunk",
            "--no-tui",
        ],
        vec!["--base", "upstream/trunk", "--no-tui"],
    ] {
        let out = local.interactive(&args, "");
        assert_eq!(
            out.status.code(),
            Some(2),
            "{:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&out.stdout).contains("Project name"),
            "{:?}: {}",
            args,
            String::from_utf8_lossy(&out.stdout)
        );
        local.unchanged();
    }

    // With a remote and --base (without --agent), interactive review rejects agy and accepts claude.
    let interactive_base = repo.interactive(
        &["--base", "upstream/trunk", "--no-tui"],
        "sample\nreview\nagy\nclaude\n\n\n\ny\n",
    );
    let out = success(interactive_base);
    assert!(
        out.contains("native tree mode without [worktrees]"),
        "{out}"
    );
    let base_config = repo.config();
    assert!(base_config.get("kind").is_none());
    assert!(base_config.get("agy_tree").is_none());
    assert_eq!(
        base_config["worktrees"]["base"].as_str(),
        Some("upstream/trunk")
    );
    assert_eq!(base_config["roles"].as_array().unwrap().len(), 3);
}

#[test]
fn model_profiles_set_each_roles_model_and_effort() {
    let repo = Repo::new(false);
    success(
        repo.command()
            .args([
                "init", "--preset", "research", "--models", "quality", "--yes",
            ])
            .output()
            .unwrap(),
    );
    let text = fs::read_to_string(repo.root.join(".herdr/crew.toml")).unwrap();
    let config: toml::Value = toml::from_str(&text).unwrap();
    let roles = config["roles"].as_array().unwrap();
    let claude = |i: usize, key: &str| roles[i]["claude"][key].as_str().map(str::to_string);
    assert_eq!(claude(0, "model").as_deref(), Some("fable"));
    assert_eq!(claude(1, "model").as_deref(), Some("opus"));
    assert_eq!(claude(1, "effort").as_deref(), Some("high"));
    // `config` edits these in a terminal and refuses to run without one.
    let output = repo.command().arg("config").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("config needs a terminal"));

    let plain = Repo::new(false);
    success(
        plain
            .command()
            .args(["init", "--agent", "pi", "--models", "inherit", "--yes"])
            .output()
            .unwrap(),
    );
    let text = fs::read_to_string(plain.root.join(".herdr/crew.toml")).unwrap();
    assert!(!text.contains("[roles.pi]"), "{text}");
    let bad = plain
        .command()
        .args(["init", "--models", "cheap", "--yes"])
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2));
}

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new(remote: bool) -> Self {
        let root = PathBuf::from("/tmp").join(format!(
            "crew-init-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let repo = Self { root };
        repo.git(&["init", "--quiet", "-b", "trunk"]);
        repo.git(&[
            "-c",
            "user.name=Crew test",
            "-c",
            "user.email=crew@example.invalid",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "Initial",
        ]);
        if remote {
            repo.git(&["remote", "add", "upstream", "."]);
            repo.git(&["update-ref", "refs/remotes/upstream/trunk", "HEAD"]);
            repo.git(&["branch", "--set-upstream-to", "upstream/trunk"]);
        }
        let tools = repo.root.join("tools");
        fs::create_dir(&tools).unwrap();
        for name in ["herdr", "claude"] {
            let path = tools.join(name);
            fs::write(
                &path,
                "#!/bin/sh\ntouch \"$HOME/unexpected-agent-call\"\nexit 1\n",
            )
            .unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        repo
    }

    fn git(&self, args: &[&str]) -> String {
        success(
            Command::new("git")
                .env("HOME", &self.root)
                .arg("-C")
                .arg(&self.root)
                .args(args)
                .output()
                .unwrap(),
        )
    }

    fn command(&self) -> Command {
        let mut command = Command::new(CREW);
        command
            .env_clear()
            .env("HOME", &self.root)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("tools").display()),
            )
            .current_dir(&self.root);
        command
    }

    fn init(&self, args: &[&str]) -> Output {
        self.command().arg("init").args(args).output().unwrap()
    }

    fn interactive(&self, args: &[&str], input: &str) -> Output {
        let mut child = self
            .command()
            .arg("init")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn config(&self) -> toml::Value {
        toml::from_str(&fs::read_to_string(self.root.join(".herdr/crew.toml")).unwrap()).unwrap()
    }

    fn unchanged(&self) {
        assert!(!self.root.join(".herdr").exists());
        assert!(!self.root.join(".gitignore").exists());
        assert!(!self.root.join("unexpected-agent-call").exists());
    }
}

impl Drop for Repo {
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

#[test]
fn presets_create_configuration_without_starting_sessions_or_worktrees() {
    for (preset, count) in [("solo", 1), ("review", 3), ("parallel", 4), ("research", 3)] {
        let repo = Repo::new(true);
        success(repo.init(&["--preset", preset, "--name", "payments", "--yes"]));
        let config = repo.config();
        assert_eq!(config["workspace"]["label"].as_str(), Some("payments"));
        let roles = config["roles"].as_array().unwrap();
        assert_eq!(roles.len(), count);
        assert_eq!(config["board"]["writer"], roles[0]["name"]);
        assert!(
            fs::read_to_string(repo.root.join(".gitignore"))
                .unwrap()
                .contains("/.herdr/status.json")
        );
        if matches!(preset, "review" | "parallel") {
            assert_eq!(config["worktrees"]["base"].as_str(), Some("upstream/trunk"));
            assert!(
                roles[1..]
                    .iter()
                    .all(|role| role["worktree"].as_bool() == Some(true))
            );
        } else {
            assert!(config.get("worktrees").is_none());
        }
        assert!(!repo.root.join(".worktrees").exists());
        assert!(!repo.root.join(".herdr/prompts").exists());
        assert!(!repo.root.join(".herdr/status.json").exists());
        assert!(!repo.root.join("unexpected-agent-call").exists());
        // The generated file can be loaded by a real command; dry-run launches no server.
        let plan = success(repo.command().args(["up", "--dry-run"]).output().unwrap());
        assert!(plan.contains("payments-dev") || plan.contains("payments-lead"));
    }
}

#[test]
fn dry_run_previews_configuration_and_ignore_changes_without_writing() {
    let repo = Repo::new(true);
    let output = success(repo.init(&[
        "--preset",
        "parallel",
        "--name",
        "sample",
        "--base",
        "upstream/trunk",
        "--yes",
        "--dry-run",
    ]));
    assert!(output.contains("sample-dev-a"));
    assert!(output.contains("upstream/trunk"));
    assert!(output.contains("/.worktrees/"));
    assert!(output.contains("Dry run"));
    repo.unchanged();
}

#[test]
fn interactive_setup_retries_invalid_values_and_can_cancel_the_preview() {
    let repo = Repo::new(true);
    let output = success(repo.interactive(
        &[],
        "INVALID\nsample\n9\nreview\n\n\n\ny\nunknown/main\nupstream/trunk\n\nn\n",
    ));
    assert!(output.contains("Use 1-21 characters"));
    assert!(output.contains("Enter 1-4"));
    assert!(output.contains("not configured"));
    assert!(output.contains("sample-reviewer"));
    assert!(output.contains("Cancelled"));
    repo.unchanged();
}

#[test]
fn interactive_review_without_a_remote_defaults_to_shared_read_only_review() {
    let repo = Repo::new(false);
    success(repo.interactive(&[], "sample\nreview\n\n\n\n\n\ny\n"));
    let config = repo.config();
    assert!(config.get("worktrees").is_none());
    for role in config["roles"].as_array().unwrap() {
        assert_eq!(role["worktree"].as_bool(), Some(false));
        assert_eq!(role["extra"].as_bool(), Some(false));
    }
    assert!(
        config["roles"][2]["prompt"]
            .as_str()
            .unwrap()
            .contains("read-only")
    );
    assert!(
        !fs::read_to_string(repo.root.join(".gitignore"))
            .unwrap()
            .contains("/.worktrees/")
    );
}

#[test]
fn existing_configuration_and_dangling_links_are_never_replaced() {
    let repo = Repo::new(false);
    let directory = repo.root.join(".herdr");
    fs::create_dir(&directory).unwrap();
    let path = directory.join("crew.toml");
    fs::write(&path, "user configuration, even when invalid").unwrap();
    let output = repo.init(&["--yes"]);
    assert!(!output.status.success());
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "user configuration, even when invalid"
    );
    fs::remove_file(&path).unwrap();
    symlink(repo.root.join("missing"), &path).unwrap();
    let output = repo.init(&["--yes", "--dry-run"]);
    assert!(!output.status.success());
    assert!(fs::symlink_metadata(path).unwrap().file_type().is_symlink());
    assert!(!repo.root.join(".gitignore").exists());
}

#[test]
fn invalid_workflow_options_and_remote_bases_leave_the_repository_unchanged() {
    let repo = Repo::new(true);
    for options in [
        vec!["--name", "Bad Name", "--yes"],
        vec!["--preset", "solo", "--base", "upstream/trunk", "--yes"],
        vec!["--preset", "research", "--shared-checkout", "--yes"],
        vec!["--preset", "parallel", "--shared-checkout", "--yes"],
        vec![
            "--preset",
            "review",
            "--shared-checkout",
            "--base",
            "upstream/trunk",
            "--yes",
        ],
        vec!["--preset", "parallel", "--base", "unknown/main", "--yes"],
        vec!["--preset", "parallel", "--base", "upstream/--bad", "--yes"],
        vec!["--preset", "parallel", "--base", "upstream/a..b", "--yes"],
    ] {
        let output = repo.init(&options);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{:?}: {}",
            options,
            String::from_utf8_lossy(&output.stderr)
        );
        repo.unchanged();
    }
    let local = Repo::new(false);
    let output = local.init(&["--preset", "review", "--yes"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--shared-checkout"));
    local.unchanged();
}

#[test]
fn ignore_updates_preserve_existing_contents_and_do_not_duplicate_rules() {
    let repo = Repo::new(true);
    let original = "# my rules\r\n/build/\r\n/.herdr/status.json";
    fs::write(repo.root.join(".gitignore"), original).unwrap();
    success(repo.init(&["--preset", "parallel", "--name", "sample", "--yes"]));
    let ignore = fs::read_to_string(repo.root.join(".gitignore")).unwrap();
    assert!(ignore.starts_with(original));
    assert_eq!(
        ignore
            .lines()
            .filter(|line| *line == "/.herdr/status.json")
            .count(),
        1
    );
    assert!(ignore.contains("/.worktrees/"));
    let check = Command::new("git")
        .arg("-C")
        .arg(&repo.root)
        .args(["check-ignore", ".herdr/crew.toml"])
        .output()
        .unwrap();
    assert_eq!(
        check.status.code(),
        Some(1),
        "crew.toml must remain versionable"
    );
}

#[test]
fn ignore_links_are_preserved_and_no_ignore_allows_config_only_setup() {
    let repo = Repo::new(false);
    let target = repo.root.join("custom-ignore");
    fs::write(&target, "custom rules\n").unwrap();
    symlink(&target, repo.root.join(".gitignore")).unwrap();
    assert!(!repo.init(&["--yes"]).status.success());
    assert!(!repo.root.join(".herdr").exists());
    success(repo.init(&["--yes", "--no-ignore"]));
    assert_eq!(fs::read_to_string(&target).unwrap(), "custom rules\n");
    assert!(repo.root.join(".herdr/crew.toml").is_file());
}

#[test]
fn end_of_input_cancels_setup_before_any_files_are_created() {
    let repo = Repo::new(false);
    let output = repo.interactive(&[], "sample\nsolo\n\n\n");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("input ended"));
    repo.unchanged();
}

#[test]
fn setup_from_a_linked_worktree_writes_only_to_the_main_checkout() {
    let repo = Repo::new(false);
    let linked = repo.root.join("linked");
    repo.git(&[
        "worktree",
        "add",
        "--quiet",
        "--detach",
        linked.to_str().unwrap(),
        "HEAD",
    ]);
    let subdirectory = linked.join("subdirectory");
    fs::create_dir(&subdirectory).unwrap();
    success(
        repo.command()
            .current_dir(&subdirectory)
            .args(["init", "--name", "sample", "--yes"])
            .output()
            .unwrap(),
    );
    assert!(repo.root.join(".herdr/crew.toml").is_file());
    assert!(!linked.join(".herdr").exists());
    assert!(!linked.join(".gitignore").exists());
    assert_eq!(repo.config()["workspace"]["label"].as_str(), Some("sample"));
    assert_eq!(repo.git(&["symbolic-ref", "--short", "HEAD"]), "trunk\n");
    assert!(Path::new(CREW).is_absolute());
}

#[test]
fn separate_git_directory_keeps_configuration_in_the_working_checkout() {
    let repo = Repo::new(false);
    let checkout = repo.root.join("checkout \n");
    let metadata_parent = repo.root.join("metadata");
    let metadata = metadata_parent.join("store.git");
    fs::create_dir(&metadata_parent).unwrap();
    repo.git(&[
        "init",
        "--quiet",
        "--separate-git-dir",
        metadata.to_str().unwrap(),
        checkout.to_str().unwrap(),
    ]);
    let subdirectory = checkout.join("subdirectory");
    fs::create_dir(&subdirectory).unwrap();
    success(
        repo.command()
            .current_dir(subdirectory)
            .args(["init", "--name", "sample", "--yes"])
            .output()
            .unwrap(),
    );
    assert!(checkout.join(".herdr/crew.toml").is_file());
    assert!(checkout.join(".gitignore").is_file());
    assert!(!metadata_parent.join(".herdr").exists());
    assert!(!metadata_parent.join(".gitignore").exists());
    repo.unchanged();
}

#[test]
fn submodule_setup_keeps_configuration_out_of_superproject_metadata() {
    let repo = Repo::new(false);
    let source = Repo::new(false);
    repo.git(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "add",
        "--quiet",
        source.root.to_str().unwrap(),
        "module",
    ]);
    let module = repo.root.join("module");
    success(
        repo.command()
            .args(["--root", module.to_str().unwrap(), "init", "--yes"])
            .output()
            .unwrap(),
    );
    assert!(module.join(".herdr/crew.toml").is_file());
    assert!(module.join(".gitignore").is_file());
    assert!(!repo.root.join(".git/modules/.herdr").exists());
    assert!(!repo.root.join(".git/modules/.gitignore").exists());
    repo.unchanged();
    source.unchanged();
}

#[test]
fn linked_worktree_with_unresolvable_main_checkout_is_rejected_without_writes() {
    let repo = Repo::new(false);
    let checkout = repo.root.join("checkout");
    let metadata_parent = repo.root.join("metadata");
    let metadata = metadata_parent.join("store.git");
    fs::create_dir(&metadata_parent).unwrap();
    repo.git(&[
        "init",
        "--quiet",
        "--separate-git-dir",
        metadata.to_str().unwrap(),
        checkout.to_str().unwrap(),
    ]);
    repo.git(&[
        "-C",
        checkout.to_str().unwrap(),
        "-c",
        "user.name=Crew test",
        "-c",
        "user.email=crew@example.invalid",
        "commit",
        "--quiet",
        "--allow-empty",
        "-m",
        "Initial",
    ]);
    let linked = repo.root.join("linked");
    repo.git(&[
        "-C",
        checkout.to_str().unwrap(),
        "worktree",
        "add",
        "--quiet",
        "--detach",
        linked.to_str().unwrap(),
        "HEAD",
    ]);
    let output = repo
        .command()
        .current_dir(&linked)
        .args(["init", "--yes"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("main checkout"));
    for directory in [&checkout, &linked, &metadata_parent, &metadata] {
        assert!(!directory.join(".herdr").exists());
        assert!(!directory.join(".gitignore").exists());
    }
    repo.unchanged();
}

#[test]
fn bare_repositories_are_rejected_before_initializing_their_parent_directory() {
    let repo = Repo::new(false);
    let bare = repo.root.join("bare.git");
    repo.git(&["init", "--quiet", "--bare", bare.to_str().unwrap()]);
    let output = repo
        .command()
        .current_dir(&bare)
        .args(["init", "--yes"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("bare repository"));
    repo.unchanged();
}
