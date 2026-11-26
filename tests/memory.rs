//! End-to-end offline CLI integration tests for `[memory] mode = "wiki"`,
//! `herdr-crew mem`, `board --once`, `mem mcp`, and `up --dry-run`.
#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

const CREW: &str = env!("CARGO_BIN_EXE_herdr-crew");
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct WikiRepo {
    root: PathBuf,
    mem_base: PathBuf,
}

impl WikiRepo {
    fn new() -> Self {
        let id = format!(
            "crew-mem-it-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let root = PathBuf::from("/tmp").join(&id);
        let mem_base = PathBuf::from("/tmp").join(format!("{id}-store"));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&mem_base);
        fs::create_dir_all(&root).unwrap();
        let repo = Self { root, mem_base };

        repo.git(&["init", "--quiet", "-b", "main"]);
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

        let tools = repo.root.join("tools");
        fs::create_dir(&tools).unwrap();
        for name in ["herdr", "claude"] {
            let path = tools.join(name);
            fs::write(&path, "#!/bin/sh\nexit 1\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        fs::create_dir_all(repo.root.join(".herdr")).unwrap();
        fs::write(
            repo.root.join(".herdr/crew.toml"),
            "\
version = 1

[workspace]
label = \"acme\"

[board]
tab = \"acme-status\"
writer = \"acme-lead\"

[memory]
mode = \"wiki\"

[[roles]]
name = \"acme-lead\"
prompt = \"You are {{NAME}} in {{REPO}}. Board: {{STATUS}} Schema: {{SCHEMA}}\"
",
        )
        .unwrap();

        repo
    }

    fn git(&self, args: &[&str]) -> String {
        success(
            Command::new("git")
                .env("HOME", &self.root)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .arg("-C")
                .arg(&self.root)
                .args(args)
                .output()
                .unwrap(),
        )
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(CREW);
        cmd.env_clear()
            .env("HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("CREW_MEMORY_DIR", &self.mem_base)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("tools").display()),
            )
            .current_dir(&self.root);
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn run_mcp(&self, extra_args: &[&str], lines: &[String]) -> Vec<Value> {
        let mut child = self
            .command()
            .args(["mem", "mcp"])
            .args(extra_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let payload = format!("{}\n", lines.join("\n"));
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        let stdout = success(output);
        stdout
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .collect()
    }
}

impl Drop for WikiRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
        let _ = fs::remove_dir_all(&self.mem_base);
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
fn wiki_mem_cli_and_board_once_work_end_to_end() {
    let repo = WikiRepo::new();

    let mem_path_str = success(repo.run(&["mem", "path"])).trim().to_string();
    let mem_dir = PathBuf::from(&mem_path_str);
    assert!(
        mem_dir.starts_with(&repo.mem_base),
        "expected memory dir under {}: {mem_path_str}",
        repo.mem_base.display()
    );
    assert!(!mem_dir.exists());

    let init_out = success(repo.run(&["mem", "init"]));
    assert!(init_out.contains("SCHEMA.md"), "{init_out}");
    assert!(mem_dir.join("now.md").is_file());
    assert!(mem_dir.join("index.md").is_file());
    assert!(mem_dir.join("log.md").is_file());
    assert!(!repo.root.join(".herdr/status.json").exists());

    let lint_out = success(repo.run(&["mem", "lint"]));
    assert!(lint_out.contains("memory lint ok"), "{lint_out}");

    let index_out = success(repo.run(&["mem", "index"]));
    assert!(index_out.contains("# acme Memory Index"), "{index_out}");

    let timeline_out = success(repo.run(&["mem", "timeline", "--limit", "5"]));
    assert!(
        timeline_out.contains("init | Initialized operational memory"),
        "{timeline_out}"
    );

    let search_out = success(repo.run(&["mem", "search", "Initialized"]));
    assert!(search_out.contains("log.md:"), "{search_out}");

    let show_out = success(repo.run(&["mem", "show", "index"]));
    assert!(show_out.contains("[[now]]"), "{show_out}");

    let board_out = success(repo.run(&["board", "--once"]));
    assert!(
        board_out.contains("[init] Initialized operational memory"),
        "{board_out}"
    );
    assert!(board_out.contains("now.md"), "{board_out}");
}

#[test]
fn mem_mcp_child_process_handles_read_only_and_rw_workflows() {
    let repo = WikiRepo::new();
    let mem_dir = PathBuf::from(success(repo.run(&["mem", "path"])).trim());

    // 1. `--read-only` on an uninitialized store exposes only 3 tools, rejects writes,
    //    and never creates `<memory_dir>` on disk.
    let ro = repo.run_mcp(
        &["--read-only"],
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}).to_string(),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}).to_string(),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}).to_string(),
            json!({
                "jsonrpc":"2.0",
                "id":3,
                "method":"tools/call",
                "params":{
                    "name":"mem_status_update",
                    "arguments":{
                        "sessions":[],
                        "owner":{"urgent":[],"beforeMain":[],"optional":[]}
                    }
                }
            })
            .to_string(),
        ],
    );
    assert_eq!(ro.len(), 3, "{ro:?}");
    let ro_tools: Vec<&str> = ro[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(ro_tools, ["mem_search", "mem_timeline", "mem_get"]);
    assert_eq!(ro[2]["result"]["isError"], true);
    assert!(
        !mem_dir.exists(),
        "read-only MCP must not create {}",
        mem_dir.display()
    );

    // 2. Default read-write MCP auto-initializes, updates now.md, upserts a page, and reads timeline.
    let rw = repo.run_mcp(
        &[],
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"initialize"}).to_string(),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}).to_string(),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}).to_string(),
            json!({
                "jsonrpc":"2.0",
                "id":3,
                "method":"tools/call",
                "params":{
                    "name":"mem_status_update",
                    "arguments":{
                        "sessions":[{
                            "role":"acme-lead",
                            "state":"working",
                            "now":"Shipping PR #36: operational memory",
                            "next":["Verify CI"]
                        }],
                        "owner":{"urgent":[],"beforeMain":["Merge PR #36"],"optional":[]}
                    }
                }
            })
            .to_string(),
            json!({
                "jsonrpc":"2.0",
                "id":4,
                "method":"tools/call",
                "params":{
                    "name":"mem_upsert",
                    "arguments":{
                        "page":"tasks/pr-36",
                        "content":"# PR 36\n\nOperational memory integration test.\n",
                        "summary":"Operational memory PR",
                        "log_type":"task",
                        "log_title":"Validated MCP end-to-end"
                    }
                }
            })
            .to_string(),
            json!({
                "jsonrpc":"2.0",
                "id":5,
                "method":"tools/call",
                "params":{"name":"mem_timeline","arguments":{"limit":2}}
            })
            .to_string(),
        ],
    );
    assert_eq!(rw.len(), 5, "{rw:?}");
    assert_eq!(rw[1]["result"]["tools"].as_array().unwrap().len(), 5);
    assert!(rw[2]["result"].get("isError").is_none(), "{:?}", rw[2]);
    assert!(rw[3]["result"].get("isError").is_none(), "{:?}", rw[3]);
    assert!(
        rw[3]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Lint ok"),
        "{:?}",
        rw[3]
    );
    let timeline = rw[4]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        timeline.contains("task | Validated MCP end-to-end"),
        "{timeline}"
    );
    let first_line = timeline.lines().next().unwrap();
    assert!(
        first_line.contains("Validated MCP end-to-end"),
        "expected newest prepended entry first: {timeline}"
    );

    assert!(success(repo.run(&["mem", "lint"])).contains("memory lint ok"));
    let board_out = success(repo.run(&["board", "--once"]));
    assert!(
        board_out.contains("Shipping PR #36: operational memory"),
        "{board_out}"
    );
    assert!(board_out.contains("Merge PR #36"), "{board_out}");
}

