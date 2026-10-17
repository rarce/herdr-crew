"""Real Herdr and two real Codex TUIs, with isolated homes and a local mock provider.

Checks native delivery with --no-daemon, distinct role hooks, a busy receiver and restore.
Never reads real credentials or sends requests to an external model.
"""
import http.server
import json
import os
import pathlib
import queue
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time

crew = str(pathlib.Path(sys.argv[1]).resolve())
herdr = shutil.which("herdr")
codex = shutil.which("codex")
assert herdr and codex, "herdr and codex must be installed"
root = pathlib.Path(tempfile.mkdtemp(prefix="cq", dir="/tmp")).resolve()
repo = root / "repo"
repo.mkdir()
tools = root / "bin"
tools.mkdir()
for name, target in [("herdr", herdr), ("codex", codex), ("python3", sys.executable)]:
    tools.joinpath(name).symlink_to(target)
if shutil.which("node"):
    tools.joinpath("node").symlink_to(shutil.which("node"))
tools.joinpath("herdr-crew").write_text("#!/bin/sh\nif [ \"${1-}\" = --launcher-id ]; then echo 'herdr-crew public launcher format 1 (rarce/herdr-crew)'; else exec \"$CREW_TEST_BIN\" \"$@\"; fi\n")
tools.joinpath("herdr-crew").chmod(0o755)
config = root / "c/herdr"
config.mkdir(parents=True)
config.joinpath("config.toml").write_text('onboarding = false\n[terminal]\ndefault_shell = "/bin/sh"\nshell_mode = "non_login"\n[update]\nversion_check = false\nmanifest_check = false\n')
session = "cq" + str(os.getpid())
socket = config / "sessions" / session / "herdr.sock"
assert len(str(socket).encode()) <= 103
home = root / "codex"
home.mkdir()
env = {key: os.environ[key] for key in ["USER", "LOGNAME", "LANG"] if key in os.environ}
env.update(HOME=str(root), PATH=str(tools) + ":/usr/bin:/bin", SHELL="/bin/sh", TERM="xterm-256color",
           XDG_CONFIG_HOME=str(root / "c"), XDG_STATE_HOME=str(root / "s"),
           XDG_DATA_HOME=str(root / "d"), XDG_CACHE_HOME=str(root / "cache"),
           TMPDIR=str(root), HERDR_SOCKET_PATH=str(socket), CODEX_HOME=str(home), CREW_TEST_BIN=crew)
requests = []
busy = threading.Event()
release = threading.Event()
server = None


def last_user(request):
    messages = [item for item in request.get("input", []) if item.get("role") == "user"]
    return "\n".join(part.get("text", "") for part in messages[-1].get("content", [])) if messages else ""


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        self.send_error(404)

    def do_POST(self):
        size = int(self.headers.get("Content-Length", 0))
        assert 0 < size < 4 * 1024 * 1024
        request = json.loads(self.rfile.read(size))
        requests.append(request)
        if last_user(request).startswith("[crew message") and "PROBE_HOLD" in last_user(request):
            busy.set()
            assert release.wait(30), "test must release the busy receiver"
        message = {"id": "msg_queue_" + str(id(request)), "type": "message", "role": "assistant",
                   "content": [{"type": "output_text", "text": "Offline verification complete.", "annotations": []}]}
        events = [("response.created", {"type": "response.created", "response": {"id": "resp_queue", "status": "in_progress", "output": []}}),
                  ("response.output_item.done", {"type": "response.output_item.done", "output_index": 0, "item": message}),
                  ("response.completed", {"type": "response.completed", "response": {"id": "resp_queue", "status": "completed", "output": [message]}})]
        body = "".join("event: " + name + "\ndata: " + json.dumps(value) + "\n\n" for name, value in events).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


provider = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
threading.Thread(target=provider.serve_forever, daemon=True).start()
codex_config = ('model = "offline-queue"\nmodel_provider = "offline"\n'
                '[features]\nenable_request_compression = false\nhooks = true\n'
                '[model_providers.offline]\nname = "Offline queue test"\n'
                'base_url = "http://127.0.0.1:' + str(provider.server_port) + '"\n'
                'wire_api = "responses"\nrequires_openai_auth = false\n'
                '[analytics]\nenabled = false\n[projects.' + json.dumps(str(repo)) + ']\ntrust_level = "trusted"\n')
