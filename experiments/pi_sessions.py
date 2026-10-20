#!/usr/bin/env python3
"""Opt-in Pi/Herdr messaging experiment, with all mutable state under /tmp.

Requires a locally unpacked pi-sessions 0.20.0 package, Pi 1.3.0 and Node 24/25.
Default: deterministic localhost model, no credentials. --live-model connects to an
existing local OpenAI-compatible endpoint, with no authentication or remote fallback.
"""
import argparse
import http.server
import json
import os
import pathlib
import pty
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("crew", type=pathlib.Path)
    parser.add_argument("--package", required=True, type=pathlib.Path)
    parser.add_argument("--node", required=True, type=pathlib.Path)
    parser.add_argument("--live-model", help="Local model ID (otherwise use the deterministic provider)")
    parser.add_argument("--local-url", default="http://127.0.0.1:8000/v1")
    args = parser.parse_args()
    crew, package, node = args.crew.resolve(), args.package.resolve(), args.node.resolve()
    source = pathlib.Path(__file__).resolve().parents[1]
    assert json.loads(package.joinpath("package.json").read_text())["version"] == "0.20.0"
    version = subprocess.check_output([str(node), "--version"], text=True).strip()
    assert int(version[1:].split(".")[0]) in (24, 25), "pi-sessions requires Node >=24 <26"
    if args.live_model:
        from urllib.parse import urlparse
        url = urlparse(args.local_url)
        assert url.hostname in ("127.0.0.1", "localhost", "::1"), "Live mode is local-only"
    herdr, pi = shutil.which("herdr"), shutil.which("pi")
    assert herdr and pi
    root = pathlib.Path(tempfile.mkdtemp(prefix="cp", dir="/tmp")).resolve()
    print("Evidence:", root, flush=True)
    repo, foreign, agent, tools, evidence = [root / name for name in ("repo", "foreign", "agent", "bin", "evidence")]
    for path in (repo, foreign, agent, tools, evidence):
        path.mkdir()
    session = "cp" + str(os.getpid())
    config = root / "c/herdr"
    config.mkdir(parents=True)
    config.joinpath("config.toml").write_text('onboarding = false\n[terminal]\ndefault_shell = "/bin/sh"\nshell_mode = "non_login"\n[update]\nversion_check = false\nmanifest_check = false\n')
    socket = config / "sessions" / session / "herdr.sock"
    assert len(str(socket).encode()) <= 103
    ids = {role: str(uuid.uuid4()) for role in ("lead", "dev", "foreign")}
    roster = root / "roster.json"
    roster.write_text(json.dumps({"version": 1, "roles": {role: {"id": ids[role], "cwd": str(repo)} for role in ("lead", "dev")}}))
    for name, target in (("herdr", herdr), ("node", node), ("python3", sys.executable)):
        tools.joinpath(name).symlink_to(target)
    tools.joinpath("herdr-crew").write_text('#!/bin/sh\nif [ "${1-}" = --launcher-id ]; then echo "herdr-crew public launcher format 1 (rarce/herdr-crew)"; else exec "$CREW_TEST_BIN" "$@"; fi\n')
    tools.joinpath("herdr-crew").chmod(0o755)
    # Explicit resources only; global settings, packages, auth and extensions are never loaded.
    scoped = root / "scoped.ts"
    scoped.write_text(f'import install from {json.dumps(str(package / "extensions/pi-sessions.ts"))};\nimport {{ scopedPiSessions }} from {json.dumps(str(source / "experiments/pi_sessions_scope.ts"))};\nexport default (pi) => scopedPiSessions(pi, install);\n')
    wrapper = tools / "pi"
    wrapper.write_text('#!' + sys.executable + '\n' + f'''
import os, sys
args = sys.argv[1:]
if "--version" in args:
    os.execv({str(pi)!r}, [{str(pi)!r}, "--version"])
if "--session" not in args:
    role = args[args.index("--herdr-crew-role") + 1]
    args = ["--session-id", {ids!r}[role]] + args
extra = ["--approve", "--offline", "--no-extensions", "--no-mcp", "--no-skills", "--no-context-files", "--no-prompt-templates",
         "-e", {str(agent / "extensions/herdr-crew.ts")!r}, "-e", {str(agent / "extensions/herdr-agent-state.ts")!r},
         "-e", {str(scoped)!r}, "-e", {str(source / "tests/support/pi_probe.ts")!r}]
os.execv({str(pi)!r}, [{str(pi)!r}] + extra + args)
''')
    wrapper.chmod(0o755)
    env = {key: os.environ[key] for key in ("USER", "LOGNAME", "LANG") if key in os.environ}
    env.update(HOME=str(root), PATH=str(tools) + ":/usr/bin:/bin", SHELL="/bin/sh", TERM="xterm-256color",
               XDG_CONFIG_HOME=str(root / "c"), XDG_STATE_HOME=str(root / "s"),
               XDG_DATA_HOME=str(root / "d"), XDG_CACHE_HOME=str(root / "cache"), TMPDIR=str(root),
               HERDR_SOCKET_PATH=str(socket), PI_CODING_AGENT_DIR=str(agent),
               PI_SESSIONS_MESSAGING_DIR=str(root / "broker"), PI_TELEMETRY="0", PI_OFFLINE="1",
               CREW_TEST_BIN=str(crew), CREW_PI_ROSTER=str(roster), CREW_PI_EVIDENCE=str(evidence))
    settings = {"defaultTools": ["read"], "sessions": {name: {"enable": name == "messaging"} for name in ("messaging", "subagents", "handoff", "search", "ask", "autoTitle", "hooks")}}
    agent.joinpath("settings.json").write_text(json.dumps(settings))
    server = outsider = None
    requests = []
    request_lock = threading.Lock()

    def last_user(body):
        messages = [m for m in body["messages"] if m["role"] == "user"]
        content = messages[-1]["content"] if messages else ""
        if isinstance(content, list):
            content = "\n".join(p.get("text", "") for p in content)
        return content

    class Provider(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            with request_lock:
                requests.append(body)
                root.joinpath("requests.json").write_text(json.dumps(requests))
            latest = body["messages"][-1]
            text = last_user(body)
            tool = None
            if latest["role"] == "user":
                match = re.search(r"PROBE_SEND ([\w-]+) (PROBE_\w+)", text)
                if match:
                    tool = ("session_send_message", {"session": match[1], "message": "Echo exactly " + match[2] + ". Do not send any messages.", "requestResponse": False})
                elif "PROBE_HOLD" in text:
                    tool = ("crew_probe", {"operation": "hold"})
                elif "PROBE_LIST" in text:
                    tool = ("session_reachable", {})
            delta = {"role": "assistant"}
            if tool:
                delta["tool_calls"] = [{"index": 0, "id": "call_" + uuid.uuid4().hex, "type": "function", "function": {"name": tool[0], "arguments": json.dumps(tool[1])}}]
            else:
                delta["content"] = "ACK " + (text if latest["role"] == "user" else "tool completed")
            chunk = {"id": "chat-test", "object": "chat.completion.chunk", "created": 1, "model": "crew-test", "choices": [{"index": 0, "delta": delta, "finish_reason": None}]}
            end = {**chunk, "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls" if tool else "stop"}]}
            payload = ("data: " + json.dumps(chunk) + "\n\ndata: " + json.dumps(end) + "\n\ndata: [DONE]\n\n").encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    provider = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    model = args.live_model or "crew-test"
    base = args.local_url if args.live_model else f"http://127.0.0.1:{provider.server_port}/v1"
    agent.joinpath("models.json").write_text(json.dumps({"providers": {"crew-test": {"api": "openai-completions", "baseUrl": base, "apiKey": "local-test", "models": [{"id": model, "contextWindow": 32768, "maxTokens": 512, "reasoning": False, "compat": {"supportsDeveloperRole": False}}]}}}))
    subprocess.run(["git", "init", "-q", "-b", "main", str(repo)], env=env, check=True)
    repo.joinpath(".herdr").mkdir()
    repo.joinpath(".herdr/crew.toml").write_text(f'''version = 1
kind = "pi"
start_message = "PROBE_INITIAL. Echo INITIAL_OK and wait."
common_prompt = "This is an isolated messaging test. Follow direct PROBE requests. Use session_send_message only when explicitly asked. Never send acknowledgments to another session."
[pi]
provider = "crew-test"
model = {json.dumps(model)}
thinking = "off"
[workspace]
label = "pi-crew"
[board]
tab = "status"
writer = "lead"
[[roles]]
name = "lead"
prompt = "PROBE_ROLE_LEAD. Your role is lead."
[[roles]]
name = "dev"
prompt = "PROBE_ROLE_DEV. Your role is developer."
''')

    def call(*argv):
        return subprocess.run([herdr, "--session", session, *argv], env=env, cwd=repo, capture_output=True, text=True, timeout=15)

    def result(*argv):
        p = call(*argv)
        assert p.returncode == 0, p.stdout + p.stderr
        value = json.loads(p.stdout)
        return value.get("result", value)

    def wait(label, predicate, seconds=45):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(0.15)
        raise RuntimeError("Timed out: " + label)

    def run_crew(*argv, input=None):
        p = subprocess.run([str(crew), *argv], env=env, cwd=repo, input=input, capture_output=True, text=True, timeout=90)
        assert p.returncode == 0, p.stdout + p.stderr
        return p.stdout

    def panes():
        ws = next(w for w in result("workspace", "list")["workspaces"] if w["label"] == "pi-crew")
        tabs = result("tab", "list", "--workspace", ws["workspace_id"])["tabs"]
        labels = {t["tab_id"]: t["label"] for t in tabs}
        return {labels[p["tab_id"]]: p for p in result("pane", "list", "--workspace", ws["workspace_id"])["panes"]}

    def events(role):
        path = evidence / (ids[role] + ".jsonl")
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def entries(role):
        starts = [e for e in events(role) if e["event"] == "start"]
        if not starts:
            return []
        path = pathlib.Path(starts[-1]["file"])
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def received(role, token):
        return any(
            (e.get("type") == "custom_message" and token in json.dumps(e.get("content")))
            or (e.get("customType") == "pi-sessions.message_received" and token in e.get("data", {}).get("body", ""))
            for e in entries(role)
        )

    def echoed(role, token):
        return any(e.get("message", {}).get("role") == "assistant" and token in json.dumps(e["message"].get("content")) for e in entries(role))

    def settled(role):
        values = [e for e in events(role) if e["event"] in ("before", "settled")]
        return bool(values) and values[-1]["event"] == "settled" and values[-1]["idle"]

    def send_request(role, message):
        run_crew("send", role, input=message)

    def peer_send(sender, target, token):
        send_request(sender, f"PROBE_SEND {ids[target]} {token}. Call session_send_message with session={ids[target]}, message='Echo exactly {token}. Do not send any messages.', requestResponse=false. Then stop.")
        wait("recipient processed " + token, lambda: received(target, token) and echoed(target, token))
        wait("both roles settled", lambda: settled(sender) and settled(target))

    def start():
        nonlocal server
        server = subprocess.Popen([herdr, "--session", session, "server"], env=env, cwd=repo, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
        wait("Herdr server", lambda: call("workspace", "list").returncode == 0)
        assert result("status", "server", "--json")["socket"] == str(socket)

    def stop():
        nonlocal server
        if server:
            call("session", "stop", session)
            try:
                server.wait(timeout=8)
            except subprocess.TimeoutExpired:
                server.terminate()
                server.wait(timeout=5)
            server = None

    master = slave = None
    try:
        run_crew("pi-install")
        integration = pathlib.Path.home() / ".pi/agent/extensions/herdr-agent-state.ts"
        agent.joinpath("extensions/herdr-agent-state.ts").write_bytes(integration.read_bytes())
        start()
        print(run_crew("up", "--no-attach"), flush=True)
        wait("initial role replies", lambda: all(settled(r) for r in ("lead", "dev")))
        initial = panes()
        assert set(initial) == {"lead", "dev", "status"}, initial
        master, slave = pty.openpty()
        outsider = subprocess.Popen([pi, "--approve", "--offline", "--no-extensions", "--no-mcp", "--no-skills", "--no-context-files", "--no-prompt-templates", "--session-id", ids["foreign"], "--provider", "crew-test", "--model", model, "-e", str(package / "extensions/pi-sessions.ts"), "-e", str(source / "tests/support/pi_probe.ts"), "PROBE_INITIAL. Echo FOREIGN_OK and wait."], env=env, cwd=foreign, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        slave = None
        threading.Thread(target=lambda: drain(master), daemon=True).start()
        wait("foreign session live", lambda: settled("foreign"))
        os.write(master, b"PROBE_LIST. Call session_reachable once, then stop.\r")
        wait("unscoped foreign directory", lambda: any(e["event"] == "tool_end" and e["name"] == "session_reachable" for e in events("foreign")))
        raw = next(e for e in reversed(events("foreign")) if e["event"] == "tool_end" and e["name"] == "session_reachable")["result"]["details"]["sessions"]
        assert {s["sessionId"] for s in raw} == {ids["lead"], ids["dev"]}, raw
        send_request("lead", "PROBE_LIST. Call session_reachable once, then stop.")
        wait("scoped directory", lambda: any(e["event"] == "tool_end" and e["name"] == "session_reachable" for e in events("lead")))
        listed = next(e for e in reversed(events("lead")) if e["event"] == "tool_end" and e["name"] == "session_reachable")["result"]["details"]["sessions"]
        assert [s["sessionId"] for s in listed] == [ids["dev"]], listed
        wait("lead settled", lambda: settled("lead"))
        peer_send("lead", "dev", "PROBE_TO_DEV")
        peer_send("dev", "lead", "PROBE_TO_LEAD")
        send_request("dev", "PROBE_HOLD. Call crew_probe with operation='hold', then stop.")
        wait("dev busy tool", lambda: evidence.joinpath("busy").exists())
        send_request("lead", f"PROBE_SEND {ids['dev']} PROBE_BUSY. Call session_send_message with session={ids['dev']}, message='Echo exactly PROBE_BUSY. Do not send any messages.', requestResponse=false. Then stop.")
        wait("busy recipient accepted message", lambda: received("dev", "PROBE_BUSY"))
        assert not echoed("dev", "PROBE_BUSY")
        evidence.joinpath("release").touch()
        wait("busy message processed", lambda: echoed("dev", "PROBE_BUSY") and settled("dev") and settled("lead"))
        send_request("lead", f"PROBE_SEND {ids['foreign']} PROBE_DENIED. Call session_send_message with session={ids['foreign']}, message='PROBE_DENIED', requestResponse=false. Do not retry.")
        wait("outsider rejected", lambda: any(e["event"] == "tool_end" and e["name"] == "session_send_message" and e.get("isError") for e in events("lead")))
        denied = next(e for e in reversed(events("lead")) if e["event"] == "tool_end" and e["name"] == "session_send_message" and e.get("isError"))
        assert "Recipient is outside this live crew roster" in json.dumps(denied["result"]), denied
        wait("lead settled", lambda: settled("lead"))
        assert not received("foreign", "PROBE_DENIED")
        refs = {r: initial[r]["agent_session"] for r in ("lead", "dev")}
        stop()
        start()
        wait("restored processes", lambda: all(sum(e["event"] == "start" for e in events(r)) == 2 for r in refs))
        assert {r: panes()[r]["agent_session"] for r in refs} == refs
        for role in refs:
            restarted = [e for e in events(role) if e["event"] == "start"][-1]
            assert "crew_probe" in restarted["tools"], restarted
        peer_send("lead", "dev", "PROBE_AFTER_RESTART")
        peer_send("dev", "lead", "PROBE_RETURN_AFTER_RESTART")
        for role in refs:
            for event in (e for e in events(role) if e["event"] == "before"):
                # This observer runs after the crew handler; Pi forwards its prompt update.
                assert "PROBE_ROLE_" + role.upper() in event["systemPrompt"], event
        # The controlled provider also records what the model actually receives after tools.
        for request in requests:
            if any("PROBE_INITIAL. Echo FOREIGN" in json.dumps(m) for m in request["messages"]):
                continue
            systems = "\n".join(m["content"] for m in request["messages"] if m["role"] in ("system", "developer"))
            assert ("PROBE_ROLE_LEAD" in systems) != ("PROBE_ROLE_DEV" in systems), systems
        report = {"pi": subprocess.check_output([pi, "--version"], env=env, text=True).strip(), "node": version, "package": "pi-sessions 0.20.0", "model": model, "live": bool(args.live_model), "ids": ids, "checks": ["separate Herdr role tabs", "scoped discovery", "foreign UUID rejected", "bidirectional idle delivery", "busy delivery", "same session refs after restart", "saved active tool restored", "bidirectional delivery after restart", "role prompt on every turn"]}
        root.joinpath("report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps(report, indent=2), flush=True)
    finally:
        evidence.joinpath("release").touch()
        if sys.exc_info()[0] is not None and server:
            try:
                for role, pane in panes().items():
                    print(role, call("pane", "read", pane["pane_id"], "--source", "recent-unwrapped").stdout[-4000:], flush=True)
            except Exception:
                pass
        stop()
        call("session", "delete", session)
        if outsider:
            outsider.terminate()
            outsider.wait(timeout=5)
        for fd in (master, slave):
            if fd is not None:
                os.close(fd)
        provider.shutdown()
        provider.server_close()


def drain(fd):
    try:
        while os.read(fd, 65536):
            pass
    except OSError:
        pass


if __name__ == "__main__":
    main()
