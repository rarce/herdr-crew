"""Real Codex collaboration tree in a disposable Herdr server and Codex home.

The local scripted Responses provider requests actual native tools. It simulates
model decisions, not Codex, Herdr, tools, filesystem enforcement, or persistence.
No credentials or external model requests. Failed runs retain their artifacts.
"""
import http.server
import importlib.util
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
import tomllib

crew = str(pathlib.Path(sys.argv[1]).resolve())
herdr = shutil.which("herdr")
codex = shutil.which("codex")
assert herdr and codex, "herdr and codex must be installed"
root = pathlib.Path(tempfile.mkdtemp(prefix="ct", dir="/tmp")).resolve()
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
session = "ct" + str(os.getpid())
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
provider_errors = []
server = None
manifest = None
phase = "bootstrap"
steps = {}
calls = {}
sys.dont_write_bytecode = True


def texts(item):
    return "\n".join(part.get("text", part.get("encrypted_content", ""))
                     for part in item.get("content", []) if isinstance(part, dict))


def identity(request):
    messages = [item for item in request.get("input", [])
                if item.get("type") == "agent_message" and item.get("recipient", "").startswith("/root/")]
    return messages[-1]["recipient"] if messages else "/root"


def is_agent_request(request):
    return any(item.get("type") == "function" and item.get("name") == "exec_command"
               for item in request.get("tools", []))


def message(text):
    return {"type": "message", "id": "msg_" + str(time.time_ns()), "role": "assistant",
            "content": [{"type": "output_text", "text": text, "annotations": []}]}


def tool(name, args, namespace="collaboration"):
    number = str(time.time_ns())
    return {"type": "function_call", "id": "fc_" + number, "call_id": "call_" + number,
            "namespace": namespace, "name": name, "arguments": json.dumps(args)}


def decide(request):
    # The TUI also asks the provider for background titles. Those requests have
    # no collaboration tools and are not agent turns.
    if not is_agent_request(request):
        return message("Native crew experiment")
    who = identity(request)
    key = (phase, who)
    step = steps.get(key, 0)
    steps[key] = step + 1
    if who != "/root":
        task = who.rsplit("/", 1)[1]
        if phase == "bootstrap":
            if step == 0:
                cmd = "printf 'allowed\n' > " + task + ".txt\nprintf 'denied\n' > .codex/forbidden-" + task
                return tool("exec_command", {"cmd": cmd, "login": False, "yield_time_ms": 1000}, namespace=None)
        return message("DONE_" + phase + "_" + task)
    if phase == "bootstrap":
        actions = [("list_agents", {}),
                   *[("spawn_agent", {"task_name": role["task_name"], "fork_turns": "none", "message": role["message"]}) for role in manifest],
                   ("list_agents", {})]
    elif phase == "note":
        actions = [("send_message", {"target": "/root/tree_reviewer", "message": "INFORMATION_ONLY_NOTE"})]
    elif phase == "handoff":
        actions = [("followup_task", {"target": "/root/tree_coder", "message": "Reviewer findings: DONE_followup_tree_reviewer"})]
    else:
        actions = [("list_agents", {}),
                   *[("followup_task", {"target": "/root/" + role["task_name"], "message": "TASK_" + phase + " for " + role["role"]}) for role in manifest]]
    if step < len(actions):
        return tool(*actions[step])
    if phase != "note":
        history = json.dumps(request.get("input", []))
        expected = [role for role in manifest if phase != "handoff" or role["task_name"] == "tree_coder"]
        if not all("DONE_" + phase + "_" + role["task_name"] in history for role in expected):
            assert step < 25, "native workers did not report their results"
            return tool("wait_agent", {"timeout_ms": 10000})
    return message("ROOT_DONE_" + phase)


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        self.send_error(404)

    def do_POST(self):
        try:
            size = int(self.headers.get("Content-Length", 0))
            assert 0 < size < 4 * 1024 * 1024
            request = json.loads(self.rfile.read(size))
            with root.joinpath("requests.jsonl").open("a") as file:
                file.write(json.dumps({"phase": phase, "request": request}) + "\n")
            requests.append((phase, request))
            output = decide(request)
            if output.get("type") == "function_call":
                calls[output["call_id"]] = {"phase": phase, "agent": identity(request),
                                            "name": output["name"], "namespace": output["namespace"]}
            response_id = "resp_" + str(time.time_ns())
            events = [("response.created", {"type": "response.created", "response": {"id": response_id, "status": "in_progress", "output": []}}),
                      ("response.output_item.done", {"type": "response.output_item.done", "output_index": 0, "item": output}),
                      ("response.completed", {"type": "response.completed", "response": {"id": response_id, "status": "completed", "output": [output]}})]
            body = "".join("event: " + name + "\ndata: " + json.dumps(value) + "\n\n" for name, value in events).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        except Exception as error:
            provider_errors.append(repr(error))
            self.send_error(500)