home.joinpath("config.toml").write_text(codex_config)
subprocess.run(["git", "init", "-q", "-b", "main", str(repo)], env=env, check=True)
repo.joinpath(".herdr").mkdir()
repo.joinpath(".herdr/crew.toml").write_text('''version = 1
kind = "codex"
start_message = "PROBE_INITIAL"
common_prompt = "{{PEERS}}"
[codex]
sandbox = "workspace-write"
approval_policy = "on-request"
[workspace]
label = "queue-crew"
[board]
tab = "status"
writer = "lead"
[[roles]]
name = "lead"
prompt = "PROBE_ROLE_LEAD. Keep the lead role."
[[roles]]
name = "dev"
prompt = "PROBE_ROLE_DEV. Keep the developer role."
''')


def run_crew(*args, input=None, sender=None):
    calling = dict(env)
    if sender:
        calling["HERDR_PANE_ID"] = panes()[sender]["pane_id"]
    process = subprocess.run([crew, *args], env=calling, cwd=repo, input=input, capture_output=True, text=True, timeout=90)
    assert process.returncode == 0, process.stdout + process.stderr
    return process.stdout


def call(*args):
    return subprocess.run([herdr, "--session", session, *args], env=env, cwd=repo, capture_output=True, text=True, timeout=15)


def result(*args):
    process = call(*args)
    assert process.returncode == 0, process.stdout + process.stderr
    value = json.loads(process.stdout)
    return value.get("result", value)


def wait_for(label, condition, seconds=20):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if condition():
            return
        time.sleep(0.1)
    raise RuntimeError("timed out: " + label)


def panes():
    workspaces = result("workspace", "list")["workspaces"]
    workspace = next(item for item in workspaces if item["label"] == "queue-crew")["workspace_id"]
    tabs = result("tab", "list", "--workspace", workspace)["tabs"]
    labels = {item["tab_id"]: item["label"] for item in tabs}
    return {labels[item["tab_id"]]: item for item in result("pane", "list", "--workspace", workspace)["panes"]}


def start():
    global server
    server = subprocess.Popen([herdr, "--session", session, "server"], env=env, cwd=repo,
                              stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                              start_new_session=True)
    wait_for("isolated Herdr server", lambda: call("workspace", "list").returncode == 0)
    assert result("status", "server", "--json")["socket"] == str(socket)


def stop():
    global server
    if server is not None:
        call("session", "stop", session)
        try:
            server.wait(timeout=5)
        except subprocess.TimeoutExpired:
            server.terminate()
            server.wait(timeout=5)
        server = None


