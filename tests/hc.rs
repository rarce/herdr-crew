#![cfg(unix)]
//! `up`, `send` and `close` against herdr-core, through `tests/support/hc.py`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

const CREW: &str = env!("CARGO_BIN_EXE_herdr-crew");
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    state: PathBuf,
}

const CONFIG: &str = r#"version = 1
start_message = "Confirm your role"
common_prompt = "Board {{STATUS}}."
[workspace]
label = "crew"
[board]
tab = "status"
writer = "lead"
[[roles]]
name = "lead"
prompt = "Lead {{NAME}}."
[[roles]]
name = "dev"
prompt = "Developer {{NAME}}."
"#;

impl Fixture {
    fn new(config: &str) -> Self {
        let root = PathBuf::from("/tmp").join(format!(
            "crew-hc-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("repo/.herdr")).unwrap();
        fs::create_dir(root.join("state")).unwrap();
        fs::write(root.join("state/hc-online"), "").unwrap();
        let hc = root.join("hc");
        fs::write(&hc, include_str!("support/hc.py")).unwrap();
        fs::set_permissions(&hc, fs::Permissions::from_mode(0o755)).unwrap();
        let f = Fixture {
            repo: root.join("repo").canonicalize().unwrap(),
            state: root.join("state"),
            root,
        };
        f.git(&["init", "--quiet", "-b", "main"]);
        fs::write(
            f.repo.join(".gitignore"),
            "/.herdr/prompts/\n/.herdr/status.json\n/.herdr/status.schema.json\n",
        )
        .unwrap();
        fs::write(f.repo.join(".herdr/crew.toml"), config).unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "--quiet", "-m", "Initial"]);
        f
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Crew test")
            .env("GIT_AUTHOR_EMAIL", "crew@example.invalid")
            .env("GIT_COMMITTER_NAME", "Crew test")
            .env("GIT_COMMITTER_EMAIL", "crew@example.invalid")
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    }

    fn crew(&self, args: &[&str]) -> Command {
        let mut c = Command::new(CREW);
        c.env_clear()
            .env("HOME", &self.root)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.root.join("tools").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("SHELL", "/bin/sh")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("CREW_HOST", "herdr-core")
            .env("CREW_HC_BIN", self.root.join("hc"))
            .env("CREW_HERDR_APP_BIN", "/usr/bin/false")
            .env("CREW_TEST_STATE", &self.state)
            .current_dir(&self.repo)
            .args(args);
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.crew(args).output().unwrap()
    }

    fn terminals(&self) -> Vec<Value> {
        let state: Value =
            serde_json::from_slice(&fs::read(self.state.join("hc.json")).unwrap()).unwrap();
        state["terminals"].as_array().unwrap().clone()
    }

    fn tab(&self, label: &str) -> Value {
        self.terminals()
            .into_iter()
            .find(|t| t["labels"]["crew.tab"] == label && t["state"] == "live")
            .unwrap_or_else(|| panic!("no live terminal for {label}: {:#?}", self.terminals()))
    }

