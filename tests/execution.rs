//! CLI flows against real Git repositories and a stateful offline herdr simulator.
//! No server, terminal process, Claude installation or network access is needed.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

const CREW: &str = env!("CARGO_BIN_EXE_herdr-crew");
const MESSAGE: &str = "Confirma tu rol «ahora»; it's fine, $HOME `id`.";
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    herdr: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        Self::with_repo_name("repo")
    }

    fn with_repo_name(name: &str) -> Self {
        let root = PathBuf::from("/tmp").join(format!(
            "crew-execution-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join(name)).unwrap();
        let f = Self {
            repo: root.join(name).canonicalize().unwrap(),
            state: root.join("state"),
            herdr: root.join("herdr"),
            root,
        };
        fs::create_dir(&f.state).unwrap();
        fs::write(f.state.join("online"), "").unwrap();
        fs::write(&f.herdr, include_str!("support/herdr.sh")).unwrap();
        fs::set_permissions(&f.herdr, fs::Permissions::from_mode(0o755)).unwrap();
        f.git(&["init", "--quiet", "-b", "main"]);
        fs::create_dir(f.repo.join(".herdr")).unwrap();
        fs::write(
            f.repo.join(".gitignore"),
            "/.worktrees/\n/.herdr/prompts/\n/.herdr/status.json\n/.herdr/status.schema.json\n",
        )
        .unwrap();
        fs::write(
            f.repo.join(".herdr/crew.toml"),
            r#"version = 1
start_message = "Confirma tu rol «ahora»; it's fine, $HOME `id`."
common_prompt = "Board {{STATUS}}; schema {{SCHEMA}}."
[workspace]
label = "crew"
[board]
tab = "status"
writer = "lead"
[worktrees]
dir = ".worktrees"
base = "origin/main"
[[roles]]
name = "lead"
prompt = "Lead {{NAME}} in {{REPO}}; launcher {{LAUNCHER}}."
[[roles]]
name = "dev"
prompt = "Developer {{NAME}} in {{REPO}}."
worktree = true
extra = true
"#,
        )
        .unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "--quiet", "-m", "Initial"]);
        let origin = f.root.join("origin.git");
        success(
            f.command("git")
                .args(["init", "--quiet", "--bare", "-b", "main"])
                .arg(&origin)
                .output()
                .unwrap(),
        );
        f.git(&["remote", "add", "origin", origin.to_str().unwrap()]);
        f.git(&["push", "--quiet", "origin", "main"]);
        f
    }

    fn command(&self, program: impl AsRef<Path>) -> Command {
        let mut c = Command::new(program.as_ref());
        c.env_clear()
            .env("HOME", &self.root)
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Crew test")
            .env("GIT_AUTHOR_EMAIL", "crew@example.invalid")
            .env("GIT_COMMITTER_NAME", "Crew test")
            .env("GIT_COMMITTER_EMAIL", "crew@example.invalid")
            .env("HERDR_BIN_PATH", &self.herdr)
            .env("CREW_TEST_STATE", &self.state)
            .current_dir(&self.repo);
        c
    }

    fn crew(&self, args: &[&str]) -> Output {
        self.command(CREW).args(args).output().unwrap()
    }

    fn up(&self) -> String {
        success(self.crew(&["up", "--no-attach"]))
    }

    fn git(&self, args: &[&str]) -> String {
        success(self.command("git").args(args).output().unwrap())
    }

    fn result(&self, args: &[&str]) -> Value {
        let out = success(self.command(&self.herdr).args(args).output().unwrap());
        serde_json::from_str::<Value>(&out).unwrap()["result"].clone()
    }

    fn workspace(&self) -> String {
        let w = self.result(&["workspace", "list"]);
        let workspaces = w["workspaces"].as_array().unwrap();
        assert_eq!(workspaces.len(), 1, "{w}");
        workspaces[0]["workspace_id"].as_str().unwrap().into()
    }

    fn tab(&self, label: &str) -> PathBuf {
        let w = self.workspace();
        let t = self.result(&["tab", "list", "--workspace", &w]);
        let tab = t["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["label"] == label)
            .unwrap_or_else(|| panic!("missing {label}: {t}"));
        self.state
            .join("workspaces")
            .join(w)
            .join("tabs")
            .join(tab["tab_id"].as_str().unwrap())
    }

    fn labels(&self) -> Vec<String> {
        let t = self.result(&["tab", "list", "--workspace", &self.workspace()]);
        let mut labels: Vec<_> = t["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["label"].as_str().unwrap().to_string())
            .collect();
        labels.sort();
        labels
    }

    fn agents(&self) -> Vec<String> {
        let a = self.result(&["agent", "list"]);
        let mut names: Vec<_> = a["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["name"].as_str().unwrap().to_string())
            .collect();
        names.sort();
        names
    }

    fn calls(&self) -> Vec<Vec<String>> {
        let directory = self.state.join("calls");
        if !directory.exists() {
            return Vec::new();
        }
        let mut entries: Vec<_> = fs::read_dir(directory)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort_by_key(|p| {
            p.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .parse::<usize>()
                .unwrap()
        });
        entries
            .into_iter()
            .map(|p| {
                fs::read(p.join("args"))
                    .unwrap()
                    .split(|b| *b == 0)
                    .filter(|part| !part.is_empty())
                    .map(|part| String::from_utf8(part.to_vec()).unwrap())
                    .collect()
            })
            .collect()
    }

    fn count(&self, prefix: &[&str]) -> usize {
        self.calls()
            .iter()
            .filter(|c| starts_with(c, prefix))
            .count()
    }

    fn fail_once(&self, prefix: &str, mode: &str) {
        fs::write(self.state.join("fail-prefix"), prefix).unwrap();
        fs::write(self.state.join("fail-mode"), mode).unwrap();
    }

    fn close_tab(&self, label: &str) {
        let tab = self.tab(label);
        self.result(&["tab", "close", tab.file_name().unwrap().to_str().unwrap()]);
    }

    fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.repo.join(relative)).unwrap()
    }

    fn no_generated_files(&self) {
        for path in [
            ".herdr/prompts",
            ".herdr/status.schema.json",
            ".herdr/status.json",
        ] {
            assert!(!self.repo.join(path).exists(), "unexpected {path}");
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn starts_with(call: &[String], prefix: &[&str]) -> bool {
    call.iter()
        .map(String::as_str)
        .take(prefix.len())
        .eq(prefix.iter().copied())
}

fn success(out: Output) -> String {
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn failure(out: Output, text: &str) {
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(text),
        "{out:?}"
    );
}

#[test]
fn up_creates_complete_crew_and_repeated_up_preserves_state_and_shell_arguments() {
    let f = Fixture::with_repo_name("repo 'quoted' $literal `literal`");
    f.up();
    let mutations: Vec<_> = f
        .calls()
        .into_iter()
        .filter(|c| !["list", "process-info"].contains(&c.get(1).map(String::as_str).unwrap_or("")))
        .map(|c| c[..2].join(" "))
        .collect();
    assert_eq!(
        mutations,
        [
            "workspace create",
            "tab rename",
            "tab create",
            "agent start",
            "agent start",
            "tab create",
            "pane run",
            "workspace focus"
        ]
    );
    assert_eq!(f.labels(), ["dev", "lead", "status"]);
    assert_eq!(f.agents(), ["dev", "lead"]);
    assert_eq!(f.count(&["workspace", "create"]), 1);
    assert_eq!(f.count(&["tab", "create"]), 2);
    let lead = f.read(".herdr/prompts/lead.txt");
    assert!(lead.contains(&format!("Lead lead in {}", f.repo.display())));
    assert!(lead.contains(CREW));
    assert!(lead.contains("Board .herdr/status.json; schema .herdr/status.schema.json."));
    assert!(f.read(".herdr/prompts/dev.txt").contains("Developer dev"));
    let cwd: String =
        serde_json::from_str(&fs::read_to_string(f.tab("dev").join("cwd")).unwrap()).unwrap();
    assert_eq!(Path::new(&cwd), f.repo.join(".worktrees/dev"));
    assert!(!lead.contains("{{"));
    for name in ["lead", "dev"] {
        let calls = f.calls();
        let call = calls
            .iter()
            .find(|c| starts_with(c, &["agent", "start", name]))
            .unwrap();
        assert_eq!(call.last().unwrap(), MESSAGE);
        assert_eq!(&call[3..6], ["--kind", "claude", "--pane"]);
        assert_eq!(
            &call[7..13],
            [
                "--timeout",
                "60000",
                "--",
                "-n",
                name,
                "--append-system-prompt-file"
            ]
        );
        assert_eq!(
            call[13],
            f.repo
                .join(format!(".herdr/prompts/{name}.txt"))
                .to_str()
                .unwrap()
        );
    }
    let schema = f.read(".herdr/status.schema.json");
    let status = f.read(".herdr/status.json");
    let mut changed: Value = serde_json::from_str(&status).unwrap();
    assert_eq!(changed["sessions"], json!([]));
    changed["sessions"] = json!([
        {"role": "lead", "state": "working", "now": "User progress to preserve", "next": []},
        {"role": "dev", "state": "waiting", "now": "Waiting for review", "next": []}
    ]);
    let status = serde_json::to_string(&changed).unwrap();
    fs::write(f.repo.join(".herdr/status.json"), &status).unwrap();
    let board_command = fs::read_to_string(f.tab("status").join("command")).unwrap();
    let board = success(
        f.command("/bin/sh")
            .args(["-c", &format!("{board_command} --once")])
            .output()
            .unwrap(),
    );
    assert!(board.contains("lead") && board.contains("dev"), "{board}");
    let before = f.calls().len();
    assert!(f.up().contains("nothing to do"));
    assert_eq!(f.read(".herdr/status.json"), status);
    assert_eq!(f.read(".herdr/status.schema.json"), schema);
    assert_eq!(f.read(".herdr/prompts/lead.txt"), lead);
    for call in &f.calls()[before..] {
        assert!(
            !starts_with(call, &["agent", "start"])
                && !starts_with(call, &["tab", "create"])
                && !starts_with(call, &["pane", "run"]),
            "unexpected repeat: {call:?}"
        );
    }
    assert_eq!(f.git(&["status", "--porcelain"]), "");
    assert_eq!(f.git(&["branch", "--show-current"]), "main\n");
    assert_eq!(
        f.git(&["-C", ".worktrees/dev", "branch", "--show-current"]),
        ""
    );
}

#[test]
fn extra_instance_close_and_add_reuse_worktree_without_fetch_or_losing_changes() {
    let f = Fixture::new();
    f.up();
    success(f.crew(&["add", "dev"]));
    assert_eq!(f.labels(), ["dev", "dev-2", "lead", "status"]);
    assert_eq!(f.agents(), ["dev", "dev-2", "lead"]);
    assert!(
        f.read(".herdr/prompts/dev-2.txt")
            .contains("Developer dev-2")
    );
    let extra = f.repo.join(".worktrees/dev-2");
    fs::write(extra.join("local-change"), "keep this").unwrap();
    let before = f.count(&["tab", "close"]);
    failure(f.crew(&["close", "dev"]), "only extra instances");
    assert_eq!(f.count(&["tab", "close"]), before);
    success(f.crew(&["close", "dev-2"]));
    assert!(extra.join(".git").is_file());
    assert_eq!(f.agents(), ["dev", "lead"]);
    fs::rename(f.root.join("origin.git"), f.root.join("unavailable.git")).unwrap();
    let out = f.crew(&["add", "dev"]);
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("fetch"),
        "{out:?}"
    );
    success(out);
    assert_eq!(
        fs::read_to_string(extra.join("local-change")).unwrap(),
        "keep this"
    );
    assert_eq!(f.count(&["agent", "start", "dev-2"]), 2);
    assert_eq!(f.labels(), ["dev", "dev-2", "lead", "status"]);
}

#[test]
fn dry_run_without_server_does_not_start_server_or_write_outputs() {
    let f = Fixture::new();
    fs::remove_file(f.state.join("online")).unwrap();
    let out = success(f.crew(&["up", "--dry-run"]));
    assert!(
        out.contains("no herdr server") && out.contains("create workspace"),
        "{out}"
    );
    assert_eq!(
        f.calls(),
        [vec!["workspace".to_string(), "list".to_string()]]
    );
    f.no_generated_files();
    assert!(!f.repo.join(".worktrees").exists());
}

#[test]
fn cold_up_starts_server_without_claude_nesting_variables() {
    let f = Fixture::new();
    fs::remove_file(f.state.join("online")).unwrap();
    let out = success(
        f.command(CREW)
            .args(["up", "--no-attach"])
            .env("CLAUDECODE", "1")
            .env("CLAUDE_CODE_CHILD_SESSION", "child")
            .env("CLAUDE_CODE_ENTRYPOINT", "entry")
            .output()
            .unwrap(),
    );
    assert!(out.contains("no herdr server; starting one"), "{out}");
    assert_eq!(
        fs::read_to_string(f.state.join("server-env")).unwrap(),
        "unset|unset|unset"
    );
    assert_eq!(f.count(&["server"]), 1);
    assert_eq!(
        fs::read_to_string(f.state.join("server-cwd")).unwrap(),
        f.repo.to_str().unwrap()
    );
    assert_eq!(f.agents(), ["dev", "lead"]);
}

#[test]
fn failed_tab_creation_stops_before_agents_and_retry_requires_explicit_empty_tab_recovery() {
    let f = Fixture::new();
    f.fail_once("tab create", "tab_failed");
    failure(f.crew(&["up", "--no-attach"]), "could not create tab");
    assert_eq!(f.labels(), ["lead"]);
    assert!(f.repo.join(".worktrees/dev/.git").is_file());
    f.no_generated_files();
    assert_eq!(f.count(&["agent", "start"]), 0);
    let retry = f.crew(&["up", "--no-attach"]);
    assert!(
        String::from_utf8_lossy(&retry.stderr).contains("tab lead has no agent"),
        "{retry:?}"
    );
    success(retry);
    assert_eq!(f.labels(), ["dev", "lead", "status"]);
    assert_eq!(f.agents(), ["dev"]);
    assert_eq!(f.count(&["workspace", "create"]), 1);
    f.close_tab("lead");
    f.up();
    assert_eq!(f.agents(), ["dev", "lead"]);
    assert_eq!(f.count(&["agent", "start", "dev"]), 1);
}

#[test]
fn failed_agent_start_preserves_live_roles_and_retry_does_not_restart_empty_tab() {
    let f = Fixture::new();
    f.fail_once("agent start dev", "start_failed");
    failure(f.crew(&["up", "--no-attach"]), "could not start dev");
    assert_eq!(f.agents(), ["lead"]);
    assert_eq!(f.labels(), ["dev", "lead"]);
    assert!(f.repo.join(".herdr/prompts/dev.txt").is_file());
    assert!(!f.repo.join(".herdr/status.json").exists());
    let retry = f.crew(&["up", "--no-attach"]);
    assert!(
        String::from_utf8_lossy(&retry.stderr).contains("tab dev has no agent"),
        "{retry:?}"
    );
    success(retry);
    assert_eq!(f.count(&["agent", "start", "lead"]), 1);
    assert_eq!(f.count(&["agent", "start", "dev"]), 1);
    f.close_tab("dev");
    fs::write(f.repo.join(".worktrees/dev/local-change"), "keep").unwrap();
    f.up();
    assert_eq!(f.agents(), ["dev", "lead"]);
    assert_eq!(f.count(&["agent", "start", "dev"]), 2);
    assert_eq!(f.read(".worktrees/dev/local-change"), "keep");
}

#[test]
fn failed_board_command_is_repaired_without_recreating_tabs_or_overwriting_status() {
    let f = Fixture::new();
    f.fail_once("pane run", "run_failed");
    failure(f.crew(&["up", "--no-attach"]), "could not type");
    assert_eq!(f.agents(), ["dev", "lead"]);
    assert_eq!(f.labels(), ["dev", "lead", "status"]);
    let status = f.read(".herdr/status.json");
    let tabs = f.count(&["tab", "create"]);
    f.up();
    assert_eq!(f.count(&["pane", "run"]), 2);
    assert_eq!(f.count(&["tab", "create"]), tabs);
    assert_eq!(f.count(&["agent", "start"]), 2);
    assert_eq!(f.read(".herdr/status.json"), status);
    f.up();
    assert_eq!(f.count(&["pane", "run"]), 2);
}

#[test]
fn busy_agent_retries_and_not_ready_warns_without_duplicate_start() {
    for (mode, starts) in [("agent_pane_busy", 2), ("agent_not_ready", 1)] {
        let f = Fixture::new();
        f.fail_once("agent start lead", mode);
        let out = f.crew(&["up", "--no-attach"]);
        if mode == "agent_not_ready" {
            assert!(
                String::from_utf8_lossy(&out.stderr).contains("waiting for an answer"),
                "{out:?}"
            );
        }
        success(out);
        assert_eq!(f.agents(), ["dev", "lead"]);
        assert_eq!(f.count(&["agent", "start", "lead"]), starts);
        f.up();
        assert_eq!(f.count(&["agent", "start", "lead"]), starts);
    }
}

#[test]
fn query_transport_and_malformed_responses_stop_before_project_mutations() {
    for (prefix, mode, message) in [
        ("agent list", "transport", "simulated transport failure"),
        ("agent list", "malformed", "response without \"agents\""),
        (
            "workspace create",
            "malformed",
            "response without workspace.workspace_id",
        ),
    ] {
        let f = Fixture::new();
        f.fail_once(prefix, mode);
        failure(f.crew(&["up", "--no-attach"]), message);
        assert_eq!(f.count(&["agent", "start"]), 0);
        assert_eq!(f.count(&["tab", "create"]), 0);
        f.no_generated_files();
        f.up();
        assert_eq!(f.agents(), ["dev", "lead"]);
    }
}

#[test]
fn startup_adopts_initial_workspace_then_repairs_only_board_on_restore() {
    let f = Fixture::new();
    let w = f.result(&[
        "workspace",
        "create",
        "--cwd",
        f.repo.to_str().unwrap(),
        "--label",
        "repo",
        "--no-focus",
    ]);
    let id = w["workspace"]["workspace_id"].as_str().unwrap();
    let startup = || {
        f.command(CREW)
            .arg("startup")
            .env("HERDR_PLUGIN_ID", "herdr-crew")
            .env(
                "HERDR_PLUGIN_CONTEXT_JSON",
                json!({"workspace_id": id}).to_string(),
            )
            .output()
            .unwrap()
    };
    success(startup());
    assert_eq!(f.labels(), ["dev", "lead", "status"]);
    assert_eq!(f.agents(), ["dev", "lead"]);
    assert_eq!(f.count(&["workspace", "create"]), 1);
    assert_eq!(f.count(&["workspace", "rename"]), 1);
    assert_eq!(f.count(&["workspace", "focus"]), 1);
    let dev = f.tab("dev");
    fs::remove_file(dev.join("agent")).unwrap();
    fs::remove_file(dev.join("running")).unwrap();
    let status_tab = f.tab("status");
    fs::remove_file(status_tab.join("running")).unwrap();
    let status = f.read(".herdr/status.json");
    let repaired = startup();
    assert!(
        String::from_utf8_lossy(&repaired.stderr).contains("claude -r session-"),
        "{repaired:?}"
    );
    success(repaired);
    assert_eq!(f.count(&["agent", "start"]), 2);
    assert_eq!(f.count(&["pane", "run"]), 2);
    assert_eq!(f.count(&["workspace", "focus"]), 1);
    assert_eq!(f.count(&["notification", "show"]), 0);
    assert_eq!(f.read(".herdr/status.json"), status);
    f.close_tab("dev");
    f.close_tab("status");
    success(startup());
    assert_eq!(f.labels(), ["lead"]);
    assert_eq!(f.count(&["tab", "create"]), 2);
}

#[test]
fn action_with_unavailable_server_notifies_without_starting_it() {
    let f = Fixture::new();
    fs::remove_file(f.state.join("online")).unwrap();
    let out = f
        .command(CREW)
        .args(["up", "--no-attach"])
        .env("HERDR_PLUGIN_ID", "herdr-crew")
        .env(
            "HERDR_PLUGIN_CONTEXT_JSON",
            json!({"workspace_cwd": f.repo}).to_string(),
        )
        .output()
        .unwrap();
    failure(out, "the herdr server does not answer");
    assert_eq!(f.count(&["server"]), 0);
    assert_eq!(f.count(&["notification", "show"]), 1);
    f.no_generated_files();
}

#[test]
fn fetch_failure_uses_cached_ref_but_missing_ref_stops_before_agents_and_can_recover() {
    for cached in [true, false] {
        let f = Fixture::new();
        if !cached {
            f.git(&["update-ref", "-d", "refs/remotes/origin/main"]);
        }
        fs::rename(f.root.join("origin.git"), f.root.join("unavailable.git")).unwrap();
        let out = f.crew(&["up", "--no-attach"]);
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("using the local ref"),
            "{out:?}"
        );
        if cached {
            success(out);
            assert_eq!(f.agents(), ["dev", "lead"]);
        } else {
            failure(out, "git worktree add");
            assert_eq!(f.count(&["agent", "start"]), 0);
            assert_eq!(f.labels(), ["lead"]);
            f.no_generated_files();
            fs::rename(f.root.join("unavailable.git"), f.root.join("origin.git")).unwrap();
            f.up();
            assert_eq!(f.agents(), ["dev"]);
            f.close_tab("lead");
            f.up();
            assert_eq!(f.agents(), ["dev", "lead"]);
            assert_eq!(f.count(&["workspace", "create"]), 1);
        }
    }
}

#[test]
fn alternate_agent_command_is_typed_whole_in_role_panes_and_never_uses_agent_start() {
    let f = Fixture::new();
    let command = "printf '%s' 'literal $HOME `id`'";
    success(
        f.command(CREW)
            .args(["up", "--no-attach"])
            .env("CREW_AGENT_CMD", command)
            .output()
            .unwrap(),
    );
    assert_eq!(f.count(&["agent", "start"]), 0);
    assert_eq!(f.count(&["pane", "run"]), 3);
    for name in ["lead", "dev"] {
        assert_eq!(
            fs::read_to_string(f.tab(name).join("command")).unwrap(),
            command
        );
        assert!(f.repo.join(format!(".herdr/prompts/{name}.txt")).is_file());
    }
    f.up();
    assert_eq!(f.count(&["pane", "run"]), 3);
    assert_eq!(f.count(&["agent", "start"]), 0);
}

#[test]
fn stale_schema_is_replaced_but_busy_or_unknown_board_foreground_is_left_alone() {
    let f = Fixture::new();
    f.up();
    let schema = f.read(".herdr/status.schema.json");
    let status = f.read(".herdr/status.json");
    fs::write(f.repo.join(".herdr/status.schema.json"), "obsolete schema").unwrap();
    f.up();
    assert_eq!(f.read(".herdr/status.schema.json"), schema);
    assert_eq!(f.count(&["pane", "run"]), 1);
    let board = f.tab("status");
    fs::remove_file(board.join("running")).unwrap();
    f.fail_once("pane process-info", "transport");
    f.up();
    assert_eq!(f.count(&["pane", "run"]), 1);
    assert!(!board.join("running").exists());
    f.up();
    assert!(board.join("running").exists());
    assert_eq!(f.count(&["pane", "run"]), 2);
    assert_eq!(f.count(&["agent", "start"]), 2);
    assert_eq!(f.read(".herdr/status.json"), status);
}

#[test]
fn failed_extra_agent_in_action_notifies_and_explicit_close_allows_recovery() {
    let f = Fixture::new();
    f.up();
    let status = f.read(".herdr/status.json");
    f.fail_once("agent start dev-2", "start_failed");
    let out = f
        .command(CREW)
        .args(["add", "dev"])
        .env("HERDR_PLUGIN_ID", "herdr-crew")
        .env(
            "HERDR_PLUGIN_CONTEXT_JSON",
            json!({"workspace_cwd": f.repo}).to_string(),
        )
        .output()
        .unwrap();
    failure(out, "could not start dev-2");
    assert_eq!(f.count(&["notification", "show"]), 1);
    assert_eq!(f.agents(), ["dev", "lead"]);
    assert_eq!(f.labels(), ["dev", "dev-2", "lead", "status"]);
    assert_eq!(f.read(".herdr/status.json"), status);
    success(f.crew(&["close", "dev-2"]));
    success(f.crew(&["add", "dev"]));
    assert_eq!(f.agents(), ["dev", "dev-2", "lead"]);
    assert_eq!(f.count(&["agent", "start", "lead"]), 1);
    assert_eq!(f.count(&["agent", "start", "dev"]), 1);
    assert_eq!(f.count(&["agent", "start", "dev-2"]), 2);
    assert_eq!(f.read(".herdr/status.json"), status);
}