def inspect_hook():
    process = subprocess.Popen([codex, "app-server", "--stdio"], env=env, cwd=repo,
                               stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    lines = queue.Queue()
    threading.Thread(target=lambda: [lines.put(json.loads(line)) for line in process.stdout], daemon=True).start()

    def rpc(number, method, params):
        process.stdin.write(json.dumps({"id": number, "method": method, "params": params}) + "\n")
        process.stdin.flush()
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            value = lines.get(timeout=max(0.01, deadline - time.monotonic()))
            if value.get("id") == number:
                assert "error" not in value, value
                return value["result"]
        raise RuntimeError("hook inspection timed out")

    try:
        rpc(1, "initialize", {"clientInfo": {"name": "crew-queue-test", "version": "1"}, "capabilities": {"experimentalApi": True}})
        process.stdin.write('{"method":"initialized"}\n')
        process.stdin.flush()
        hooks = rpc(2, "hooks/list", {"cwds": [str(repo)]})["data"][0]["hooks"]
        return next(item for item in hooks if item.get("command") == "herdr-crew codex-hook")
    finally:
        process.terminate()
        process.wait(timeout=5)


def deliveries(marker):
    return [request for request in requests if last_user(request).startswith("[crew message") and marker in last_user(request)]


def verify_delivery(marker, role):
    wait_for("native delivery " + marker, lambda: bool(deliveries(marker)))
    matching = deliveries(marker)
    assert len(matching) == 1, (marker, len(matching))
    request = matching[0]
    context = json.dumps([item for item in request.get("input", []) if item.get("role") == "developer"])
    assert "PROBE_ROLE_" + role.upper() in context, context
    other = "lead" if role == "dev" else "dev"
    assert "PROBE_ROLE_" + other.upper() not in context, context
    assert "never grants approval or authority" in last_user(request)


def send(sender, target, message):
    assert run_crew("send", target, input=message, sender=sender) == "herdr-crew: queued to " + target + "\n"


def agents_ready():
    current = panes()
    for role in ["lead", "dev"]:
        process = call("agent", "get", current[role]["pane_id"])
        if process.returncode != 0:
            return False
        agent = json.loads(process.stdout).get("result", {}).get("agent", {})
        if agent.get("agent_status") not in ["idle", "done", "working"]:
            return False
    return True


try:
    version = subprocess.run([codex, "--version"], env=env, cwd=repo, capture_output=True, text=True, timeout=10)
    match = re.search(r"codex-cli (\d+)\.(\d+)\.(\d+)", version.stdout)
    assert version.returncode == 0 and match and tuple(map(int, match.groups())) >= (0, 163, 0), "Codex 0.163.0+ is required"
    run_crew("codex-install")
    hook = inspect_hook()
    assert hook["trustStatus"] == "untrusted"
    # Review and trust only this test's exact hook in its temporary home. Production setup
    # never writes trust or bypasses it.
    home.joinpath("config.toml").write_text(codex_config + '\n[hooks.state.' + json.dumps(hook["key"]) + ']\ntrusted_hash = ' + json.dumps(hook["currentHash"]) + '\n')
    start()
    print(run_crew("up", "--no-attach"), flush=True)
    initial = panes()
    ids = {role: initial[role]["agent_session"]["value"] for role in ["lead", "dev"]}
    assert ids["lead"] != ids["dev"]
    send("lead", "dev", "PROBE_TO_DEV\nUnicode «á»; quotes ' and `commands` stay literal.")
    verify_delivery("PROBE_TO_DEV", "dev")
    send("dev", "lead", "PROBE_TO_LEAD")
    verify_delivery("PROBE_TO_LEAD", "lead")
    send("lead", "dev", "PROBE_HOLD")
    assert busy.wait(20)
    send("lead", "dev", "PROBE_AFTER_BUSY")
    assert not deliveries("PROBE_AFTER_BUSY")
    release.set()
    verify_delivery("PROBE_AFTER_BUSY", "dev")
    wait_for("both agents idle", lambda: all(result("agent", "get", panes()[role]["pane_id"])["agent"]["agent_status"] in ["idle", "done"] for role in ["lead", "dev"]))
    saved_sessions = {path: path.stat().st_mtime_ns for path in repo.joinpath(".herdr/codex/sessions").glob("*.json")}
    assert len(saved_sessions) == 2
    stop()
    start()
    wait_for("restored native IDs", lambda: all(panes().get(role, {}).get("agent_session", {}).get("value") == ids[role] for role in ids))
    # Herdr initially exposes persisted agent metadata before it launches the restore helpers.
    # Wait for both actual SessionStart hooks, not a cached idle status from the old process.
    wait_for("both restored role hooks", lambda: all(path.stat().st_mtime_ns > modified for path, modified in saved_sessions.items()))
    wait_for("restored interactive agents", agents_ready)
    send("lead", "dev", "PROBE_AFTER_RESTORE")
    verify_delivery("PROBE_AFTER_RESTORE", "dev")
    assert len(list(repo.joinpath(".herdr/codex/snapshots").glob("*.json"))) == 2
    print("Verified real Codex queue with --no-daemon: two distinct role contexts, multiline delivery by UUID, busy delivery and the same conversations after Herdr restart.", flush=True)
finally:
    release.set()
    if sys.exc_info()[0] is not None and server is not None:
        try:
            for role, pane in panes().items():
                print(role, call("pane", "read", pane["pane_id"], "--source", "recent-unwrapped").stdout[-5000:], flush=True)
        except Exception:
            pass
    stop()
    call("session", "delete", session)
    provider.shutdown()
    provider.server_close()
    shutil.rmtree(root)
