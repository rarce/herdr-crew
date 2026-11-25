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

/// Runs `herdr-crew <hook>` in terminal `address` with `input` on stdin; returns its JSON.
fn hook(f: &Fixture, hook: &str, address: &str, input: &str, env: &[(&str, &str)]) -> Value {
    use std::io::Write;
    use std::process::Stdio;
    let mut command = f.crew(&[hook]);
    command
        .env("HC_TERMINAL_ID", address)
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{out:?}");
    serde_json::from_slice(&out.stdout).unwrap()
}

/// The public launcher on PATH, which the Codex and agy hooks run as `herdr-crew`.
fn launcher(f: &Fixture) {
    let tools = f.root.join("tools");
    fs::create_dir_all(&tools).unwrap();
    fs::write(
        tools.join("herdr-crew"),
        format!(
            "#!/bin/sh\nif [ \"${{1-}}\" = --launcher-id ]; then echo 'herdr-crew public launcher format 1 (rarce/herdr-crew)'; else exec {CREW} \"$@\"; fi\n"
        ),
    )
    .unwrap();
    fs::set_permissions(tools.join("herdr-crew"), fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn codex_roles_bind_to_their_terminal_and_resume_through_the_crew() {
    let f = Fixture::new(&CONFIG.replace(
        "[workspace]",
        "kind = \"codex\"\n[codex]\nmodel = \"original-model\"\n[workspace]",
    ));
    launcher(&f);
    let tools = f.root.join("tools");
    fs::write(tools.join("codex"), include_str!("support/codex.py")).unwrap();
    fs::set_permissions(tools.join("codex"), fs::Permissions::from_mode(0o755)).unwrap();
    let home = f.root.join(".codex");
    let codex_home = home.to_str().unwrap();
    let run = |args: &[&str]| f.crew(args).env("CODEX_HOME", &home).output().unwrap();
    ok(&run(&["codex-install"]));
    ok(&run(&["check"]));
    ok(&run(&["up"]));
    let lead = f.tab("lead");
    let start = lead["typed"][0].as_str().unwrap();
    assert!(start.starts_with("codex "), "{start}");
    assert!(start.contains("features.hooks=true"), "{start}");
    assert!(
        lead["env"]
            .as_array()
            .unwrap()
            .contains(&Value::from(format!("CODEX_HOME={codex_home}"))),
        "{lead:#?}"
    );

    // Codex's SessionStart hook in lead's terminal gets lead's context and records its binding.
    let address = lead["address"].as_str().unwrap().to_string();
    let event = |source: &str, id: &str| {
        serde_json::json!({"hook_event_name": "SessionStart", "session_id": id,
            "source": source, "cwd": f.repo})
        .to_string()
    };
    let home_env = [("CODEX_HOME", codex_home)];
    let first = hook(
        &f,
        "codex-hook",
        &address,
        &event("startup", "lead-id"),
        &home_env,
    );
    let context = first["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_default();
    assert!(context.contains("Lead lead"), "{first}");
    let token = f.tab("lead")["labels"]["crew.session"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(token.len(), 32, "{token}");
    assert!(
        lead["labels"]["crew.core"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "{lead:#?}"
    );
    // A core started from a TUI pane passes the pane's variables on, complete or not: the
    // core terminal still finds its binding, with or without the explicit one.
    let tui = [
        ("CODEX_HOME", codex_home),
        ("HERDR_ENV", "1"),
        ("HERDR_PANE_ID", "w9:p9"),
    ];
    let partial = hook(
        &f,
        "codex-hook",
        &address,
        &event("compact", "lead-id"),
        &tui,
    );
    assert_eq!(partial, first, "{partial}");
    let socket = f.root.join("gone.sock");
    let full = [
        ("CODEX_HOME", codex_home),
        ("HERDR_ENV", "1"),
        ("HERDR_PANE_ID", "w9:p9"),
        ("HERDR_TAB_ID", "w9:t9"),
        ("HERDR_WORKSPACE_ID", "w9"),
        ("HERDR_SOCKET_PATH", socket.to_str().unwrap()),
        ("CREW_CODEX_BINDING", token.as_str()),
    ];
    let explicit = hook(
        &f,
        "codex-hook",
        &address,
        &event("compact", "lead-id"),
        &full,
    );
    assert_eq!(explicit, first, "{explicit}");
    // dev's terminal does not hold lead's binding; an ordinary Codex there gets nothing.
    let dev = f.tab("dev")["address"].as_str().unwrap().to_string();
    let other = hook(
        &f,
        "codex-hook",
        &dev,
        &event("startup", "lead-id"),
        &home_env,
    );
    assert!(
        other["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .is_none_or(|c| !c.contains("Lead lead")),
        "{other}"
    );

    lose_agent(&f, "lead");
    let dry = ok(&run(&["up", "--dry-run"]));
    assert!(dry.contains(&format!("resume (session {token})")), "{dry}");
    let text = ok(&run(&["up"]));
    assert!(text.contains("lead resumed in"), "{text}");
    let typed = f.tab("lead")["typed"].as_array().unwrap().clone();
    assert_eq!(
        typed[1],
        format!("herdr-crew codex-resume {token}").as_str()
    );

    // `codex-resume` in lead's terminal resumes the saved conversation with lead's options.
    let out = f
        .crew(&["codex-resume", &token])
        .env("CODEX_HOME", &home)
        .env("HC_TERMINAL_ID", &address)
        .output()
        .unwrap();
    ok(&out);
    let resumed: Value =
        serde_json::from_slice(&fs::read(f.state.join("codex-resume.json")).unwrap()).unwrap();
    assert_eq!(resumed["args"][0], "resume");
    assert_eq!(
        resumed["args"].as_array().unwrap().last().unwrap(),
        "lead-id"
    );
    assert!(
        resumed["args"]
            .as_array()
            .unwrap()
            .contains(&Value::from("model=\"original-model\"")),
        "{resumed}"
    );
    assert_eq!(resumed["binding"], token.as_str());
    // In another role's terminal it refuses.
    let out = f
        .crew(&["codex-resume", &token])
        .env("CODEX_HOME", &home)
        .env("HC_TERMINAL_ID", &dev)
        .output()
        .unwrap();
    assert!(!out.status.success(), "{out:?}");

    // A resume that exits at once (no saved conversation) warns, forgets the session and
    // lets `up` go on; the next `up` does not try it again.
    lose_agent(&f, "lead");
    let text = ok(&f
        .crew(&["up"])
        .env("CODEX_HOME", &home)
        .env("CREW_TEST_HC_LOST", &token)
        .output()
        .unwrap());
    assert!(text.contains("could not resume session"), "{text}");
    assert!(text.contains("codex-resume"), "{text}");
    assert!(f.tab("lead")["labels"].get("crew.session").is_none());
    let text = ok(&run(&["up"]));
    assert!(!text.contains("lead resumed"), "{text}");

    // Typed delivery: herdr-core has no Codex queue.
    use std::io::Write;
    let mut send = f
        .crew(&["send", "dev"])
        .env("CODEX_HOME", &home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    send.stdin.take().unwrap().write_all(b"hello dev").unwrap();
    let text = ok(&send.wait_with_output().unwrap());
    assert!(text.contains("sent to dev"), "{text}");
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
fn pi_roles_resume_with_their_session_id() {
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
    // The extension ignores the prompt flags in a session that holds the role, and gives the
    // role to one pi never saved.
    assert!(resume.contains("--herdr-crew-prompt"), "{resume}");
    assert!(!resume.contains("Confirm your role"), "{resume}");
}

#[test]
fn a_lost_conversation_that_was_never_saved_starts_again() {
    let f = Fixture::new(CONFIG);
    ok(&f.run(&["up"]));
    let session = f.tab("lead")["labels"]["crew.session"]
        .as_str()
        .unwrap()
        .to_string();
    lose_agent(&f, "lead");
    // Claude died before saving its first message: `claude --resume` finds nothing and exits.
    let text = ok(&f
        .crew(&["up"])
        .env("CREW_TEST_HC_LOST", &session)
        .output()
        .unwrap());
    assert!(text.contains("held no conversation to resume"), "{text}");
    let lead = f.tab("lead");
    let typed = lead["typed"].as_array().unwrap();
    assert_eq!(typed.len(), 3, "{typed:?}");
    assert!(
        typed[1]
            .as_str()
            .unwrap()
            .starts_with(&format!("claude --resume {session} ")),
        "{typed:?}"
    );
    assert!(
        typed[2]
            .as_str()
            .unwrap()
            .starts_with(&format!("claude --session-id {session} -n lead ")),
        "{typed:?}"
    );
    assert_eq!(lead["labels"]["crew.session"], session.as_str());
    assert!(ok(&f.run(&["up"])).contains("nothing to do"));
}

#[test]
fn a_failed_read_stops_a_real_up() {
    let f = Fixture::new(CONFIG);
    ok(&f.run(&["up"]));
    let before = f.terminals().len();
    // `hc` fails while reading the crew: planning over an empty core would start every role
    // a second time.
    fs::write(f.state.join("hc-fail"), "get").unwrap();
    let out = f.run(&["up"]);
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(
        text.contains("could not read the crew from herdr-core"),
        "{text}"
    );
    assert_eq!(f.terminals().len(), before, "nothing created");
    assert!(ok(&f.run(&["up"])).contains("nothing to do"));
}

#[test]
fn app_install_writes_only_its_own_mod() {
    let f = Fixture::new(CONFIG);
    let home = f.root.join("app-home");
    let install = |command: &str| {
        f.crew(&[command])
            .env("HERDR_APP_HOME", &home)
            .output()
            .unwrap()
    };
    ok(&install("app-install"));
    let dir = home.join("mods/herdr-crew");
    let main = fs::read_to_string(dir.join("main.js")).unwrap();
    assert!(
        main.contains(&format!(
            "const CREW = {};",
            serde_json::to_string(CREW).unwrap()
        )),
        "the mod runs this binary"
    );
    assert!(dir.join("mod.json").is_file());
    // Again: an update in place.
    ok(&install("app-install"));
    ok(&install("app-uninstall"));
    assert!(!dir.exists());
    // Someone else's mod of that name is left alone.
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("main.js"), "export default 1;").unwrap();
    assert!(!install("app-install").status.success());
    assert!(!install("app-uninstall").status.success());
    assert_eq!(
        fs::read_to_string(dir.join("main.js")).unwrap(),
        "export default 1;"
    );
}

fn agy_fixture(config: &str) -> Fixture {
    let f = Fixture::new(&config.replace("version = 1", "version = 1\nkind = \"agy\""));
    // agy's preflight needs agy 1.5+, the public launcher on PATH and the crew hook.
    let tools = f.root.join("tools");
    fs::create_dir_all(&tools).unwrap();
    fs::write(tools.join("agy"), "#!/bin/sh\necho 1.5.15\n").unwrap();
    fs::set_permissions(tools.join("agy"), fs::Permissions::from_mode(0o755)).unwrap();
    let settings = f.root.join(".gemini/antigravity-cli/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(
        settings,
        serde_json::json!({"trustedWorkspaces": [f.repo]}).to_string(),
    )
    .unwrap();
    launcher(&f);
    ok(&f.run(&["agy-install"]));
    f
}

#[test]
fn agy_startup_dialog_preserves_the_role_prompt_until_session_start() {
    let f = agy_fixture(CONFIG);
    let up = f
        .crew(&["up", "--no-attach"])
        .env("CREW_TEST_HC_READY", "blocked")
        .output()
        .unwrap();
    assert!(ok(&up).contains("waiting for an answer at startup"));
    let lead = f.tab("lead");
    assert_eq!(lead["status"]["state"], "blocked");
    assert!(
        lead["typed"][0]
            .as_str()
            .unwrap()
            .contains("--prompt-interactive 'Confirm your role'")
    );

    let address = lead["address"].as_str().unwrap();
    let input = serde_json::json!({"conversationId": "after-dialog", "cwd": f.repo}).to_string();
    let first = hook(&f, "agy-hook", address, &input, &[]);
    assert_eq!(
        first["injectSteps"][0]["systemMessage"]["systemMessage"],
        fs::read_to_string(f.repo.join(".herdr/prompts/lead.txt")).unwrap()
    );
    assert_eq!(f.tab("lead")["labels"]["crew.session"], "after-dialog");
    assert_eq!(
        hook(&f, "agy-hook", address, &input, &[]),
        serde_json::json!({})
    );
}

#[test]
fn agy_roles_get_their_prompt_and_resume_their_conversation() {
    let f = agy_fixture(&CONFIG.replace("start_message = \"Confirm your role\"", ""));
    ok(&f.run(&["check"]));
    ok(&f.run(&["up"]));
    let lead = f.tab("lead");
    let start = lead["typed"][0].as_str().unwrap().to_string();
    assert_eq!(start, "agy");
    assert!(lead["labels"].get("crew.session").is_none(), "{lead:#?}");

    // agy's SessionStart hook, run in lead's terminal, takes the role prompt once and records
    // the conversation agy started.
    let hook = |address: &str, conversation: &str| -> Value {
        let input = format!(
            r#"{{"conversationId":"{conversation}","cwd":"{}"}}"#,
            f.repo.display()
        );
        hook(&f, "agy-hook", address, &input, &[])
    };
    let address = lead["address"].as_str().unwrap().to_string();
    let first = hook(&address, "conv-1");
    let injected = first["injectSteps"][0]["systemMessage"]["systemMessage"]
        .as_str()
        .unwrap_or_default();
    assert_eq!(
        injected,
        fs::read_to_string(f.repo.join(".herdr/prompts/lead.txt")).unwrap()
    );
    assert_eq!(f.tab("lead")["labels"]["crew.session"], "conv-1");
    // A second session in that terminal (a subagent) gets nothing and keeps the record.
    assert_eq!(hook(&address, "sub-1"), serde_json::json!({}));
    assert_eq!(f.tab("lead")["labels"]["crew.session"], "conv-1");

    lose_agent(&f, "lead");
    let dry = ok(&f.run(&["up", "--dry-run"]));
    assert!(dry.contains("resume (session conv-1)"), "{dry}");
    let text = ok(&f.run(&["up"]));
    assert!(text.contains("lead resumed in"), "{text}");
    let typed = f.tab("lead")["typed"].as_array().unwrap().clone();
    let resume = typed[1].as_str().unwrap();
    assert!(resume.starts_with("agy --conversation conv-1"), "{resume}");
    assert!(!resume.contains("--prompt-interactive"), "{resume}");
    assert!(ok(&f.run(&["up"])).contains("nothing to do"));
}