provider = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
threading.Thread(target=provider.serve_forever, daemon=True).start()
codex_config = ('model = "offline-collaboration"\nmodel_provider = "offline"\n'
                '[agents]\nenabled = true\nmax_concurrent_threads_per_session = 3\n'
                '[features]\nmulti_agent_v2 = true\nenable_request_compression = false\nhooks = true\n'
                '[model_providers.offline]\nname = "Offline collaboration test"\n'
                'base_url = "http://127.0.0.1:' + str(provider.server_port) + '"\n'
                'wire_api = "responses"\nrequires_openai_auth = false\n'
                '[analytics]\nenabled = false\n[projects.' + json.dumps(str(repo)) + ']\ntrust_level = "trusted"\n')
home.joinpath("config.toml").write_text(codex_config)
subprocess.run(["git", "init", "-q", "-b", "main", str(repo)], env=env, check=True)
repo.joinpath(".codex").mkdir()
prototype = pathlib.Path(__file__).resolve().parents[2] / "experiments/codex_tree.py"
spec = importlib.util.spec_from_file_location("codex_tree", prototype)
experiment = importlib.util.module_from_spec(spec)
spec.loader.exec_module(experiment)
fixture = pathlib.Path(__file__).resolve().parents[1] / "fixtures/crew-codex-tree.toml"
with fixture.open("rb") as file:
    generated, manifest = experiment.prepare(tomllib.load(file), repo)
repo.joinpath(".herdr").mkdir()
repo.joinpath(".herdr/crew.toml").write_text(generated)
print("Isolated experiment:", root, flush=True)

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
    workspace = next(item for item in workspaces if item["label"] == "tree-crew")["workspace_id"]
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



def completed(label):
    # Look for the actual native final output in the saved rollout, not the provider's decision.
    for path in home.joinpath("sessions").rglob("*.jsonl"):
        if 'ROOT_DONE_' + label in path.read_text():
            return True
    return False


def root_pane():
    return panes()["tree-lead"]


def ready():
    process = call("agent", "get", root_pane()["pane_id"])
    if process.returncode != 0:
        return False
    return json.loads(process.stdout).get("result", {}).get("agent", {}).get("agent_status") in ["idle", "done"]


def send_phase(label):
    global phase
    phase = label
    output = run_crew("send", "tree-lead", input="Run the native crew experiment phase " + label)
    assert output == "herdr-crew: queued to tree-lead\n", output
    wait_for("root completed " + label, lambda: completed(label), seconds=40)
    wait_for("root idle " + label, ready)


def tool_outputs(label):
    found = {}
    for current, request in requests:
        if current == label:
            for item in request.get("input", []):
                if item.get("type") == "function_call_output" and calls.get(item["call_id"], {}).get("phase") == label:
                    found[item["call_id"]] = item["output"]
    return found


def verify_tools(label):
    outputs = tool_outputs(label)
    issued = {key: value for key, value in calls.items() if value["phase"] == label}
    assert outputs.keys() == issued.keys(), ("missing native tool outputs", issued.keys() - outputs.keys())
    for key, output in outputs.items():
        call = issued[key]
        if call["name"] == "exec_command":
            assert "operation not permitted" in output.lower(), output
        elif call["name"] == "spawn_agent":
            assert json.loads(output)["task_name"].startswith("/root/tree_"), output
        elif call["name"] == "list_agents":
            assert json.loads(output)["agents"], output
        elif call["name"] == "wait_agent":
            assert "timed_out" in json.loads(output), output
        else:
            assert output == "", (call, output)
    print(label + ": verified " + str(len(outputs)) + " native tool calls", flush=True)


def verify_results(label, tasks=("tree_reviewer", "tree_coder")):
    root_requests = [request for current, request in requests
                     if current == label and is_agent_request(request) and identity(request) == "/root"]
    delivered = [item for request in root_requests for item in request.get("input", [])
                 if item.get("type") == "agent_message" and item.get("recipient") == "/root"]
    for task in tasks:
        assert any(item.get("author") == "/root/" + task
                   and "FINAL_ANSWER" in texts(item) and "DONE_" + label + "_" + task in texts(item)
                   for item in delivered), ("missing native final result", label, task)


def session_metadata():
    found = []
    for path in home.joinpath("sessions").rglob("*.jsonl"):
        for line in path.read_text().splitlines():
            item = json.loads(line)
            if item.get("type") == "session_meta":
                found.append(item["payload"])
                break
    return found