#[test]
fn wiki_up_dry_run_skips_repo_status_and_adds_memory_dir_without_writing() {
    let repo = WikiRepo::new();
    let mem_dir = PathBuf::from(success(repo.run(&["mem", "path"])).trim());
    assert!(!mem_dir.exists());

    let plan = success(repo.run(&["up", "--dry-run"]));
    assert!(
        plan.contains("--add-dir"),
        "expected dry-run plan to include --add-dir: {plan}"
    );
    assert!(
        plan.contains(&mem_dir.display().to_string()),
        "expected dry-run plan to reference {}: {plan}",
        mem_dir.display()
    );
    assert!(
        plan.contains(&mem_dir.join("now.md").display().to_string()),
        "expected board command to target now.md: {plan}"
    );
    assert!(
        !plan.contains(".herdr/status.json"),
        "wiki dry-run should not plan .herdr/status.json: {plan}"
    );
    assert!(
        !plan.contains(".herdr/status.schema.json"),
        "wiki dry-run should not plan .herdr/status.schema.json: {plan}"
    );
    assert!(
        !mem_dir.exists(),
        "up --dry-run must not create {}",
        mem_dir.display()
    );
    assert!(!repo.root.join(".herdr/status.json").exists());
    assert!(!Path::new(&repo.root).join(".herdr/prompts").exists());
}
