//! Test against a real herdr, isolated through XDG (design §7.2). Runs only with `CREW_REAL_HERDR=1`:
//!
//! ```sh
//! CREW_REAL_HERDR=1 cargo test --test real_herdr -- --ignored --nocapture
//! ```
//!
//! Starts `herdr --session crewtest server` with `XDG_CONFIG_HOME` and `XDG_STATE_HOME` in a
//! temporary directory and without the caller's `HERDR_*` and `CLAUDE*` variables, over a
//! temporary git repository. When it ends, also on failure, it stops the session and deletes the
//! temporary directory.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::Value;

const SESSION: &str = "crewtest";
const BIN: &str = env!("CARGO_BIN_EXE_herdr-crew");
/// macOS `sun_path`: 104 bytes including the trailing NUL.
const MAX_SOCKET: usize = 103;

struct Isolated {
    dir: PathBuf,
    repo: PathBuf,
    socket: PathBuf,
    server: Option<Child>,
}

impl Isolated {
    /// A clean environment: none of the `HERDR_*` or `CLAUDE*` variables of the calling pane.
    fn command(&self, program: &str) -> Command {
        let mut cmd = Command::new(program);
        cmd.env_clear();
        for key in ["HOME", "USER", "LOGNAME", "PATH", "LANG", "SHELL", "TMPDIR"] {
            if let Some(v) = std::env::var_os(key) {
                cmd.env(key, v);
            }
        }
        cmd.env("TERM", "xterm-256color")
            .env("XDG_CONFIG_HOME", self.dir.join("c"))
            .env("XDG_STATE_HOME", self.dir.join("s"))
            .env("HERDR_SOCKET_PATH", &self.socket)
            .current_dir(&self.repo)
            .stdin(Stdio::null());
        cmd
    }

    fn herdr(&self, args: &[&str]) -> Output {
        self.command("herdr").args(args).output().expect("herdr")
    }