    fn calls(&self) -> Vec<Vec<String>> {
        fs::read_to_string(self.state.join("hc-calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn ok(out: &Output) -> String {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    text
}

#[test]
fn up_creates_one_labeled_terminal_per_tab_in_the_project_space() {
    let f = Fixture::new(CONFIG);
    let text = ok(&f.run(&["up"]));
    assert!(text.contains("open the Herdr app"), "{text}");
    let terminals = f.terminals();
    assert_eq!(terminals.len(), 3, "{terminals:#?}");
    let space = terminals[0]["labels"]["herdr.space"].as_str().unwrap();
    assert!(space.starts_with("crew-"), "{space}");
    for t in &terminals {
        assert_eq!(t["labels"]["herdr.space"], space);
        assert_eq!(t["keep"], true);
        assert_eq!(t["parent"], Value::Null, "crew terminals have no parent");
        assert_eq!(t["cwd"], f.repo.to_str().unwrap());
    }
    let lead = f.tab("lead");
    assert_eq!(lead["labels"]["crew.agent"], "lead");
    assert_eq!(lead["labels"]["crew.kind"], "claude");
    let typed = lead["typed"][0].as_str().unwrap();
    let session = lead["labels"]["crew.session"].as_str().unwrap();
    assert_eq!(session.len(), 36, "{session}");
    assert!(
        typed.starts_with(&format!(
            "claude --session-id {session} -n lead --append-system-prompt-file "
        )) && typed.ends_with("'Confirm your role'"),
        "{typed}"
    );
    assert_eq!(lead["status"]["app"], "claude-code");
    let dev = f.tab("dev");
    assert_eq!(dev["labels"]["crew.agent"], "dev");
    let board = f.tab("status");
    assert!(
        board["typed"][0].as_str().unwrap().contains(" board"),
        "{board}"
    );
    assert!(board["labels"].get("crew.agent").is_none());
}

#[test]
fn a_second_up_changes_nothing() {
    let f = Fixture::new(CONFIG);
    ok(&f.run(&["up"]));
    let before = f.calls().len();
    let text = ok(&f.run(&["up"]));
    assert!(text.contains("nothing to do"), "{text}");
    let mutating: Vec<_> = f.calls()[before..]
        .iter()
        .filter(|c| !matches!(c[0].as_str(), "ls" | "get"))
        .cloned()
        .collect();
    assert!(mutating.is_empty(), "{mutating:?}");
    assert_eq!(f.terminals().len(), 3);
}

#[test]
fn dry_run_without_a_core_plans_against_an_empty_host() {
    let f = Fixture::new(CONFIG);
    fs::remove_file(f.state.join("hc-online")).unwrap();
    let text = ok(&f.run(&["up", "--dry-run"]));
    assert!(text.contains("no herdr server"), "{text}");
}

#[test]
fn send_types_a_framed_message_into_the_receiver() {
    let f = Fixture::new(CONFIG);
    ok(&f.run(&["up"]));
    let lead = f.tab("lead");
    let mut send = f.crew(&["send", "dev"]);
    send.env("HC_TERMINAL_ID", lead["address"].as_str().unwrap())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = send.spawn().unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"please review x.rs")
        .unwrap();
    ok(&child.wait_with_output().unwrap());
    let dev = f.tab("dev");
    let typed = dev["typed"].as_array().unwrap();
    let message = typed.last().unwrap().as_str().unwrap();
    assert!(message.contains("please review x.rs"), "{message}");
    assert!(message.contains("lead (claude)"), "{message}");
}

#[test]
fn send_refuses_a_blocked_receiver() {
    let f = Fixture::new(CONFIG);
    ok(&f.run(&["up"]));
    let mut state: Value =
        serde_json::from_slice(&fs::read(f.state.join("hc.json")).unwrap()).unwrap();
    for t in state["terminals"].as_array_mut().unwrap() {
        if t["labels"]["crew.tab"] == "dev" {
            t["status"]["state"] = "blocked".into();
        }
    }
    fs::write(f.state.join("hc.json"), state.to_string()).unwrap();
    let mut send = f.crew(&["send", "dev"]);
    send.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = send.spawn().unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(b"hello").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("approval or question dialog"),
        "{out:?}"
    );
}

#[test]
fn an_unreachable_core_is_a_sandbox_refusal_for_send() {
    let f = Fixture::new(CONFIG);
    fs::remove_file(f.state.join("hc-online")).unwrap();
    let mut send = f.crew(&["send", "dev"]);
    send.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = send.spawn().unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(b"hello").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(77), "{out:?}");
}

#[test]
fn codex_roles_are_refused_before_anything_is_created() {
    let f = Fixture::new(&CONFIG.replace("version = 1", "version = 1\nkind = \"codex\""));
    let out = f.run(&["up"]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("need the Herdr TUI"), "{text}");
    assert!(
        !f.calls().iter().any(|c| c[0] == "run"),
        "no terminal was created: {:?}",
        f.calls()
    );
}

