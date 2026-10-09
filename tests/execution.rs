//! CLI flows against real Git repositories and a stateful offline herdr simulator.
//! No server, terminal process, Claude installation or network access is needed.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

#[path = "../src/process.rs"]
mod inspect;

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
    fn codex_tools(&self) {
        let tools = self.root.join("tools");
        fs::create_dir(&tools).unwrap();
        fs::write(tools.join("codex"), include_str!("support/codex.py")).unwrap();
        fs::write(tools.join("herdr-crew"), "#!/bin/sh\nif [ \"${1-}\" = --launcher-id ]; then echo 'herdr-crew public launcher format 1 (rarce/herdr-crew)'; else exec \"$CREW_TEST_BIN\" \"$@\"; fi\n").unwrap();
        for name in ["codex", "herdr-crew"] {
            fs::set_permissions(tools.join(name), fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn codex_config(&self, shared: bool) {
        let mut config = self
            .read(".herdr/crew.toml")
            .replace("version = 1", "version = 1\nkind = \"codex\"");
        config = config.replace("[workspace]", "[codex]\nmodel = \"original-model\"\nsandbox = \"workspace-write\"\napproval_policy = \"on-request\"\n[workspace]");
        if shared {
            config = config.replace("worktree = true", "worktree = false");
        }
        fs::write(self.repo.join(".herdr/crew.toml"), config).unwrap();
    }

    /// A fake `claude` in the test PATH that answers `--version` with `mode` permissions.
    fn claude_tool(&self, mode: u32) {
        let tools = self.root.join("tools");
        fs::create_dir_all(&tools).unwrap();
        fs::write(
            tools.join("claude"),
            "#!/bin/sh\necho '2.1.0 (Claude Code)'\n",
        )
        .unwrap();
        fs::set_permissions(tools.join("claude"), fs::Permissions::from_mode(mode)).unwrap();
    }

    fn snapshots(&self) -> Vec<Value> {
        fs::read_dir(self.repo.join(".herdr/codex/snapshots"))
            .unwrap()
            .map(|entry| serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap())
            .collect()
    }
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
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("tools").display()),
            )
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Crew test")
            .env("GIT_AUTHOR_EMAIL", "crew@example.invalid")
            .env("GIT_COMMITTER_NAME", "Crew test")
            .env("GIT_COMMITTER_EMAIL", "crew@example.invalid")
            .env("HERDR_BIN_PATH", &self.herdr)
            .env("CODEX_HOME", self.root.join(".codex"))
            .env("CREW_TEST_BIN", CREW)
            .env("CREW_TEST_STATE", &self.state)
            .env("CREW_TEST_SOCKET", self.root.join("herdr.sock"))
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

#[test]
fn mixed_agents_and_extras_use_their_effective_kind_and_settings() {
    let f = Fixture::new();
    f.codex_tools();
    let config = f
        .read(".herdr/crew.toml")
        .replace("name = \"dev\"", "name = \"dev\"\nkind = \"codex\"");
    fs::write(
        f.repo.join(".herdr/crew.toml"),
        format!(
            "{config}\n[roles.codex]\nmodel = \"account-model\"\nreasoning_effort = \"high\"\n"
        ),
    )
    .unwrap();
    f.up();
    success(f.crew(&["add", "dev"]));
    let starts: Vec<_> = f
        .calls()
        .into_iter()
        .filter(|c| starts_with(c, &["agent", "start"]))
        .collect();
    assert_eq!(starts.len(), 3);
    assert_eq!(&starts[0][3..5], ["--kind", "claude"]);
    for call in &starts[1..] {
        assert_eq!(&call[3..5], ["--kind", "codex"]);
        assert!(call.contains(&"--no-daemon".into()));
        assert!(call.contains(&"model=\"account-model\"".into()));
        assert!(call.contains(&"model_reasoning_effort=\"high\"".into()));
        assert_eq!(call.last().unwrap(), MESSAGE);
        assert!(!call.contains(&"--append-system-prompt-file".into()));
    }
    assert_eq!(f.snapshots().len(), 2);
    f.up();
    assert_eq!(f.count(&["agent", "start"]), 3);
}

#[test]
fn untrusted_codex_hooks_stop_before_project_mutations_but_allow_board_repair() {
    let f = Fixture::new();
    f.codex_tools();
    f.codex_config(true);
    let failed = f
        .command(CREW)
        .env("CREW_TEST_TRUST", "untrusted")
        .args(["up", "--no-attach"])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("/hooks"));
    f.no_generated_files();
    assert!(!f.repo.join(".herdr/codex").exists());
    assert_eq!(f.count(&["workspace", "create"]), 0);
    f.up();
    let board = f.tab("status");
    fs::remove_file(board.join("running")).unwrap();
    let repaired = f
        .command(CREW)
        .env("CREW_TEST_TRUST", "modified")
        .args(["up", "--no-attach"])
        .output()
        .unwrap();
    assert!(
        repaired.status.success(),
        "{}",
        String::from_utf8_lossy(&repaired.stderr)
    );
    assert_eq!(f.count(&["agent", "start"]), 2);
}

