#!/usr/bin/env python3
"""Stateful, offline subset of the herdr-core CLI (`hc`). Never starts a process.

State lives in $CREW_TEST_STATE/hc.json; every call is appended to hc-calls.jsonl. Typing an
agent command line into a terminal whose shell is in front starts a simulated agent: its
program comes to the foreground and it reports `idle` with the app the real agent reports
(`status_ms` counts reports). `claude --resume <id>` of an id in $CREW_TEST_HC_LOST (comma
separated) exits at once: the next `get` shows it in front, the one after the shell again.
"""

import json
import os
import sys

STATE = os.environ["CREW_TEST_STATE"]
PATH = os.path.join(STATE, "hc.json")
APPS = {"claude": "claude-code", "pi": "pi", "codex": "codex", "agy": "antigravity"}


def load():
    try:
        with open(PATH) as f:
            return json.load(f)
    except FileNotFoundError:
        return {"next": 1, "terminals": []}


def save(state):
    with open(PATH, "w") as f:
        json.dump(state, f, indent=1)


def fail(kind, message, code=3):
    print(json.dumps({"error": {"kind": kind, "message": message}}))
    sys.exit(code)


def find(state, ref):
    for t in state["terminals"]:
        if ref in (t["address"], t.get("name")) and t["state"] == "live":
            return t
    for t in state["terminals"]:
        if t["address"] == ref:
            return t
    fail("not_found", f'no terminal "{ref}"')


def options(args, flags, valued):
    """Splits `args` into options and positionals; `--` ends options."""
    opts, rest, i = {}, [], 0
    while i < len(args):
        a = args[i]
        if a == "--":
            rest.extend(args[i + 1 :])
            break
        if a in valued:
            opts.setdefault(a, []).append(args[i + 1])
            i += 2
        elif a in flags:
            opts[a] = True
            i += 1
        else:
            rest.append(a)
            i += 1
    return opts, rest


def labels_match(t, wanted):
    for kv in wanted:
        k, v = kv.split("=", 1)
        if t["labels"].get(k) != v:
            return False
    return True


def main():
    args = [a for a in sys.argv[1:] if a != "--json"]
    with open(os.path.join(STATE, "hc-calls.jsonl"), "a") as f:
        f.write(json.dumps(args) + "\n")
    injected = os.path.join(STATE, "hc-fail")
    if os.path.exists(injected):
        with open(injected) as f:
            prefix = f.read().strip()
        if " ".join(args).startswith(prefix):
            os.remove(injected)
            print("Error: Operation not permitted", file=sys.stderr)
            sys.exit(1)
    if not os.path.exists(os.path.join(STATE, "hc-online")):
        print("Error: Operation not permitted", file=sys.stderr)
        sys.exit(1)
    state = load()
    cmd, rest = args[0], args[1:]
    if cmd == "run":
        opts, argv = options(
            rest,
            {"--bg", "--no-parent", "--no-notify", "--keep", "--rm", "--outlive-parent"},
            {"--cwd", "--label", "--name", "--on", "--parent", "--timeout"},
        )
        address = f"t{state['next']}"
        state["next"] += 1
        pid = 1000 * state["next"]
        shell = argv[-2] if argv[:1] == ["env"] else os.environ.get("SHELL", "/bin/sh")
        env = [a for a in argv[1:] if "=" in a] if argv[:1] == ["env"] else []
        t = {
            "address": address,
            "pid": pid,
            "argv": argv or [shell, "-l"],
            "env": env,
            "cwd": opts.get("--cwd", [os.getcwd()])[0],
            "labels": dict(kv.split("=", 1) for kv in opts.get("--label", [])),
            "name": opts.get("--name", [None])[0],
            "keep": "--keep" in opts,
            "parent": None if "--no-parent" in opts else os.environ.get("HC_TERMINAL_ID"),
            "foreground": {"pid": pid, "argv": [shell, "-l"], "name": os.path.basename(shell)},
            "status": None,
            "state": "live",
            "created_ms": state["next"],
            "typed": [],
        }
        state["terminals"].append(t)
        save(state)
        print(json.dumps({"address": address, "state": "live", "cwd": t["cwd"]}))
    elif cmd == "ls":
        opts, _ = options(rest, {"--all"}, {"--label"})
        live = [
            t
            for t in state["terminals"]
            if (t["state"] == "live" or "--all" in opts) and labels_match(t, opts.get("--label", []))
        ]
        print(json.dumps({"terminals": [{"address": t["address"], "state": t["state"]} for t in live]}))
    elif cmd == "get":
        t = find(state, rest[0])
        print(json.dumps(t))
        if t["foreground"].pop("exits", False):
            t["foreground"] = {"pid": t["pid"], "argv": t["argv"], "name": os.path.basename(t["argv"][0])}
            save(state)
    elif cmd == "label":
        t = find(state, rest[0])
        for kv in rest[1:]:
            k, v = kv.split("=", 1)
            if v:
                t["labels"][k] = v
            else:
                t["labels"].pop(k, None)
        save(state)
        print(json.dumps(t))
    elif cmd == "send":
        opts, pos = options(rest, {"--force", "--wait"}, {"--if", "--timeout"})
        t, text = find(state, pos[0]), pos[1]
        if t["state"] != "live":
            fail("not_live", f"{t['address']} has finished")
        if (t["status"] or {}).get("state") == "blocked" and "--force" not in opts:
            fail("blocked", f"{t['address']} is blocked")
        t["typed"].append(text)
        program = os.path.basename(text.split(" ", 1)[0].strip("'"))
        # A command typed at the shell's prompt runs in the foreground; an agent also reports.
        if t["foreground"]["pid"] == t["pid"]:
            words = text.split(" ")
            lost = os.environ.get("CREW_TEST_HC_LOST", "").split(",")
            t["foreground"] = {"pid": t["pid"] + 1, "argv": words, "name": program}
            if program == "claude" and "--resume" in words and words[words.index("--resume") + 1] in lost:
                t["foreground"]["exits"] = True
            elif program in APPS:
                state["clock"] = state.get("clock", 0) + 1
                t["status"] = {"app": APPS[program], "state": os.environ.get("CREW_TEST_HC_READY", "idle")}
                t["status_ms"] = state["clock"]
        save(state)
        print(json.dumps({"started": True}))
    elif cmd == "wait":
        t = find(state, rest[0])
        status = (t["status"] or {}).get("state")
        print(json.dumps({"terminal": t["address"], "status": status, "reason": "status" if status else "timeout"}))
    elif cmd == "kill":
        t = find(state, rest[0])
        t["state"] = "finished"
        save(state)
        print(json.dumps({"address": t["address"], "state": "finished"}))
    elif cmd == "rm":
        t = find(state, rest[0])
        state["terminals"].remove(t)
        save(state)
        print(json.dumps({"removed": t["address"]}))
    else:
        fail("usage", f"unsupported command {cmd}", 2)


main()