#[test]
fn add_and_close_an_extra_instance() {
    let f = Fixture::new(&CONFIG.replace(
        "prompt = \"Developer {{NAME}}.\"",
        "prompt = \"Developer {{NAME}}.\"\nextra = true",
    ));
    ok(&f.run(&["up"]));
    let text = ok(&f.run(&["add", "dev"]));
    assert!(text.contains("new instance dev-2"), "{text}");
    let extra = f.tab("dev-2");
    assert_eq!(extra["labels"]["crew.agent"], "dev-2");
    let text = ok(&f.run(&["close", "dev-2"]));
    assert!(text.contains("dev-2 closed"), "{text}");
    assert!(
        !f.terminals()
            .iter()
            .any(|t| t["labels"]["crew.tab"] == "dev-2"),
        "the extra terminal is gone"
    );
    let base = f.run(&["close", "dev"]);
    assert!(
        !base.status.success(),
        "a base role is never closed: {base:?}"
    );
    assert_eq!(f.tab("dev")["state"], "live");
}

/// What a core restart or a crash leaves: the kept shell back in front, no status.
fn lose_agent(f: &Fixture, tab: &str) {
    let mut state: Value =
        serde_json::from_slice(&fs::read(f.state.join("hc.json")).unwrap()).unwrap();
    for t in state["terminals"].as_array_mut().unwrap() {
        if t["labels"]["crew.tab"] == tab {
            t["foreground"]["pid"] = t["pid"].clone();
            t["status"] = Value::Null;
        }
    }
    fs::write(f.state.join("hc.json"), state.to_string()).unwrap();
}

#[test]
fn up_resumes_a_lost_agent_in_its_kept_shell() {
    let f = Fixture::new(CONFIG);
    ok(&f.run(&["up"]));
    let before = f.tab("lead");
    let session = before["labels"]["crew.session"]
        .as_str()
        .unwrap()
        .to_string();
    lose_agent(&f, "lead");
    let dry = ok(&f.run(&["up", "--dry-run"]));
    assert!(
        dry.contains(&format!("resume (session {session})")),
        "{dry}"
    );
    let text = ok(&f.run(&["up"]));
    assert!(text.contains("lead resumed in"), "{text}");
    assert!(text.contains("resumed its conversation"), "{text}");
    let lead = f.tab("lead");
    assert_eq!(lead["address"], before["address"], "same terminal");
    let typed = lead["typed"].as_array().unwrap();
    assert_eq!(typed.len(), 2, "{typed:?}");
    let resume = typed[1].as_str().unwrap();
    assert!(
        resume.starts_with(&format!(
            "claude --resume {session} -n lead --append-system-prompt-file "
        )),
        "{resume}"
    );
    assert!(
        !resume.contains("Confirm your role"),
        "no new first message: {resume}"
    );
    assert_eq!(lead["labels"]["crew.session"], session.as_str());
    // Resumed and running again: nothing more to do.
    assert!(ok(&f.run(&["up"])).contains("nothing to do"));
}

#[test]
fn pi_roles_resume_with_their_session_id_only() {
    let f = Fixture::new(&CONFIG.replace("version = 1", "version = 1\nkind = \"pi\""));
    // pi's preflight needs a recent pi and the crew's extension.
    let tools = f.root.join("tools");
    fs::create_dir_all(&tools).unwrap();
    fs::write(tools.join("pi"), "#!/bin/sh\necho 99.0.0\n").unwrap();
    fs::set_permissions(tools.join("pi"), fs::Permissions::from_mode(0o755)).unwrap();
    ok(&f.run(&["pi-install"]));
    ok(&f.run(&["up"]));
    let start = f.tab("lead")["typed"][0].as_str().unwrap().to_string();
    assert!(start.contains("--herdr-crew-prompt"), "{start}");
    let session = f.tab("lead")["labels"]["crew.session"]
        .as_str()
        .unwrap()
        .to_string();
    lose_agent(&f, "lead");
    ok(&f.run(&["up"]));
    let typed = f.tab("lead")["typed"].as_array().unwrap().clone();
    let resume = typed[1].as_str().unwrap();
    assert!(
        resume.starts_with(&format!("pi --session-id {session}")),
        "{resume}"
    );
    assert!(!resume.contains("--herdr-crew-prompt"), "{resume}");
}
