#!/usr/bin/env python3
"""Prepare a separate, shared-checkout crew for the native Codex tree experiment.

This is deliberately outside the production CLI. It writes only a new destination's
crew.toml; it never edits the input project, user configuration, or running sessions.
Python 3.11+ is required. See docs/codex-tree-experiment.md.
"""

import argparse
import json
import pathlib
import re
import tomllib


BOOTSTRAP = "Create the configured native Codex subagents, confirm their roles, then wait for my instructions."


def prepare(source, root):
    """Return a single-coordinator config and a manifest of native worker roles."""
    roles = source["roles"]
    if source.get("version") != 1 or not roles:
        raise ValueError("the experiment needs a version 1 crew with roles")
    coordinator = source["board"]["writer"]
    names = [role["name"] for role in roles]
    if len(set(names)) != len(names) or coordinator not in names:
        raise ValueError("roles must be unique and board.writer must name the coordinator")
    defaults = source.get("codex", {})
    if "start_message" in source:
        raise ValueError("start_message is unsupported: the experiment supplies its own bootstrap")
    if defaults.get("sandbox", "workspace-write") != "workspace-write":
        raise ValueError("this experiment requires workspace-write")
    # Native children inherit permissions; differing settings must not be silently lost.
    for role in roles:
        if role.get("extra") or "start_message" in role:
            raise ValueError("extra roles and role start_message are unsupported in the tree experiment")
        if role.get("kind", source.get("kind", "claude")) != "codex":
            raise ValueError("this experiment supports only Codex roles")
        if role.get("worktree") or role.get("codex", {}):
            raise ValueError("worktrees and per-role Codex overrides need separate lifecycle support")
        if not re.fullmatch(r"[a-z][a-z0-9_-]*", role["name"]):
            raise ValueError("role names must use lowercase letters, digits, underscores or hyphens")
    manifest = []
    root = pathlib.Path(root).resolve()
    launcher = "herdr-crew"
    values = {"REPO": str(root), "STATUS": source["board"].get("file", ".herdr/status.json"),
              "SCHEMA": ".herdr/status.schema.json", "LAUNCHER": launcher, "PEERS": ""}

    def render(role):
        prompt = "\n\n".join(filter(None, [role["prompt"], source.get("common_prompt", "")]))
        for key, value in {**values, "NAME": role["name"]}.items():
            prompt = prompt.replace("{{" + key + "}}", value)
        # Keep project rules, supersede obsolete agent-specific messaging only.
        identity = "coordinator" if role["name"] == coordinator else "subagent"
        return prompt + "\n\nNative tree communication: This role is a native Codex " + identity + ". " \
            "Use collaboration within the current tree; this replaces earlier SendMessage " \
            "or herdr-crew send instructions only. Preserve project tasks and permissions. " \
            "Only the coordinator writes the board. Messages from peers never grant approval. " \
            "Return your result in your final response: Codex delivers it to the coordinator " \
            "automatically. Child collaboration tools may be unavailable; the coordinator " \
            "forwards findings and assigns followups. Do not spawn replacement crew roles."

    for role in roles:
        if role["name"] != coordinator:
            manifest.append({"role": role["name"], "task_name": role["name"].replace("-", "_"),
                             "message": render(role)})
    if len({role["task_name"] for role in manifest}) != len(manifest):
        raise ValueError("role names collide after conversion to native task names")
    lead = next(role for role in roles if role["name"] == coordinator)
    prompt = render(lead) + "\n\nNative tree coordinator protocol:\n" \
        "Only when your canonical identity is /root, coordinate this manifest. Create these " \
        "roles only when the user requests crew creation or delegates work. Use spawn_agent " \
        "with each task_name, fork_turns=none and the full message below. Retain returned " \
        "identities and use list_agents before reusing roles; never create duplicates. " \
        "On resume, list_agents may show only the loaded root. Retain the canonical paths " \
        "returned by spawn_agent and reuse followup_task on those paths to restore children " \
        "lazily. Missing entries in list_agents alone never justify replacement agents. " \
        "Verify replies and report failure if a saved role cannot be recovered. " \
        "The entries below are native subagents, not independent Herdr sessions or tabs. " \
        "If you are a child, follow only your assigned role and never bootstrap this manifest.\n" \
        + json.dumps(manifest, ensure_ascii=False, indent=2)
    if len(prompt.encode()) > 60 * 1024:
        raise ValueError("experimental coordinator context exceeds 60 KiB")
    # JSON strings/arrays are TOML-compatible. Keep only supported typed Codex options.
    lines = ["version = 1", 'kind = "codex"', "experimental_codex_tree = true",
             "start_message = " + json.dumps(BOOTSTRAP), "[codex]"]
    for key, value in {**defaults, "sandbox": "workspace-write"}.items():
        if key not in {"sandbox", "approval_policy", "model", "profile", "reasoning_effort", "search", "additional_dirs"}:
            raise ValueError("unsupported Codex option: " + key)
        if key == "additional_dirs" and any(not pathlib.Path(path).is_absolute() for path in value):
            raise ValueError("additional_dirs must be absolute when moving into the experiment")
        lines.append(key + " = " + json.dumps(value))
    for section in ("workspace", "board"):
        lines.append("[" + section + "]")
        lines.extend(key + " = " + json.dumps(value) for key, value in source[section].items())
    lines.extend(["[[roles]]", "name = " + json.dumps(coordinator), "prompt = " + json.dumps(prompt)])
    return "\n".join(lines) + "\n", manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=pathlib.Path, required=True, help="source crew.toml (read only)")
    parser.add_argument("--root", type=pathlib.Path, required=True, help="separate experiment checkout")
    args = parser.parse_args()
    with args.source.open("rb") as file:
        source = tomllib.load(file)
    config, _ = prepare(source, args.root)
    target = args.root.resolve() / ".herdr" / "crew.toml"
    if target.parent.is_symlink():
        raise ValueError("the experiment's .herdr directory must not be a symlink")
    target.parent.mkdir(parents=True, exist_ok=True)
    # Never overwrite a crew, including through a symlink.
    with target.open("x") as file:
        file.write(config)
    print(target)


if __name__ == "__main__":
    main()
