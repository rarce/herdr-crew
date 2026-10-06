"""Real Herdr with fake Codex: native identity, hook context, persistence and restore.

All processes and files belong to this temporary named test session. No model requests.
"""
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time

crew = str(pathlib.Path(sys.argv[1]).resolve())
herdr = shutil.which("herdr")
assert herdr, "herdr must be installed"
root = pathlib.Path(tempfile.mkdtemp(prefix="cx", dir="/tmp")).resolve()
session = "crewcodex"
repo = root / "repo"
repo.mkdir()
tools = root / "bin"
tools.mkdir()
captures = root / "captures"
captures.mkdir()
for name, target in [("herdr", herdr), ("python3", sys.executable)]:
    tools.joinpath(name).symlink_to(target)
tools.joinpath("codex").write_bytes(pathlib.Path(__file__).with_name("codex.py").read_bytes())
tools.joinpath("codex").chmod(0o755)
tools.joinpath("herdr-crew").write_text("#!/bin/sh\nif [ \"${1-}\" = --launcher-id ]; then echo 'herdr-crew public launcher format 1 (rarce/herdr-crew)'; else exec \"$CREW_TEST_BIN\" \"$@\"; fi\n")
tools.joinpath("herdr-crew").chmod(0o755)
config = root / "c/herdr"
config.mkdir(parents=True)
config.joinpath("config.toml").write_text('onboarding = false\n[terminal]\ndefault_shell = "/bin/sh"\nshell_mode = "non_login"\n[update]\nversion_check = false\nmanifest_check = false\n')
socket = config / "sessions" / session / "herdr.sock"
assert len(str(socket).encode()) <= 103
env = {key: os.environ[key] for key in ["USER", "LOGNAME", "LANG", "TMPDIR"] if key in os.environ}
env.update(HOME=str(root), PATH=str(tools) + ":/usr/bin:/bin", SHELL="/bin/sh", TERM="xterm-256color",
           XDG_CONFIG_HOME=str(root / "c"), XDG_STATE_HOME=str(root / "s"), HERDR_SOCKET_PATH=str(socket),
           CODEX_HOME=str(root / "codex-home"), CREW_TEST_BIN=crew, CREW_TEST_STATE=str(captures),
           CREW_TEST_NATIVE="1", CREW_TEST_PHASE="initial")
subprocess.run(["git", "init", "-q", "-b", "main", str(repo)], env=env, check=True)
repo.joinpath(".herdr").mkdir()
repo.joinpath(".herdr/crew.toml").write_text('''version = 1
kind = "codex"
start_message = "Confirm role; it's literal $HOME `id`."
[codex]
model = "original-model"
reasoning_effort = "high"
sandbox = "workspace-write"
approval_policy = "on-request"
[workspace]
label = "native-crew"
[board]
tab = "status"
writer = "dev"
[[roles]]
name = "dev"
prompt = ''' + "'''Developer {{NAME}}. Unicode «á»; quotes ' and `commands` stay literal.'''\n")
server = None

def call(*args, timeout=15):
    return subprocess.run([herdr, "--session", session, *args], env=env, cwd=repo, capture_output=True, text=True, timeout=timeout)

def result(*args):
    process = call(*args)
    assert process.returncode == 0, process.stdout + process.stderr
    return json.loads(process.stdout)["result"] if process.stdout.strip() else {}

def wait_for(label, condition, seconds=15):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if condition():
            return
        time.sleep(0.1)
    raise RuntimeError("timed out: " + label)

def start():
    global server
    server = subprocess.Popen([herdr, "--session", session, "server"], env=env, cwd=repo,
                              stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                              start_new_session=True)
    wait_for("isolated server", lambda: call("workspace", "list").returncode == 0)
    status = json.loads(call("status", "server", "--json").stdout)
    assert status["socket"] == str(socket), status

def stop():
    global server
    call("session", "stop", session)
    if server:
        try:
            server.wait(timeout=5)
        except subprocess.TimeoutExpired:
            server.terminate()
            server.wait(timeout=5)
        server = None

try:
    start()
    guard = result("workspace", "create", "--cwd", str(repo), "--label", "guard", "--no-focus")
    pane = guard["root_pane"]["pane_id"]
    time.sleep(0.3)
    result("pane", "run", pane, "command -v codex > '" + str(root / "which-codex") + "'")
    wait_for("guard", lambda: root.joinpath("which-codex").exists())
    assert root.joinpath("which-codex").read_text().strip() == str(tools / "codex")
    result("workspace", "close", guard["workspace"]["workspace_id"])
    process = subprocess.run([crew, "up", "--no-attach"], env=env, cwd=repo, capture_output=True, text=True, timeout=90)
    print(process.stdout + process.stderr, flush=True)
    if process.returncode != 0:
        for workspace in result("workspace", "list")["workspaces"]:
            if workspace["label"] == "native-crew":
                panes = result("pane", "list", "--workspace", workspace["workspace_id"])
                print("PANES:", json.dumps(panes), flush=True)
                for pane in panes["panes"]:
                    print(call("pane", "read", pane["pane_id"], "--source", "recent-unwrapped").stdout, flush=True)
        for file in captures.glob("*.json"):
            print(file.name, file.read_text(), flush=True)
    assert process.returncode == 0
    wait_for("initial hook", lambda: captures.joinpath("initial-hook.json").exists())
    initial = json.loads(captures.joinpath("initial-hook.json").read_text())
    assert "Developer dev" in initial["hookSpecificOutput"]["additionalContext"], initial
    snapshots = list(repo.joinpath(".herdr/codex/snapshots").glob("*.json"))
    assert len(snapshots) == 1
    token = json.loads(snapshots[0].read_text())["token"]
    expected = ["herdr-crew", "codex-resume", token]
    time.sleep(0.5)
    stop()
    saved = []
    for file in root.joinpath("c").rglob("*.json"):
        try:
            data = json.loads(file.read_text())
        except Exception:
            continue
        def collect(value):
            if isinstance(value, dict):
                if "agent_resume" in value:
                    saved.append(value["agent_resume"])
                for child in value.values(): collect(child)
            elif isinstance(value, list):
                for child in value: collect(child)
        collect(data)
    assert any(item and item.get("argv") == expected for item in saved), saved
    crew_file = repo / ".herdr/crew.toml"
    crew_file.write_text(crew_file.read_text().replace("original-model", "changed-model").replace("Developer", "Changed role"))
    env["CREW_TEST_PHASE"] = "restored"
    start()
    wait_for("configured native restoration", lambda: captures.joinpath("restored-hook.json").exists(), seconds=30)
    restored = json.loads(captures.joinpath("restored-hook.json").read_text())
    assert restored == initial, restored
    argv = json.loads(captures.joinpath("restored-args.json").read_text())
    assert argv[0] == "resume" and argv[-1] == "00000000-0000-0000-0000-000000000001", argv
    assert 'model="original-model"' in argv and '--no-daemon' in argv, argv
    assert "Confirm role; it's literal $HOME `id`." not in argv
    print("Verified real Herdr: exact hook context, persisted custom recovery, native-hook coexistence, saved options and no initial message on restore.", flush=True)
finally:
    stop()
    call("session", "delete", session)
    shutil.rmtree(root)