try:
    version = subprocess.run([codex, "--version"], env=env, cwd=repo, capture_output=True, text=True, timeout=10)
    match = re.search(r"codex-cli (\d+)\.(\d+)\.(\d+)", version.stdout)
    assert version.returncode == 0 and match and tuple(map(int, match.groups())) >= (0, 163, 0), "Codex 0.163.0+ is required"
    run_crew("codex-install")
    hook = inspect_hook()
    assert hook["trustStatus"] == "untrusted"
    home.joinpath("config.toml").write_text(codex_config + '\n[hooks.state.' + json.dumps(hook["key"]) + ']\ntrusted_hash = ' + json.dumps(hook["currentHash"]) + '\n')
    start()
    print(run_crew("up", "--no-attach"), flush=True)
    wait_for("native tree bootstrap", lambda: completed("bootstrap"), seconds=45)
    wait_for("root idle", ready)
    assert not provider_errors, provider_errors
    verify_tools("bootstrap")
    verify_results("bootstrap")
    before = session_metadata()
    print("Session identities:", json.dumps([{key: item.get(key) for key in ("id", "session_id", "parent_thread_id", "agent_path")} for item in before]), flush=True)
    assert len(before) == 3, "one coordinator and exactly two native workers are required"
    root_id = root_pane()["agent_session"]["value"]
    assert all(item.get("session_id") == root_id for item in before), "roles do not share one native session tree"
    assert all(item.get("parent_thread_id") == root_id for item in before if item["id"] != root_id)
    for role in manifest:
        initial_requests = [request for current, request in requests
                            if current == "bootstrap" and identity(request) == "/root/" + role["task_name"]]
        assert initial_requests, ("worker never ran", role["role"])
        assert any(role["message"] in texts(item) for request in initial_requests
                   for item in request.get("input", []) if item.get("type") == "agent_message"), "worker did not receive its complete role instructions"
        assert repo.joinpath(role["task_name"] + ".txt").read_text() == "allowed\n"
        assert not repo.joinpath(".codex/forbidden-" + role["task_name"]).exists(), "child escaped the workspace-write policy"
    send_phase("note")
    verify_tools("note")
    assert not any(current == "note" and is_agent_request(request) and identity(request) != "/root" for current, request in requests), "send_message unexpectedly activated an idle child"
    send_phase("followup")
    verify_tools("followup")
    verify_results("followup")
    assert any(current == "followup" and identity(request) == "/root/tree_reviewer" and "INFORMATION_ONLY_NOTE" in json.dumps(request["input"]) for current, request in requests), "queued information never reached the worker"
    send_phase("handoff")
    verify_tools("handoff")
    verify_results("handoff", tasks=("tree_coder",))
    assert any(current == "handoff" and identity(request) == "/root/tree_coder" and "Reviewer findings: DONE_followup_tree_reviewer" in json.dumps(request["input"]) for current, request in requests), "coordinator did not forward the review to the coder"
    assert len(session_metadata()) == 3, "followup created duplicate workers"
    native_id = root_pane()["agent_session"]["value"]
    saved_sessions = {path: path.stat().st_mtime_ns for path in repo.joinpath(".herdr/codex/sessions").glob("*.json")}
    assert len(saved_sessions) == 1, "children must not acquire independent Herdr bindings"
    stop()
    start()
    wait_for("same root restored", lambda: root_pane().get("agent_session", {}).get("value") == native_id)
    wait_for("restored crew hook", lambda: all(path.stat().st_mtime_ns > modified for path, modified in saved_sessions.items()), seconds=30)
    wait_for("restored root ready", ready)
    send_phase("restored")
    verify_tools("restored")
    verify_results("restored")
    for role in manifest:
        assert any(current == "restored" and identity(request) == "/root/" + role["task_name"]
                   and "DONE_followup_" + role["task_name"] in json.dumps(request["input"])
                   for current, request in requests), "worker history was lost during restoration"
    assert {item["id"] for item in session_metadata()} == {item["id"] for item in before}, "restoration changed native identities"
    assert root_pane()["agent_session"]["value"] == native_id, "a child overwrote the coordinator's Herdr identity"
    assert len(list(repo.joinpath(".herdr/codex/snapshots").glob("*.json"))) == 1
    child_requests = [request for _, request in requests if identity(request) != "/root"]
    child_collaboration_tools = any(item.get("name") == "collaboration" for request in child_requests
                                    for item in request.get("tools", []))
    assert not provider_errors, provider_errors
    report = {"codex": version.stdout.strip(), "herdr": result("status", "server", "--json")["version"],
              "provider": "local scripted Responses; no external model", "sandbox": "workspace-write",
              "phases": ["bootstrap", "note", "followup", "handoff", "restored"],
              "session_tree": [{key: item.get(key) for key in ("id", "session_id", "parent_thread_id", "agent_path")} for item in before],
              "native_tool_calls": len(calls), "same_ids_after_restart": True,
              "child_collaboration_tools": child_collaboration_tools}
    if os.environ.get("CREW_TREE_REPORT"):
        pathlib.Path(os.environ["CREW_TREE_REPORT"]).write_text(json.dumps(report, indent=2) + "\n")
    print("PASS: native root coordination, two automatic worker reports, sandbox enforcement, idle followups, review handoff and same threads after Herdr restart.", flush=True)
finally:
    failed = sys.exc_info()[0] is not None
    if failed and server is not None:
        try:
            for role, pane in panes().items():
                print(role, call("pane", "read", pane["pane_id"], "--source", "recent-unwrapped").stdout[-5000:], flush=True)
        except Exception:
            pass
        print("Provider errors:", provider_errors, flush=True)
    stop()
    call("session", "delete", session)
    provider.shutdown()
    provider.server_close()
    if not failed and not os.environ.get("CREW_TREE_KEEP"):
        shutil.rmtree(root)
    else:
        print("Experiment artifacts retained:", root, flush=True)