#[test]
fn oversized_codex_context_and_runtime_symlinks_fail_before_mutations() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    f.codex_tools();
    f.codex_config(true);
    let config = f.read(".herdr/crew.toml");
    fs::write(
        f.repo.join(".herdr/crew.toml"),
        config.replace("Lead {{NAME}}", &"x".repeat(65_536)),
    )
    .unwrap();
    let failed = f.crew(&["up", "--no-attach"]);
    assert!(!failed.status.success());
    assert_eq!(f.count(&["workspace", "create"]), 0);
    f.no_generated_files();
    fs::write(f.repo.join(".herdr/crew.toml"), config).unwrap();
    symlink(&f.root, f.repo.join(".herdr/codex")).unwrap();
    let failed = f.crew(&["up", "--no-attach"]);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("symlink"));
    assert_eq!(f.count(&["workspace", "create"]), 0);
    fs::remove_file(f.repo.join(".herdr/codex")).unwrap();
    fs::create_dir(f.repo.join(".herdr/codex")).unwrap();
    symlink(&f.root, f.repo.join(".herdr/codex/snapshots")).unwrap();
    failure(f.crew(&["up", "--no-attach"]), "symlink");
    assert_eq!(f.count(&["workspace", "create"]), 0);
}

#[test]
fn explicit_hook_installation_preserves_user_configuration_and_credentials() {
    let f = Fixture::new();
    f.codex_tools();
    let home = f.root.join(".codex");
    fs::create_dir(&home).unwrap();
    let existing = json!({"future":1,"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"native herdr hook"}]}],"Stop":[]}});
    fs::write(home.join("hooks.json"), existing.to_string()).unwrap();
    fs::write(home.join("auth.json"), "credentials fixture").unwrap();
    fs::write(home.join("config.toml"), "model = 'user-model'\n").unwrap();
    success(f.crew(&["codex-install"]));
    let installed = fs::read(home.join("hooks.json")).unwrap();
    success(f.crew(&["codex-install"]));
    assert_eq!(fs::read(home.join("hooks.json")).unwrap(), installed);
    success(f.crew(&["codex-uninstall"]));
    let restored: Value =
        serde_json::from_slice(&fs::read(home.join("hooks.json")).unwrap()).unwrap();
    assert_eq!(restored, existing);
    assert_eq!(
        fs::read_to_string(home.join("auth.json")).unwrap(),
        "credentials fixture"
    );
    assert_eq!(
        fs::read_to_string(home.join("config.toml")).unwrap(),
        "model = 'user-model'\n"
    );
    assert_eq!(f.count(&["workspace", "list"]), 0);
}

#[test]
fn codex_only_check_does_not_require_claude_and_bad_profiles_explain_the_failure() {
    let f = Fixture::new();
    f.codex_tools();
    f.codex_config(true);
    let checked = success(f.crew(&["check"]));
    assert!(checked.contains("codex is on the PATH"));
    assert!(!checked.contains("claude"));
    let config = f
        .read(".herdr/crew.toml")
        .replace("[codex]", "[codex]\nprofile = \"missing\"");
    fs::write(f.repo.join(".herdr/crew.toml"), config).unwrap();
    let failed = f.crew(&["up", "--no-attach"]);
    assert!(!failed.status.success());
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("Codex profile missing: cannot read"),
        "{}",
        String::from_utf8_lossy(&failed.stderr)
    );
    assert_eq!(f.count(&["workspace", "create"]), 0);
    f.no_generated_files();
    fs::create_dir_all(f.root.join(".codex")).unwrap();
    fs::write(
        f.root.join(".codex/missing.config.toml"),
        "[hooks]\nenabled = false\n",
    )
    .unwrap();
    failure(
        f.crew(&["up", "--no-attach"]),
        "cannot inspect profile hooks",
    );
    assert_eq!(f.count(&["workspace", "create"]), 0);
    fs::write(
        f.root.join(".codex/missing.config.toml"),
        "model = \"profile-model\"\n",
    )
    .unwrap();
    success(f.crew(&["check"]));
    failure(
        f.command(CREW)
            .arg("check")
            .env("CREW_TEST_HERDR_VERSION", "0.9.2")
            .output()
            .unwrap(),
        "0.9.3 or newer is required",
    );
}

#[test]
fn pi_roles_need_the_extension_then_carry_their_prompt_and_options() {
    let f = Fixture::new();
    let tools = f.root.join("tools");
    fs::create_dir_all(&tools).unwrap();
    // pi prints its version on stderr; 0.73.1 is too old for herdr to resume it.
    fs::write(tools.join("pi"), "#!/bin/sh\necho 0.73.1 >&2\n").unwrap();
    fs::set_permissions(tools.join("pi"), fs::Permissions::from_mode(0o755)).unwrap();
    let config = f
        .read(".herdr/crew.toml")
        .replace("version = 1", "version = 1\nkind = \"pi\"")
        .replace("[workspace]", "[pi]\nprovider = \"anthropic\"\n[workspace]");
    fs::write(
        f.repo.join(".herdr/crew.toml"),
        format!("{config}\n[roles.pi]\nmodel = \"sonnet\"\nthinking = \"high\"\n"),
    )
    .unwrap();

    // Without the extension a resumed conversation would lose its role: stop before any change.
    failure(f.crew(&["up", "--no-attach"]), "herdr-crew pi-install");
    assert_eq!(f.count(&["workspace", "create"]), 0);
    f.no_generated_files();
    failure(f.crew(&["check"]), "herdr-crew pi-install");

    let extension = f.root.join(".pi/agent/extensions/herdr-crew.ts");
    fs::create_dir_all(extension.parent().unwrap()).unwrap();
    fs::write(&extension, "// someone else's extension\n").unwrap();
    failure(f.crew(&["pi-install"]), "was not installed by herdr-crew");
    failure(f.crew(&["pi-uninstall"]), "was not installed by herdr-crew");
    fs::remove_file(&extension).unwrap();

    success(f.crew(&["pi-install"]));
    assert!(
        fs::read_to_string(&extension)
            .unwrap()
            .contains("herdr-crew-role")
    );
    failure(f.crew(&["check"]), "pi 0.73.1 is too old: 1.1.0 or newer");
    failure(f.crew(&["up", "--no-attach"]), "pi 0.73.1 is too old");
    assert_eq!(f.count(&["workspace", "create"]), 0);
    fs::write(tools.join("pi"), "#!/bin/sh\necho 1.1.0 >&2\n").unwrap();
    let checked = success(f.crew(&["check"]));
    assert!(checked.contains("herdr-crew: pi 1.1.0 at "), "{checked}");
    assert!(
        checked.contains("the crew pi extension is current"),
        "{checked}"
    );
    assert!(!checked.contains("claude"), "{checked}");

    f.up();
    success(f.crew(&["add", "dev"]));
    let starts: Vec<_> = f
        .calls()
        .into_iter()
        .filter(|c| starts_with(c, &["agent", "start"]))
        .collect();
    assert_eq!(starts.len(), 3);
    for (call, role, options) in [
        (&starts[0], "lead", vec!["--provider", "anthropic"]),
        (
            &starts[1],
            "dev",
            vec![
                "--provider",
                "anthropic",
                "--model",
                "sonnet",
                "--thinking",
                "high",
            ],
        ),
        (
            &starts[2],
            "dev-2",
            vec![
                "--provider",
                "anthropic",
                "--model",
                "sonnet",
                "--thinking",
                "high",
            ],
        ),
    ] {
        let prompt = f.repo.join(format!(".herdr/prompts/{role}.txt"));
        assert_eq!(
            &call[..6],
            ["agent", "start", role, "--kind", "pi", "--pane"]
        );
        let native = call.iter().position(|a| a == "--").unwrap() + 1;
        let mut args = vec![
            "--herdr-crew-role".to_string(),
            role.into(),
            "--herdr-crew-prompt".into(),
            prompt.display().to_string(),
        ];
        args.extend(options.into_iter().map(String::from));
        args.push(MESSAGE.into());
        assert_eq!(&call[native..], args.as_slice());
        assert!(
            fs::read_to_string(&prompt)
                .unwrap()
                .contains(&format!(" {role} "))
        );
    }
    f.up();
    assert_eq!(f.count(&["agent", "start"]), 3);

    // A modified extension is not the reviewed one.
    fs::write(
        &extension,
        format!(
            "{}\n// local edit\n",
            fs::read_to_string(&extension).unwrap()
        ),
    )
    .unwrap();
    failure(f.crew(&["check"]), "outdated or modified");
    success(f.crew(&["pi-uninstall"]));
    assert!(!extension.exists());
    success(f.crew(&["pi-uninstall"]));
}

#[test]
fn check_reports_versions_base_and_what_it_did_not_check() {
    let f = Fixture::new();
    f.claude_tool(0o755);
    let checked = success(f.crew(&["check"]));
    for expected in [
        "herdr-crew: git ",
        &format!("herdr-crew: herdr 0.9.3 at {}", f.herdr.display()),
        "herdr-crew: claude 2.1.0 at ",
        "worktrees.base origin/main is known locally",
        "0 existing role worktree(s) are valid",
        "not checked: network access to origin, agent sign-in and model access",
    ] {
        assert!(checked.contains(expected), "{expected:?} in {checked}");
    }
    f.git(&["update-ref", "-d", "refs/remotes/origin/main"]);
    let checked = success(f.crew(&["check"]));
    assert!(
        checked.contains("origin/main is not fetched yet; `up` fetches it"),
        "{checked}"
    );
}

#[test]
fn check_rejects_unusable_tools() {
    let f = Fixture::new();
    f.claude_tool(0o644);
    failure(
        f.crew(&["check"]),
        "tools/claude is on the PATH but not executable",
    );
    f.claude_tool(0o755);
    failure(
        f.command(CREW)
            .arg("check")
            .env("CREW_TEST_HERDR_VERSION", "0.9.0")
            .output()
            .unwrap(),
        "is too old: 0.9.1 or newer is required",
    );
    failure(
        f.command(CREW)
            .arg("check")
            .env("HERDR_BIN_PATH", f.root.join("no-herdr"))
            .output()
            .unwrap(),
        "HERDR_BIN_PATH: ",
    );
    let not_executable = f.root.join("herdr-copy");
    fs::copy(&f.herdr, &not_executable).unwrap();
    fs::set_permissions(&not_executable, fs::Permissions::from_mode(0o644)).unwrap();
    failure(
        f.command(CREW)
            .arg("check")
            .env("HERDR_BIN_PATH", &not_executable)
            .output()
            .unwrap(),
        "herdr-copy is not an executable file",
    );
}

#[test]
fn invalid_base_fails_check_and_up_before_any_change() {
    let f = Fixture::new();
    f.claude_tool(0o755);
    for (base, reason) in [
        ("missing/main", "remote \"missing\" is not configured"),
        ("origin/invalid..branch", "is not a valid Git branch name"),
    ] {
        let config = f
            .read(".herdr/crew.toml")
            .replace("base = \"origin/main\"", &format!("base = \"{base}\""));
        fs::write(f.repo.join(".herdr/crew.toml"), &config).unwrap();
        failure(f.crew(&["check"]), reason);
        failure(f.crew(&["up", "--no-attach"]), reason);
        assert_eq!(f.count(&["workspace", "create"]), 0, "{base}");
        f.no_generated_files();
        assert!(!f.repo.join(".worktrees").exists(), "{base}");
        let restored = config.replace(&format!("base = \"{base}\""), "base = \"origin/main\"");
        fs::write(f.repo.join(".herdr/crew.toml"), restored).unwrap();
    }
}

/// Native Herdr RPC simulator for the hook and resume subprocesses. This tests the wire
/// protocol and filesystem state independently of the planning CLI simulator.
struct RpcHost {
    state: std::sync::Arc<std::sync::Mutex<Value>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl RpcHost {
    fn new(f: &Fixture) -> Self {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex};
        let workspace = f.workspace();
        let state = Arc::new(Mutex::new(json!({
            "workspaces": [{"workspace_id":workspace,"label":"crew"}],
            "tabs": f.result(&["tab", "list", "--workspace", &workspace])["tabs"],
            "panes": f.result(&["pane", "list", "--workspace", &workspace])["panes"],
            "reports": [], "reject_resume": 2,
        })));
        let stop = Arc::new(AtomicBool::new(false));
        let listener = UnixListener::bind(f.root.join("herdr.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let thread_state = Arc::clone(&state);
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                // On macOS an accepted socket inherits the listener's O_NONBLOCK, which would
                // make the read below fail with WouldBlock instead of waiting for the request.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                let mut state = thread_state.lock().unwrap();
                let params = &request["params"];
                let result = match request["method"].as_str().unwrap() {
                    "workspace.list" => json!({"workspaces":state["workspaces"]}),
                    "tab.list" => json!({"tabs":state["tabs"]}),
                    "pane.list" => json!({"panes":state["panes"]}),
                    "pane.get" => {
                        json!({"pane":state["panes"].as_array().unwrap().iter().find(|pane| pane["pane_id"] == params["pane_id"]).unwrap()})
                    }
                    "pane.report_agent_session" => {
                        if params.get("resume_argv").is_some()
                            && state["reject_resume"].as_u64().unwrap() > 0
                        {
                            let remaining = state["reject_resume"].as_u64().unwrap() - 1;
                            state["reject_resume"] = json!(remaining);
                            writeln!(stream,"{}",json!({"id":request["id"],"error":{"code":"resume_not_accepted","message":"process detection pending"}})).unwrap();
                            continue;
                        }
                        if let Some(id) = params.get("agent_session_id") {
                            let pane = state["panes"]
                                .as_array_mut()
                                .unwrap()
                                .iter_mut()
                                .find(|pane| pane["pane_id"] == params["pane_id"])
                                .unwrap();
                            pane["agent_session"] = json!({"agent":"codex","kind":"id","source":"herdr:codex","value":id});
                        }
                        state["reports"]
                            .as_array_mut()
                            .unwrap()
                            .push(params.clone());
                        json!({})
                    }
                    other => panic!("unexpected Herdr RPC {other}"),
                };
                writeln!(stream, "{}", json!({"id":request["id"],"result":result})).unwrap();
            }
        });
        Self {
            state,
            stop,
            thread: Some(thread),
        }
    }

    fn pane(&self, name: &str) -> Value {
        let state = self.state.lock().unwrap();
        let tab = state["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tab| tab["label"] == name)
            .unwrap();
        state["panes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|pane| pane["tab_id"] == tab["tab_id"])
            .unwrap()
            .clone()
    }

    fn command(&self, f: &Fixture, pane: &Value) -> Command {
        let mut command = f.command(CREW);
        command
            .env("HERDR_ENV", "1")
            .env("HERDR_SOCKET_PATH", f.root.join("herdr.sock"))
            .env("HERDR_WORKSPACE_ID", pane["workspace_id"].as_str().unwrap())
            .env("HERDR_TAB_ID", pane["tab_id"].as_str().unwrap())
            .env("HERDR_PANE_ID", pane["pane_id"].as_str().unwrap())
            .current_dir(pane["cwd"].as_str().unwrap());
        command
    }

    fn hook(&self, f: &Fixture, name: &str, source: &str, id: &str) -> Value {
        use std::io::Write;
        use std::process::Stdio;
        let pane = self.pane(name);
        let mut child = self
            .command(f, &pane)
            .arg("codex-hook")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        write!(child.stdin.take().unwrap(), "{}", json!({"hook_event_name":"SessionStart","session_id":id,"source":source,"cwd":pane["cwd"]})).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

impl Drop for RpcHost {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}

#[test]
fn shared_roles_receive_distinct_context_and_keep_it_through_clear_and_compaction() {
    let f = Fixture::new();
    f.codex_tools();
    f.codex_config(true);
    f.up();
    let host = RpcHost::new(&f);
    let lead = host.hook(&f, "lead", "startup", "lead-id");
    let dev = host.hook(&f, "dev", "startup", "dev-id");
    let lead_context = lead["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    let dev_context = dev["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(lead_context.contains("Lead lead"));
    assert!(dev_context.contains("Developer dev"));
    assert_ne!(lead_context, dev_context);
    let listed = success(f.crew(&["codex-list"]));
    assert!(listed.contains("lead-id") && listed.contains("dev-id"));
    assert_eq!(host.hook(&f, "lead", "compact", "lead-id"), lead);
    assert_eq!(host.hook(&f, "lead", "clear", "new-lead-id"), lead);
    let wrong = host.hook(&f, "lead", "resume", "other-id");
    assert_eq!(wrong["continue"], false);
    let duplicate = host.hook(&f, "dev", "clear", "new-lead-id");
    assert_eq!(duplicate["continue"], false);
    assert_eq!(host.hook(&f, "lead", "startup", "unrelated-id"), json!({}));
    let state = host.state.lock().unwrap();
    let reports = state["reports"].as_array().unwrap();
    let resume = reports
        .iter()
        .find(|r| r.get("resume_argv").is_some())
        .unwrap();
    assert_eq!(resume["source"], "herdr-crew:codex");
    assert_eq!(resume["resume_argv"][0], "herdr-crew");
    assert_eq!(resume["resume_argv"][1], "codex-resume");
    assert!(resume.get("agent_session_id").is_none());
}

#[test]
fn resume_rebinds_saved_options_after_configuration_changes_without_initial_message() {
    let f = Fixture::new();
    f.codex_tools();
    f.codex_config(true);
    fs::create_dir(f.root.join(".codex")).unwrap();
    fs::write(
        f.root.join(".codex/saved.config.toml"),
        "model = \"profile-model\"\n",
    )
    .unwrap();
    fs::write(
        f.repo.join(".herdr/crew.toml"),
        f.read(".herdr/crew.toml")
            .replace("[codex]", "[codex]\nprofile = \"saved\""),
    )
    .unwrap();
    f.up();
    let host = RpcHost::new(&f);
    let original = host.hook(&f, "lead", "startup", "lead-id");
    let snapshot = f
        .snapshots()
        .into_iter()
        .find(|s| s["name"] == "lead")
        .unwrap();
    let token = snapshot["token"].as_str().unwrap();
    fs::write(
        f.repo.join(".herdr/crew.toml"),
        f.read(".herdr/crew.toml")
            .replace("original-model", "changed-model")
            .replace("Lead {{NAME}}", "Changed role"),
    )
    .unwrap();
    {
        let mut state = host.state.lock().unwrap();
        let tab = state["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["label"] == "lead")
            .unwrap()["tab_id"]
            .clone();
        let pane = state["panes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|p| p["tab_id"] == tab)
            .unwrap();
        pane["pane_id"] = json!("restored-pane");
        pane["terminal_id"] = json!("restored-terminal");
    }
    let pane = host.pane("lead");
    success(
        host.command(&f, &pane)
            .env("CODEX_THREAD_ID", "inherited-other-session")
            .env("CODEX_HOME", f.root.join("other-home"))
            .args(["codex-resume", token])
            .output()
            .unwrap(),
    );
    let resumed: Value =
        serde_json::from_slice(&fs::read(f.state.join("codex-resume.json")).unwrap()).unwrap();
    let args = resumed["args"].as_array().unwrap();
    assert_eq!(args[0], "resume");
    assert_eq!(args.last().unwrap(), "lead-id");
    assert!(args.contains(&json!("model=\"original-model\"")));
    assert!(
        args.windows(2)
            .any(|pair| pair == [json!("--profile"), json!("saved")])
    );
    assert!(!args.contains(&json!(MESSAGE)));
    assert_eq!(resumed["inherited_thread"], Value::Null);
    assert_eq!(resumed["home"], snapshot["codex_home"]);
    assert_eq!(host.hook(&f, "lead", "resume", "lead-id"), original);
    let mut renamed = host.state.lock().unwrap();
    let tab = renamed["tabs"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|tab| tab["label"] == "lead")
        .unwrap();
    tab["label"] = json!("unrelated");
    drop(renamed);
    assert!(
        !host
            .command(&f, &pane)
            .args(["codex-resume", token])
            .output()
            .unwrap()
            .status
            .success()
    );
    host.state.lock().unwrap()["tabs"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|tab| tab["label"] == "unrelated")
        .unwrap()["label"] = json!("lead");
    fs::remove_file(f.repo.join(format!(".herdr/codex/snapshots/{token}.json"))).unwrap();
    assert!(
        !host
            .command(&f, &pane)
            .args(["codex-resume", token])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
#[ignore = "needs CREW_REAL_CODEX=1 and Codex CLI 0.160.1; uses only a temporary home and local mock provider"]
fn real_codex_sends_exact_crew_hook_context_as_additive_developer_guidance() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    if std::env::var_os("CREW_REAL_CODEX").is_none() {
        return;
    }
    let real = success(Command::new("which").arg("codex").output().unwrap())
        .trim()
        .to_string();
    inspect::version(&real, (0, 160, 1)).unwrap();
    let f = Fixture::new();
    f.codex_tools();
    f.codex_config(true);
    let config = f.read(".herdr/crew.toml");
    // Larger than Codex's default spill threshold, with shell-sensitive literal text.
    // npm's Codex entry point needs Node; expose only that executable to the isolated home.
    let node = Command::new("which").arg("node").output().unwrap();
    if node.status.success() {
        std::os::unix::fs::symlink(
            String::from_utf8(node.stdout).unwrap().trim(),
            f.root.join("tools/node"),
        )
        .unwrap();
    }
    let role = format!(
        "Developer {{NAME}} in {{REPO}}. {} «á» ' $HOME `id`",
        "x".repeat(32_000)
    );
    let role = role
        .replace("{NAME}", "{{NAME}}")
        .replace("{REPO}", "{{REPO}}");
    let value = serde_json::to_string(&role).unwrap();
    fs::write(
        f.repo.join(".herdr/crew.toml"),
        config.replace(
            "\"Lead {{NAME}} in {{REPO}}; launcher {{LAUNCHER}}.\"",
            &value,
        ),
    )
    .unwrap();
    f.up();
    success(f.crew(&["codex-install"]));
    let host = RpcHost::new(&f);
    let pane = host.pane("lead");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let (send, receive) = mpsc::sync_channel(1);
    let provider = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
        loop {
            let (mut stream, _) = match listener.accept() {
                Ok(value) => value,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
                Err(_) => return,
            };
            // Blocking again: macOS accepted sockets inherit the listener's O_NONBLOCK.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            while !bytes.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                bytes.push(byte[0]);
                assert!(bytes.len() < 64 * 1024);
            }
            let header = String::from_utf8(bytes).unwrap();
            let length = header
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            assert!(length < 4 * 1024 * 1024);
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            if !header.starts_with("POST ") {
                write!(
                    stream,
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
                continue;
            }
            let body: Value = serde_json::from_slice(&body).unwrap();
            send.send(body).unwrap();
            let message = json!({"id":"msg_offline","type":"message","role":"assistant","content":[{"type":"output_text","text":"offline verification","annotations":[]}]});
            let events = format!(
                "event: response.created\ndata: {}\n\nevent: response.output_item.done\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
                json!({"type":"response.created","response":{"id":"resp_offline","status":"in_progress","output":[]}}),
                json!({"type":"response.output_item.done","output_index":0,"item":message}),
                json!({"type":"response.completed","response":{"id":"resp_offline","status":"completed","output":[message]}})
            );
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{events}",events.len()).unwrap();
            return;
        }
    });
    let home = f.root.join(".codex");
    let configuration = format!(
        "model = \"offline-test\"\nmodel_provider = \"offline\"\n[model_providers.offline]\nname = \"Offline test\"\nbase_url = \"http://{address}\"\nwire_api = \"responses\"\nrequires_openai_auth = false\n"
    );
    fs::write(home.join("config.toml"), &configuration).unwrap();
    fs::write(
        home.join("crew-test.config.toml"),
        "developer_instructions = \"Preserve the existing profile guidance.\"\n",
    )
    .unwrap();
    let spawn = || {
        // Use the installed CLI for this test; pane helpers still resolve our guarded tools.
        let mut actual = f.command(&real);
        actual
            .env("HERDR_ENV", "1")
            .env("HERDR_SOCKET_PATH", f.root.join("herdr.sock"))
            .env("HERDR_WORKSPACE_ID", pane["workspace_id"].as_str().unwrap())
            .env("HERDR_TAB_ID", pane["tab_id"].as_str().unwrap())
            .env("HERDR_PANE_ID", pane["pane_id"].as_str().unwrap())
            .args([
                "app-server",
                "--stdio",
                "-c",
                "features.hooks=true",
                "-c",
                "features.enable_request_compression=false",
            ]);
        inspect::Inspector::spawn(&mut actual).unwrap()
    };
    let initialize = |process: &mut inspect::Inspector| {
        process.send(json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"crew-test","version":"1"},"capabilities":{"experimentalApi":true}}})).unwrap();
        process.response(1).unwrap();
        process.send(json!({"method":"initialized"})).unwrap();
    };
    let mut process = spawn();
    initialize(&mut process);
    process
        .send(json!({"id":2,"method":"hooks/list","params":{"cwds":[f.repo]}}))
        .unwrap();
    let hooks = process.response(2).unwrap();
    let hook = &hooks["data"][0]["hooks"][0];
    assert_eq!(hook["trustStatus"], "untrusted");
    // Trust only the exact hook this test installed and reviewed, in its temporary home.
    // Production code never writes hooks.state or bypasses hook trust.
    fs::write(
        home.join("config.toml"),
        format!(
            "{configuration}\n[hooks.state.{}]\ntrusted_hash = {}\n",
            serde_json::to_string(hook["key"].as_str().unwrap()).unwrap(),
            serde_json::to_string(hook["currentHash"].as_str().unwrap()).unwrap()
        ),
    )
    .unwrap();
    drop(process);
    // Exercise profile validation and read-only hook inspection through the real CLI.
    let actual_tools = f.root.join("actual-tools");
    fs::create_dir(&actual_tools).unwrap();
    std::os::unix::fs::symlink(&real, actual_tools.join("codex")).unwrap();
    fs::write(
        f.repo.join(".herdr/crew.toml"),
        f.read(".herdr/crew.toml")
            .replace("[codex]", "[codex]\nprofile = \"crew-test\""),
    )
    .unwrap();
    success(
        f.command(CREW)
            .arg("check")
            .env(
                "PATH",
                format!(
                    "{}:{}:/usr/bin:/bin",
                    actual_tools.display(),
                    f.root.join("tools").display()
                ),
            )
            .output()
            .unwrap(),
    );
    let mut process = spawn();
    initialize(&mut process);
    process
        .send(json!({"id":3,"method":"thread/start","params":{"cwd":f.repo,"ephemeral":true,"config":{"developer_instructions":"Preserve the existing profile guidance."}}}))
        .unwrap();
    let started = process.response(3).unwrap();
    let id = started["thread"]["id"].as_str().unwrap();
    process.send(json!({"id":4,"method":"turn/start","params":{"threadId":id,"input":[{"type":"text","text":"Verify context only"}]}})).unwrap();
    process.response(4).unwrap();
    let request = receive
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("Codex must send its request only to our local mock provider");
    let expected = f
        .snapshots()
        .into_iter()
        .find(|s| s["name"] == "lead")
        .unwrap()["prompt"]
        .as_str()
        .unwrap()
        .to_string();
    let developers: Vec<_> = request["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["role"] == "developer")
        .flat_map(|item| item["content"].as_array().unwrap())
        .filter_map(|item| item["text"].as_str())
        .collect();
    assert!(
        developers.iter().any(|text| text.contains(&expected)),
        "the complete crew hook output must be model-visible developer context"
    );
    assert!(
        developers
            .iter()
            .any(|text| text.contains("Preserve the existing profile guidance.")),
        "existing developer guidance remains additive"
    );
    assert!(
        request["instructions"]
            .as_str()
            .is_some_and(|base| !base.is_empty() && !base.contains(&expected)),
        "Codex built-in instructions remain separate"
    );
    drop(process);
    provider.join().unwrap();
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

/// The project's lock as another herdr-crew command would hold it.
fn hold_project_lock(f: &Fixture) -> fs::File {
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(f.repo.join(".git/herdr-crew.lock"))
        .unwrap();
    lock.lock().unwrap();
    lock
}

#[test]
fn up_waits_for_another_command_before_reading_any_state() {
    use std::process::Stdio;
    let f = Fixture::new();
    let lock = hold_project_lock(&f);
    let mut up = f
        .command(CREW)
        .args(["up", "--no-attach"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Wait for up to announce that it is waiting; a fresh binary can be slow to start.
    let stderr = up.stderr.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        use std::io::BufRead;
        let mut rest = String::new();
        for line in std::io::BufReader::new(stderr).lines() {
            let line = line.unwrap();
            if line.contains("waiting up to 120 s") {
                send.send(()).unwrap();
            } else {
                rest.push_str(&line);
                rest.push('\n');
            }
        }
        rest
    });
    receive
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("up announces that it waits for the lock");
    assert!(
        up.try_wait().unwrap().is_none(),
        "up must wait for the lock"
    );
    assert!(f.calls().is_empty(), "no herdr call before the lock");
    f.no_generated_files();
    drop(lock);
    let status = up.wait().unwrap();
    let rest = reader.join().unwrap();
    assert!(status.success(), "{rest}");
    assert_eq!(f.labels(), ["dev", "lead", "status"]);
    assert_eq!(f.agents(), ["dev", "lead"]);
    // The lock is free again for the next command.
    assert!(f.up().contains("nothing to do"));
}

#[test]
fn startup_leaves_a_locked_project_to_the_command_holding_it() {
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
    let lock = hold_project_lock(&f);
    let out = f
        .command(CREW)
        .arg("startup")
        .env("HERDR_PLUGIN_ID", "herdr-crew")
        .env(
            "HERDR_PLUGIN_CONTEXT_JSON",
            json!({"workspace_id": id}).to_string(),
        )
        .output()
        .unwrap();
    let stdout = success(out);
    assert!(
        stdout.contains("another herdr-crew command is changing it"),
        "{stdout}"
    );
    assert_eq!(f.count(&["workspace", "rename"]), 0);
    assert_eq!(f.count(&["tab", "create"]), 0);
    assert_eq!(f.count(&["agent", "start"]), 0);
    f.no_generated_files();
    drop(lock);
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