    fn result(&self, args: &[&str]) -> Value {
        let out = self.herdr(args);
        let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
            panic!(
                "herdr {args:?}: {}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        assert!(v.get("error").is_none(), "herdr {args:?}: {v}");
        v["result"].clone()
    }

    fn crew(&self, args: &[&str]) -> Output {
        let out = self
            .command(BIN)
            .env("CREW_AGENT_CMD", "cat")
            .args(args)
            .output()
            .expect("crew");
        println!(
            "$ herdr-crew {}\n{}{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn workspace(&self) -> String {
        let ws = self.result(&["workspace", "list"]);
        ws["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["label"] == "crewtest")
            .map(|w| w["workspace_id"].as_str().unwrap().to_string())
            .expect("workspace crewtest")
    }

    fn tabs(&self) -> Vec<(String, String)> {
        let r = self.result(&["tab", "list", "--workspace", &self.workspace()]);
        r["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                (
                    t["label"].as_str().unwrap().to_string(),
                    t["tab_id"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    fn labels(&self) -> Vec<String> {
        self.tabs().into_iter().map(|(l, _)| l).collect()
    }

    fn pane_of(&self, label: &str) -> String {
        let tab = self
            .tabs()
            .into_iter()
            .find(|(l, _)| l == label)
            .expect("tab")
            .1;
        let r = self.result(&["pane", "list", "--workspace", &self.workspace()]);
        r["panes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["tab_id"] == tab.as_str())
            .map(|p| p["pane_id"].as_str().unwrap().to_string())
            .expect("pane")
    }

    fn process_info(&self, pane: &str) -> Value {
        self.result(&["pane", "process-info", "--pane", pane])["process_info"].clone()
    }

    fn screen(&self, pane: &str) -> String {
        String::from_utf8_lossy(
            &self
                .herdr(&["pane", "read", pane, "--source", "visible"])
                .stdout,
        )
        .into_owned()
    }
}

impl Drop for Isolated {
    fn drop(&mut self) {
        // Never `server stop` without a session: the test session is stopped by name.
        let _ = self.herdr(&["session", "stop", SESSION]);
        sleep(Duration::from_millis(500));
        let _ = self.herdr(&["session", "delete", SESSION]);
        // Reap the server process; kill it if it did not end with the session (the test started it).
        if let Some(mut child) = self.server.take() {
            let start = Instant::now();
            while matches!(child.try_wait(), Ok(None)) && start.elapsed() < Duration::from_secs(5) {
                sleep(Duration::from_millis(100));
            }
            if matches!(child.try_wait(), Ok(None)) {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) {
        if ok() {
            return;
        }
        sleep(Duration::from_millis(250));
    }
    panic!("timed out waiting for: {what}");
}

fn shell_in_foreground(info: &Value) -> bool {
    info["foreground_process_group_id"] == info["shell_pid"]
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(path: PathBuf, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

const CREW_TOML: &str = r#"version = 1

common_prompt = '''
Board: {{STATUS}} ({{SCHEMA}}).
'''

[workspace]
label = "crewtest"

[board]
tab = "ct-status"
writer = "ct-lead"

[worktrees]
dir = ".wt"
base = "origin/main"

[[roles]]
name = "ct-lead"
prompt = '''
You are {{NAME}}, the lead in {{REPO}}. For more hands: {{LAUNCHER}} add ct-dev.
'''

[[roles]]
name = "ct-dev"
prompt = '''
You are {{NAME}}, a developer.
'''
worktree = true
extra = true
"#;

fn setup() -> Isolated {
    // A short name: herdr's client socket lives under XDG_CONFIG_HOME and macOS does not accept
    // socket paths longer than 103 bytes.
    let dir = std::env::temp_dir().join(format!("cr{:04x}", std::process::id() & 0xffff));
    let socket = dir
        .join("c/herdr/sessions")
        .join(SESSION)
        .join("herdr.sock");
    let client = socket.with_file_name("herdr-client.sock");
    assert!(
        client.as_os_str().len() <= MAX_SOCKET,
        "socket path too long ({} bytes): {}",
        client.as_os_str().len(),
        client.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let origin = dir.join("origin.git");
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(
        &dir,
        &[
            "init",
            "--quiet",
            "--bare",
            "-b",
            "main",
            origin.to_str().unwrap(),
        ],
    );
    git(&repo, &["init", "--quiet", "-b", "main"]);
    write(
        repo.join(".gitignore"),
        "/.wt/\n/.herdr/status.json\n/.herdr/status.json.tmp\n/.herdr/prompts/\n/.herdr/status.schema.json\n",
    );
    write(repo.join(".herdr/crew.toml"), CREW_TOML);
    git(
        &repo,
        &[
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "user.name=test",
            "add",
            "-A",
        ],
    );
    git(
        &repo,
        &[
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "user.name=test",
            "commit",
            "--quiet",
            "-m",
            "initial",
        ],
    );
    git(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&repo, &["push", "--quiet", "origin", "main"]);

    let mut iso = Isolated {
        dir,
        repo,
        socket,
        server: None,
    };
    let mut server = iso.command("herdr");
    server
        .args(["--session", SESSION, "server"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        server.process_group(0);
    }
    iso.server = Some(server.spawn().expect("herdr server"));
    wait_until("the isolated server answers", || {
        iso.herdr(&["workspace", "list"]).status.success()
    });

    // Abort unless the server is the one under the temporary directory.
    let status =
        String::from_utf8_lossy(&iso.herdr(&["--session", SESSION, "status"]).stdout).into_owned();
    let expected = format!("socket: {}", iso.socket.display());
    assert!(
        status.contains(&expected),
        "the socket is not under the temporary directory:\n{status}"
    );
    iso
}

#[test]
#[ignore = "needs herdr; CREW_REAL_HERDR=1 cargo test --test real_herdr -- --ignored"]
fn real_herdr() {
    if std::env::var_os("CREW_REAL_HERDR").is_none() {
        eprintln!("CREW_REAL_HERDR is not set; skipping");
        return;
    }
    let real_plugins = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".config/herdr/plugins.json"))
        .unwrap();
    let before = std::fs::read(&real_plugins).ok();
    {
        let iso = setup();
        scenario(&iso);
    }
    assert_eq!(
        std::fs::read(&real_plugins).ok(),
        before,
        "{} changed",
        real_plugins.display()
    );
}

fn scenario(iso: &Isolated) {
    // 1. `up` creates the workspace, tabs, worktree, prompts and board; running it again duplicates nothing.
    let out = iso.crew(&["up", "--no-attach"]);
    assert!(out.status.success());
    assert_eq!(iso.labels(), ["ct-lead", "ct-dev", "ct-status"]);
    assert!(iso.repo.join(".wt/ct-dev").is_dir());
    let prompt = std::fs::read_to_string(iso.repo.join(".herdr/prompts/ct-lead.txt")).unwrap();
    assert!(
        prompt.contains("You are ct-lead, the lead in")
            && prompt.contains(&format!("{BIN} add ct-dev")),
        "{prompt}"
    );
    assert!(prompt.contains("Board: .herdr/status.json (.herdr/status.schema.json)."));
    assert!(
        iso.repo.join(".herdr/status.schema.json").is_file()
            && iso.repo.join(".herdr/status.json").is_file()
    );
    let board = iso.pane_of("ct-status");
    wait_until("the board is drawn", || {
        iso.screen(&board).contains("Project repo")
    });
    let main = iso.pane_of("ct-lead");
    wait_until("cat runs in ct-lead", || {
        !shell_in_foreground(&iso.process_info(&main))
    });

    let out = iso.crew(&["up", "--no-attach"]);
    assert!(out.status.success());
    assert_eq!(iso.labels(), ["ct-lead", "ct-dev", "ct-status"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("tab ct-lead has no agent"), "{stderr}");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("typed in"));

    // 2. Foreground check: after the viewer exits the shell is in the foreground and `up` repairs
    // the board; with another process in the pane, `up` types nothing.
    assert!(
        iso.herdr(&["pane", "send-keys", &board, "q"])
            .status
            .success()
    );
    wait_until("the viewer exits", || {
        shell_in_foreground(&iso.process_info(&board))
    });
    println!(
        "process-info with the shell idle: {}",
        iso.process_info(&board)
    );
    let out = iso.crew(&["up", "--no-attach"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains(&format!("typed in {board}")));
    wait_until("the viewer is back", || {
        !shell_in_foreground(&iso.process_info(&board))
    });
    wait_until("the board is drawn again", || {
        iso.screen(&board).contains("Project repo")
    });

    assert!(
        iso.herdr(&["pane", "send-keys", &board, "q"])
            .status
            .success()
    );
    wait_until("the viewer exits again", || {
        shell_in_foreground(&iso.process_info(&board))
    });
    assert!(iso.herdr(&["pane", "run", &board, "cat"]).status.success());
    wait_until("cat in the foreground", || {
        !shell_in_foreground(&iso.process_info(&board))
    });
    println!("process-info with cat: {}", iso.process_info(&board));
    let out = iso.crew(&["up", "--no-attach"]);
    assert!(out.status.success());
    assert!(!String::from_utf8_lossy(&out.stdout).contains("typed in"));
    let names: Vec<String> = iso.process_info(&board)["foreground_processes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["cat"]);

    // add and close of an extra instance; close does not close a base role.
    let out = iso.crew(&["add", "ct-dev"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("new instance ct-dev-2"));
    assert_eq!(iso.labels(), ["ct-lead", "ct-dev", "ct-status", "ct-dev-2"]);
    assert!(iso.repo.join(".wt/ct-dev-2").is_dir());
    let out = iso.crew(&["close", "ct-dev-2"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("its worktree stays in"));
    assert!(String::from_utf8_lossy(&out.stdout).contains("reuses it"));
    assert_eq!(iso.labels(), ["ct-lead", "ct-dev", "ct-status"]);
    assert!(iso.repo.join(".wt/ct-dev-2").is_dir());
    // A later add gives the same number again and reuses the worktree without creating it.
    let out = iso.crew(&["add", "ct-dev"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("new instance ct-dev-2") && !stdout.contains("worktree of"),
        "{stdout}"
    );
    assert!(iso.crew(&["close", "ct-dev-2"]).status.success());
    let out = iso.crew(&["close", "ct-dev"]);
    assert_eq!(out.status.code(), Some(1));

    // 3. Relative argv[0] and the context of an action invoked by CLI from a pane.
    let crate_dir = env!("CARGO_MANIFEST_DIR");
    let build = Command::new(env!("CARGO"))
        .args(["build", "--release", "--locked"])
        .current_dir(crate_dir)
        .status()
        .unwrap();
    assert!(build.success());
    iso.result(&["plugin", "link", crate_dir]);
    assert!(iso.dir.join("c/herdr/plugins.json").is_file());
    let ws = iso.workspace();
    let tab = iso.result(&[
        "tab",
        "create",
        "--workspace",
        &ws,
        "--cwd",
        iso.repo.to_str().unwrap(),
        "--label",
        "ct-probe",
        "--no-focus",
    ]);
    let probe = tab["root_pane"]["pane_id"].as_str().unwrap().to_string();
    sleep(Duration::from_secs(2));
    let invoke = "herdr plugin action invoke up --plugin herdr-crew";
    assert!(iso.herdr(&["pane", "run", &probe, invoke]).status.success());
    let mut log = Value::Null;
    wait_until("the action finishes", || {
        let r = iso.result(&["plugin", "log", "list", "--plugin", "herdr-crew"]);
        log = r["logs"]
            .as_array()
            .and_then(|l| l.last())
            .cloned()
            .unwrap_or(Value::Null);
        log["status"] != "running" && !log.is_null()
    });
    println!("action log: {log}");
    assert_eq!(log["exit_code"], 0, "{log}");
    assert!(
        log["stdout"].as_str().unwrap().contains("nothing to do"),
        "{log}"
    );
}
