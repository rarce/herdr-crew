#!/usr/bin/env python3
"""Opt-in live-model acceptance test using the user's native Codex ChatGPT login.

Runs one disposable Herdr server/checkout. It never copies auth, installs hooks,
or bypasses trust. Codex records normal per-folder trust for the generated checkout
and keeps its normal session records; user-wide agent settings are unchanged.
the database, logs, test files and evidence live in the printed temporary directory.
Unlike the offline test, this sends real model requests and consumes plan usage.
"""

import argparse
import json
import os
import pathlib
import secrets
import shlex
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
import tomllib

sys.dont_write_bytecode = True
from codex_tree import BOOTSTRAP, prepare


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--crew-bin", type=pathlib.Path, required=True)
    args = parser.parse_args()
    crew = str(args.crew_bin.resolve())
    herdr, codex = shutil.which("herdr"), shutil.which("codex")
    assert herdr and codex, "Herdr and Codex must be installed"
    root = pathlib.Path(tempfile.mkdtemp(prefix="cl", dir="/tmp")).resolve()
    repo, tools, database = root / "repo", root / "bin", root / "database"
    for path in (repo, tools, database):
        path.mkdir()
    home = pathlib.Path(os.environ.get("CODEX_HOME", str(pathlib.Path.home() / ".codex")))
    session = "ctlive" + str(os.getpid())
    config = root / "c/herdr"
    config.mkdir(parents=True)
    config.joinpath("config.toml").write_text('onboarding = false\n[terminal]\ndefault_shell = "/bin/sh"\nshell_mode = "non_login"\n[update]\nversion_check = false\nmanifest_check = false\n')
    socket = config / "sessions" / session / "herdr.sock"
    assert len(str(socket).encode()) <= 103
    for name, target in [("herdr", herdr), ("python3", sys.executable)]:
        tools.joinpath(name).symlink_to(target)
    if shutil.which("node"):
        tools.joinpath("node").symlink_to(shutil.which("node"))
    # Invocation-only overrides, shared by start, queue and configured resume.
    overrides = ["-c", "features.multi_agent_v2=true", "-c", "agents.max_concurrent_threads_per_session=3",
                 "-c", "sqlite_home=" + json.dumps(str(database)),
                 "-c", "log_dir=" + json.dumps(str(root / "logs"))]
    # Resume has its own -c parser. Overrides before the subcommand can be lost
    # when the crew helper supplies resume-level -c flags (such as hooks=true).
    options = shlex.join(overrides)
    executable = shlex.quote(codex)
    tools.joinpath("codex").write_text(
        '#!/bin/sh\ncase "${1-}" in\n'
        '  resume|queue)\n    command="$1"\n    shift\n'
        '    exec ' + executable + ' "$command" ' + options + ' "$@" ;;\n'
        '  app-server) exec ' + executable + ' "$@" ;;\n'
        '  *) exec ' + executable + ' ' + options + ' "$@" ;;\nesac\n')
    tools.joinpath("codex").chmod(0o755)
    tools.joinpath("herdr-crew").write_text("#!/bin/sh\nif [ \"${1-}\" = --launcher-id ]; then echo 'herdr-crew public launcher format 1 (rarce/herdr-crew)'; else exec \"$CREW_TEST_BIN\" \"$@\"; fi\n")
    tools.joinpath("herdr-crew").chmod(0o755)
    env = {key: os.environ[key] for key in ("USER", "LOGNAME", "LANG", "HOME") if key in os.environ}
    env.update(PATH=str(tools) + ":/usr/bin:/bin", SHELL="/bin/sh", TERM="xterm-256color",
               XDG_CONFIG_HOME=str(root / "c"), XDG_STATE_HOME=str(root / "s"),
               XDG_DATA_HOME=str(root / "d"), XDG_CACHE_HOME=str(root / "cache"),
               TMPDIR=str(root), HERDR_SOCKET_PATH=str(socket), CODEX_HOME=str(home), CREW_TEST_BIN=crew)
    subprocess.run(["git", "init", "-q", "-b", "main", str(repo)], env=env, check=True)
    fixture = pathlib.Path(__file__).resolve().parents[1] / "tests/fixtures/crew-codex-tree.toml"
    with fixture.open("rb") as file:
        source = tomllib.load(file)
    tokens = {name: "MEMORY_" + secrets.token_hex(6) for name in ("tree_reviewer", "tree_coder")}
    for role in source["roles"]:
        name = role["name"].replace("-", "_")
        if name in tokens:
            role["prompt"] += " Remember your original test token " + tokens[name] + ". On initialization, confirm your role and token in your final response, then wait for work."
    generated, _ = prepare(source, repo)
    initial = "Create both configured native subagents. Wait for both to confirm their roles and tokens by final response. Then answer LIVE_BOOTSTRAP_OK and wait."
    assert len(initial.encode()) <= 200
    generated = generated.replace("start_message = " + json.dumps(BOOTSTRAP), "start_message = " + json.dumps(initial), 1)
    repo.joinpath(".herdr").mkdir()
    repo.joinpath(".herdr/crew.toml").write_text(generated)
    repo.joinpath("math_utils.py").write_text("def clamp(value, low, high):\n    return min(max(value, low), high - 1)\n")
    repo.joinpath("test_math_utils.py").write_text("import unittest\nfrom math_utils import clamp\n\nclass ClampTests(unittest.TestCase):\n    def test_inclusive_upper_bound(self):\n        self.assertEqual(clamp(10, 0, 10), 10)\n    def test_lower_bound(self):\n        self.assertEqual(clamp(-1, 0, 10), 0)\n    def test_middle(self):\n        self.assertEqual(clamp(5, 0, 10), 5)\n")
    server = None
    print("Live model experiment:", root, flush=True)

    def call(*arguments):
        return subprocess.run([herdr, "--session", session, *arguments], env=env, cwd=repo,
                              capture_output=True, text=True, timeout=20)

    def result(*arguments):
        process = call(*arguments)
        assert process.returncode == 0, process.stdout + process.stderr
        if not process.stdout.strip():
            return {}
        value = json.loads(process.stdout)
        return value.get("result", value)

    def run_crew(*arguments):
        process = subprocess.run([crew, *arguments], env=env, cwd=repo, capture_output=True,
                                 text=True, timeout=120)
        assert process.returncode == 0, process.stdout + process.stderr
        return process.stdout

    def wait_for(label, condition, seconds=480):
        deadline = time.monotonic() + seconds
        next_update = time.monotonic() + 30
        while time.monotonic() < deadline:
            if condition():
                return
            if time.monotonic() >= next_update:
                print("Waiting:", label, flush=True)
                next_update = time.monotonic() + 30
            time.sleep(0.25)
        raise RuntimeError("timed out: " + label)

    def start():
        nonlocal server
        server = subprocess.Popen([herdr, "--session", session, "server"], env=env, cwd=repo,
                                  stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                  start_new_session=True)
        wait_for("isolated Herdr server", lambda: call("workspace", "list").returncode == 0, seconds=20)
        assert result("status", "server", "--json")["socket"] == str(socket)

    def stop():
        nonlocal server
        if server is not None:
            call("session", "stop", session)
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.terminate()
                server.wait(timeout=5)
            server = None

    def panes():
        workspace = next(item for item in result("workspace", "list")["workspaces"]
                         if item["label"] == "tree-crew")["workspace_id"]
        tabs = result("tab", "list", "--workspace", workspace)["tabs"]
        labels = {item["tab_id"]: item["label"] for item in tabs}
        return {labels[item["tab_id"]]: item for item in result("pane", "list", "--workspace", workspace)["panes"]}

    def threads():
        state = database / "state_5.sqlite"
        if not state.exists():
            return []
        with sqlite3.connect("file:" + str(state) + "?mode=ro", uri=True) as connection:
            connection.row_factory = sqlite3.Row
            return [dict(row) for row in connection.execute(
                "select id,rollout_path,agent_path,model,model_provider,cwd from threads where cwd=?", (str(repo),))]

    def records(thread):
        path = pathlib.Path(thread["rollout_path"])
        if not path.exists():
            return []
        values = []
        for line in path.read_text().splitlines():
            try:
                values.append(json.loads(line))
            except json.JSONDecodeError:
                pass  # The native writer can still be appending its final line.
        return values

    def root_thread():
        return next((thread for thread in threads() if not thread["agent_path"]), None)

    def idle():
        pane = panes()["tree-lead"]["pane_id"]
        return result("agent", "get", pane)["agent"]["agent_status"] in ("idle", "done")

    def final_texts(thread):
        return ["\n".join(part.get("text", "") for part in item["payload"].get("content", []))
                for item in records(thread) if item.get("type") == "response_item"
                and item.get("payload", {}).get("role") == "assistant"
                and item["payload"].get("phase") in (None, "final_answer")
                and item["payload"].get("type") == "message"]

    def completed(marker):
        thread = root_thread()
        return thread and any(marker in text for text in final_texts(thread)) and idle()

    def task(text, marker):
        baseline = len(final_texts(root_thread()))
        result("agent", "prompt", panes()["tree-lead"]["pane_id"], text)

        def settled():
            if completed(marker):
                return True
            replies = final_texts(root_thread())
            if len(replies) > baseline and idle():
                raise RuntimeError("native turn did not pass " + marker + ": " + replies[-1])
            return False

        wait_for(marker, settled)
        print(marker, flush=True)

    def tools_used(thread):
        return [item["payload"] for item in records(thread) if item.get("type") == "response_item"
                and item.get("payload", {}).get("type") == "function_call"]

    def delivered(thread):
        return [item["payload"] for item in records(thread) if item.get("type") == "response_item"
                and item.get("payload", {}).get("type") == "agent_message"
                and item["payload"].get("recipient") == "/root"]

    def execution_requests(thread):
        return [item["payload"] for item in records(thread) if item.get("type") == "response_item"
                and (item.get("payload", {}).get("type") == "custom_tool_call"
                     or (item.get("payload", {}).get("type") == "function_call"
                         and item["payload"].get("namespace") != "collaboration"))]

    try:
        # Existing trusted hooks and native login only; no installation or config writes.
        print(run_crew("check"), flush=True)
        start()
        print(run_crew("up", "--no-attach"), flush=True)
        pane = panes()["tree-lead"]["pane_id"]
        trusted = False

        def bootstrap_ready():
            nonlocal trusted
            if not trusted and root_thread() is None:
                screen = call("pane", "read", pane, "--source", "recent-unwrapped").stdout
                if "Trust this folder?" in screen:
                    assert str(repo) in screen and "1. Trust and continue" in screen
                    # Only code authored by this test is trusted through the normal UI.
                    result("pane", "send-keys", pane, "enter")
                    trusted = True
                    print("Trusted generated test checkout", flush=True)
            return completed("LIVE_BOOTSTRAP_OK")

        wait_for("LIVE_BOOTSTRAP_OK", bootstrap_ready)
        initial_threads = threads()
        assert len(initial_threads) == 3, initial_threads
        root_session = root_thread()
        for name, token in tokens.items():
            child = next(thread for thread in initial_threads if thread["agent_path"] == "/root/" + name)
            assert any(token in text for text in final_texts(child)), "worker never confirmed its original token"
            assert any(item.get("author") == "/root/" + name and token in json.dumps(item)
                       for item in delivered(root_session)), "worker reply never reached main"
        print("LIVE_BOOTSTRAP_OK: both native child replies reached main", flush=True)
        task("Use only the two existing native roles. Ask tree-reviewer via followup_task to inspect math_utils.py against test_math_utils.py and report the bug without edits. Wait for its final response, then pass its findings to tree-coder via followup_task. Have the coder fix math_utils.py and run python3 -m unittest -v. You may read files and run tests but must not implement the fix yourself. No commits. Reply LIVE_WORK_OK only after receiving both native final responses and verifying all tests pass.", "LIVE_WORK_OK")
        tests = subprocess.run([sys.executable, "-m", "unittest", "-v"], cwd=repo, capture_output=True, text=True, timeout=20)
        root.joinpath("unittest.log").write_text(tests.stdout + tests.stderr)
        assert tests.returncode == 0, tests.stdout + tests.stderr
        assert len(threads()) == 3, "work created replacement agents"
        binding_files = list(repo.joinpath(".herdr/codex/sessions").glob("*.json"))
        assert len(binding_files) == 1
        modified = binding_files[0].stat().st_mtime_ns
        stop()
        start()
        wait_for("restored root hook", lambda: binding_files[0].stat().st_mtime_ns > modified, seconds=30)
        wait_for("restored root idle", idle, seconds=30)
        task("Cold restart acceptance: do not spawn any agents. Reuse the same /root/tree_reviewer and /root/tree_coder with followup_task, even if list_agents shows only /root. Ask each to recall its original MEMORY_ token and summarize its previous review or fix. Do not include the token in either new task. Wait for both native final responses. Reply LIVE_RESTORE_OK followed by their recalled tokens only if both existing roles answered.", "LIVE_RESTORE_OK")
        final_threads = threads()
        assert {thread["id"] for thread in final_threads} == {thread["id"] for thread in initial_threads}
        root_calls = tools_used(root_session)
        assert sum(call["name"] == "spawn_agent" for call in root_calls) == 2
        followups = [json.loads(call["arguments"]) for call in root_calls if call["name"] == "followup_task"]
        assert len(followups) >= 4
        # Real tool message arguments are encrypted in the native rollout. The
        # prompt asks for unaided recall; don't pretend ciphertext can prove that
        # the coordinator didn't include a hint. Verify actual child answers.
        last_final = final_texts(root_session)[-1]
        assert all(token in last_final for token in tokens.values()), last_final
        for name, token in tokens.items():
            child = next(thread for thread in final_threads if thread["agent_path"] == "/root/" + name)
            assert token in final_texts(child)[-1], "worker failed native memory recall"
            assert sum(item.get("author") == "/root/" + name for item in delivered(root_session)) >= 3, "main did not receive a worker reply after every phase"
        root_executions = execution_requests(root_session)
        assert not any(item.get("name") == "apply_patch" or "tools.apply_patch" in item.get("input", "")
                       for item in root_executions), "coordinator applied the fix itself"
        coder = next(thread for thread in final_threads if thread["agent_path"] == "/root/tree_coder")
        assert any((item.get("name") == "apply_patch" or "tools.apply_patch" in item.get("input", ""))
                   and "math_utils.py" in json.dumps(item) for item in execution_requests(coder)), "native coder patch was not recorded"
        evidence = {"provider": "real OpenAI model via existing native ChatGPT login", "root": str(root),
                    "threads": [{key: thread[key] for key in ("id", "agent_path", "model", "model_provider")} for thread in final_threads],
                    "passed": ["bootstrap", "native worker replies to main", "review to coder handoff", "real code fix and 3 unit tests", "cold Herdr restart", "same UUIDs", "worker memory recall"],
                    "native_root_tools": [call["name"] for call in root_calls],
                    "root_execution_requests": root_executions, "final": last_final}
        root.joinpath("report.json").write_text(json.dumps(evidence, indent=2) + "\n")
        print("PASS: real-model native tree, review/fix handoff, automatic replies and memory after cold Herdr restart.", flush=True)
        print("Evidence:", root / "report.json", flush=True)
    finally:
        if sys.exc_info()[0] is not None and server is not None:
            try:
                for label, pane in panes().items():
                    root.joinpath(label + "-terminal.json").write_text(call("pane", "read", pane["pane_id"], "--source", "recent-unwrapped").stdout)
            except Exception:
                pass
            print("Failed run artifacts:", root, flush=True)
        stop()
        call("session", "delete", session)


if __name__ == "__main__":
    main()
