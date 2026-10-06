#!/usr/bin/env python3
"""Offline Codex protocol simulator. Never contacts a model or reads real credentials."""
import json
import os
import pathlib
import sys

args = sys.argv[1:]
if args == ["--version"]:
    print("codex-cli 0.160.1")
elif "app-server" in args:
    if "--profile" in args and args[args.index("--profile") + 1] == "missing":
        print("configuration profile missing does not exist", file=sys.stderr)
        raise SystemExit(1)
    for line in sys.stdin:
        request = json.loads(line)
        if request["method"] == "initialize":
            result = {"userAgent": "crew-offline-test"}
        elif request["method"] == "initialized":
            continue
        elif request["method"] == "hooks/list":
            result = {"data": [{
                "cwd": cwd, "errors": [], "warnings": [], "hooks": [{
                    "command": "herdr-crew codex-hook", "handlerType": "command",
                    "eventName": "sessionStart", "enabled": True,
                    "trustStatus": os.environ.get("CREW_TEST_TRUST", "trusted"),
                    "currentHash": "offline-test", "async": False,
                    "sourcePath": str(pathlib.Path(os.environ["CODEX_HOME"]) / "hooks.json"),
                    "matcher": "startup|resume|clear|compact", "additionalContextLimit": 0,
                }],
            } for cwd in request["params"]["cwds"]]}
        else:
            raise RuntimeError("unexpected model or thread request: " + request["method"])
        print(json.dumps({"id": request["id"], "result": result}), flush=True)
elif args and args[0] == "resume" and not os.environ.get("CREW_TEST_NATIVE"):
    pathlib.Path(os.environ["CREW_TEST_STATE"], "codex-resume.json").write_text(json.dumps({
        "args": args, "cwd": os.getcwd(), "home": os.environ.get("CODEX_HOME"),
        "binding": os.environ.get("CREW_CODEX_BINDING"),
        "inherited_thread": os.environ.get("CODEX_THREAD_ID"),
    }))
elif os.environ.get("CREW_TEST_NATIVE"):
    import socket
    import subprocess
    import time

    phase = os.environ["CREW_TEST_PHASE"]
    pane = os.environ["HERDR_PANE_ID"]
    directory = pathlib.Path(os.environ["CREW_TEST_STATE"])
    session_id = args[-1] if args and args[0] == "resume" else "00000000-0000-0000-0000-000000000001"
    print("OpenAI Codex\nResuming session", flush=True)

    def native_report():
        request = {"id": "native-test", "method": "pane.report_agent_session", "params": {
            "pane_id": pane, "source": "herdr:codex", "agent": "codex", "seq": time.time_ns(),
            "agent_session_id": session_id, "session_start_source": "resume" if args and args[0] == "resume" else "startup",
        }}
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
            client.settimeout(2)
            client.connect(os.environ["HERDR_SOCKET_PATH"])
            client.sendall((json.dumps(request) + "\n").encode())
            client.recv(8192)

    native_report()
    event = {"hook_event_name": "SessionStart", "session_id": session_id, "cwd": os.getcwd(),
             "source": "resume" if args and args[0] == "resume" else "startup"}
    hook = subprocess.run(["herdr-crew", "codex-hook"], input=json.dumps(event), text=True, capture_output=True, timeout=20)
    directory.joinpath(phase + "-hook.json").write_text(hook.stdout)
    directory.joinpath(phase + "-args.json").write_text(json.dumps(args))
    native_report()  # A later native hook must preserve the crew's reported command.
    print("\x1b[2J\x1b[HOpenAI Codex\n› Ask Codex to do anything", flush=True)
    while True:
        time.sleep(1)
else:
    raise RuntimeError("unexpected Codex invocation: " + repr(args))
