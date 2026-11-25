# Antigravity CLI (`agy`) support and communication

Investigation: 2026-11-23. Verified against installed **agy 1.5.15** and **herdr 0.9.5**,
Antigravity CLI documentation (`antigravity.google/docs`), and bundled Antigravity
customization docs (`hooks.md`, `rules.md`). This records documented capabilities and
offline-tested crew behavior; live end-to-end Herdr + `agy` session restore and model
invocation checks are noted separately below.

## Herdr integration and launch contract

Herdr 0.9.5 natively supports `agy`:

- `herdr agent start <name> --kind agy --pane <id> -- [args...]` starts `agy` and waits for
  interactive readiness.
- `herdr integration install antigravity-cli` installs Herdr's session-tracking hook in
  `~/.gemini/config/hooks/herdr-agent-state.sh` (registered in `~/.gemini/config/hooks.json`).
- On server restart, Herdr restores the tab with `agy --conversation <id>`.

Two CLI differences from Claude Code, Codex and pi shape how crew starts `agy`:

1. **No positional prompts.** An offline check of `agy 1.5.15` (`agy "hello"`) exits 2 with:
   `Prompts are read only from -p/--print, -i/--prompt-interactive, or stdin, so this argument would have been ignored.`
   Crew therefore passes `start_message` via `--prompt-interactive <message>` instead of a
   trailing positional argument.
2. **No `--append-system-prompt-file` flag.** Role instructions are delivered via a
   `SessionStart` lifecycle hook (`herdr-crew agy-install` / `herdr-crew agy-hook`) rather
   than a launch flag or custom agent file.

### Approve each checkout before starting a role

In the reported `agy 1.5.16` startup sequence, an untrusted checkout could send
`--prompt-interactive` before loading even the global `SessionStart` hooks. Approving
trust after the first turn did not replay that event. [Issue #29](https://github.com/rarce/herdr-crew/issues/29)

Crew checks the canonical path of each new `agy` role against `trustedWorkspaces` in
`~/.gemini/antigravity-cli/settings.json`. For each checkout listed by `herdr-crew check`,
`up` or `add`, open plain `agy` in that directory, review and approve the folder in the
CLI, then exit and retry. The main checkout and each role worktree need separate
approval. If a role worktree is missing, run the `git worktree add --detach`
command printed by Crew from the main checkout. Fetch the configured base first
if its local remote-tracking ref is missing. Only the Antigravity CLI writes the
trust approval during this workflow. [Antigravity CLI settings](https://antigravity.google/docs/cli/reference/)

## Role prompt delivery and restoration

`herdr-crew agy-install` requires the public `herdr-crew` launcher on `PATH` and registers a
named `"herdr-crew"` entry in `~/.gemini/config/hooks.json`, alongside Herdr's own `antigravity-cli` hook and any
user hooks. Installation and preflight use this path even when
`ANTIGRAVITY_CLI_CONFIG_DIR` is set, matching the global hooks loaded by agy 1.5.15.

```json
{
  "herdr-crew": {
    "SessionStart": [
      {
        "type": "command",
        "command": "herdr-crew agy-hook",
        "timeout": 20
      }
    ]
  }
}
```

Right before launching an `agy` role (whether via `CreateWorkspace`, `AdoptWorkspace`,
`CreateTab`, or `add`), crew saves the rendered prompt for that repository and pane in
the private directory `~/.herdr-crew-agy/`. A failed launch clears its pending prompt.
When Herdr reports a role with a `start_message` is ready, Crew gives the hook
five seconds to consume the binding. If it is still pending, Crew clears it and
reports an error. Close the named tab, verify checkout trust and hook installation,
then retry. If Herdr reports the role is waiting at startup, Crew warns the operator
and keeps the binding for `SessionStart` after the dialog. Roles without a
`start_message` also keep their binding for the first conversation. If an older
abandoned launch blocks a reused pane, the error identifies the exact file to
remove after the old agent has exited.
On `SessionStart`, `herdr-crew agy-hook` resolves
the repository root from `cwd`, atomically consumes the pending binding for `HERDR_PANE_ID` (on herdr-core, for the terminal's `crew.core` label, where it also records the `conversationId` in the terminal's `crew.session` label so that `up` can resume it with `agy --conversation <id>`),
and returns:

```json
{
  "injectSteps": [
    {
      "systemMessage": {
        "systemMessage": "<rendered role prompt>"
      }
    }
  ]
}
```

Per Antigravity's `hooks.md` contract, an injected `systemMessage` persists in the
conversation trajectory across turns, so a plain `agy --conversation <id>` restore retains
the role prompt. Because the pending binding is atomically deleted on first consumption,
subagents spawned by a coordinator (`agy_tree = true`), resumed conversations,
and non-crew `agy` sessions never receive duplicate or leaked role prompts (they receive `{}`).

Custom agents (`--agent <name>` via `.agents/agents/<name>.md` with `mainAgent: true`) were
considered and not chosen for role prompts: their Markdown body replaces the base system
prompt rather than appending to it, requires writing files inside the repository and
worktrees, and does not cover extra instances (`<role>-2`) cleanly.

## Communication between roles (comparison with Claude Code)

**`agy` does not currently provide Claude Code-style cross-session peer messaging between
independent local CLI sessions.**

| Mechanism in `agy` | Scope | Can address an independent Herdr crew tab? |
| --- | --- | --- |
| `invoke_subagent`, `manage_subagents`, `send_message` | Parent-child subagent tree inside one conversation | **No.** Requires a `conversationId` spawned by this conversation; role names and tab labels are not valid targets. |
| `/swarm` (`swarm` tool) | Coordinator + worker subagents on a shared board inside one session | **No.** Manages workers spawned by the coordinator, not pre-existing independent CLI tabs. |
| `agentapi` (`send-message`) | Internal/sidecar CLI helper scoped to a project/automation config | **No native peer discovery.** Lacks `list-conversations` / `list-peers` discovery for interactive tabs. |

Independent `agy` crew roles therefore communicate through
`herdr-crew --root <project> send <role>` (reading the message from stdin), the same common
channel used by `pi` and mixed-agent pairs. Bound Codex targets receive via native
`codex queue`; `agy`, Claude and `pi` targets receive via Herdr terminal input
(`herdr agent prompt`).

### Terminal sandbox restrictions

When `agy` runs with `--sandbox`, `--mode auto`, or `enableTerminalSandbox: true`, its
OS-level terminal sandbox (macOS `sandbox-exec` / Seatbelt, Linux namespaces) normally
blocks access to local Unix sockets outside the workspace, including the Herdr socket.
In that configuration `herdr-crew send` exits `77` (`EXIT_UNREACHABLE`). Crew preserves
the invariant from [messaging.md](messaging.md): it never widens permissions or bypasses the
sandbox. Generated `agy` prompts state this limitation explicitly and instruct roles to
report actual failures and deliver through commits/output or ask the user to relay.

First-class single-coordinator native tree mode is enabled via `agy_tree = true`
(with `experimental_agy_tree = true` accepted as a backward-compatible alias; see below
and `tests/fixtures/crew-agy-tree.toml`), analogous to
[codex-tree-experiment.md](codex-tree-experiment.md).

## Single-coordinator native mode (`agy_tree = true`) — Recommended for `agy`-only crews

When `agy_tree = true` is set in `.herdr/crew.toml` (or legacy alias `experimental_agy_tree = true`;
specifying both keys with contradictory values is rejected):

- Configuration validation requires **exactly one `agy` coordinator role** without `worktree`
  or `extra` (and cannot be combined with `experimental_codex_tree`). `herdr-crew config`
  also refuses changing the coordinator's agent away from `agy` while `agy_tree` is active.
- `herdr-crew init` and first-time `herdr-crew up` support `--topology tree|tabs`:
  - For multi-role `agy` presets (`review`, `parallel`, `research`), `--yes` defaults to
    `tree` and the interactive wizard (TUI and `--no-tui`) offers a `Topology` step
    defaulting to `tree`.
  - `--topology tree` is valid only with `--agent agy` and a multi-role preset (rejected
    with other agents, `--preset solo`, or `--base`).
  - Tree mode requires no Git remote and emits no `[worktrees]` section; the `Models` step
    configures only `<name>-lead`.
  - In `review`, isolation chooses between isolated native worktrees (`Workspace: "share"`,
    default) and shared checkout (`Workspace: "inherit"`, via `--shared-checkout`). In
    `parallel`, `dev-a` and `dev-b` default to `Workspace: "share"`.
  - `--topology tabs` preserves the independent multi-tab configuration.
- `prompt::render` emits the `Crew runtime communication (agy native tree):` block: the
  coordinator delegates manifest workers via `invoke_subagent` (`TypeName: "self"` for
  implementers, `"research"` for read-only reviewers/researchers; `Workspace: "share"` or
  `"inherit"`), reuses returned `conversationId`s via `send_message`, and inspects active
  workers via `manage_subagents (Action: "list")`.
- **No Herdr socket access is needed** for worker communication, so `agy`'s default `auto`
  mode and terminal sandbox work with zero friction.
- **Post-restore recovery (`agy --conversation <id>`):** After a session restore, the
  coordinator's runtime instructions require checking `manage_subagents (Action: "list")`
  first and reusing any surviving worker `conversationId` via `send_message` before
  considering a replacement, and reporting recovery failures to the user instead of
  spawning duplicate workers.

```toml
version = 1
kind = "agy"
agy_tree = true
start_message = "Confirm your coordinator role in one line and wait for an explicit task before changing files."

[agy]
model = "gemini-4-neon"
effort = "high"

[workspace]
label = "my-crew"

[board]
tab = "my-crew-status"
writer = "my-crew-lead"

[[roles]]
name = "my-crew-lead"
prompt = '''You are {{NAME}} in {{REPO}}. Delegate to manifest workers (dev, reviewer) via invoke_subagent.'''
```

## Live experiment results (2026-11-23, `agy 1.5.15` + `herdr 0.9.5`)

Both modes were tested in an isolated Herdr server and temporary repository with
`gemini-4-hydrogen` at `effort = "low"`:

- **Mode 1 (Independent Herdr tabs + `herdr-crew send`):**
  - `herdr-crew up --no-attach` launched 2 `agy` tabs (`exp-lead`, `exp-dev`) and the board
    tab in `10.7 s` (`auto · Gemini 4 Hydrogen · low`).
  - `herdr integration install antigravity-cli` captured both `agy` conversation UUIDs
    (`source: "herdr:antigravity_cli"`), detected `blocked` on the folder-trust prompt, and
    `herdr-crew send` safely refused with exit `76` while blocked.
  - Once idle, `herdr-crew send exp-dev` delivered the framed message into `agy`'s TUI and
    received `MODE1_DEV_ACK` in `4.0 s`.
  - **Hook discovery findings:** `agy 1.5.15` reads global hooks from `~/.gemini/config/hooks.json`
    (ignoring `ANTIGRAVITY_CLI_CONFIG_DIR` for hook loading) and workspace hooks from
    `.agents/hooks.json` (which are skipped until the folder is trusted). The crew's
    `SessionStart` hook consumes a one-shot binding in `~/.herdr-crew-agy/` and injects
    the role prompt as a `systemMessage`. Ensure `herdr-crew` is on `PATH` so the hook
    can invoke it.
  - **Sandbox friction:** `agy`'s default `auto` mode enables the terminal sandbox, blocking
    an `agy` role from calling `herdr-crew send` over the local Herdr socket unless allowed
    outside the sandbox.
- **Mode 2 (Single coordinator + native `invoke_subagent` / `send_message`):**
  - Full round-trip (spawn `exp-dev` → receive `MODE2_FIRST_ACK` → follow-up via
    `send_message` → receive `MODE2_SECOND_ACK` → coordinator summary) succeeded in `22.0 s`
    with zero approval friction, zero sandbox socket blocks, and automatic reactive wakeup.

**Recommendation:** Use `agy_tree = true` (default for multi-role `agy` in `init`, or `--topology tree`) for `agy`-only crews when you want
Claude-like native messaging fluidity; use independent tabs (`--topology tabs`) when you need separate Herdr
tabs per role or mixed-agent crews (`agy` + `claude`/`codex`/`pi`).

## Launch options and models

`[agy]` and `[roles.agy]` accept typed launch options:

- `model` → `--model <value>`
- `effort` → `--effort <low|medium|high|xhigh|max>`
- `mode` → `--mode <accept-edits|auto|plan>`
- `sandbox` → `--sandbox` (when `true`)
- `additional_dirs` → repeatable `--add-dir <path>` (resolved relative to project root)

`agy models` (1.5.15) publishes effort-suffixed slugs (e.g. `gemini-4-neon-high`), while
1.5.x also accepts base model names with `--effort` (e.g. `--model gemini-4-neon --effort high`).
Crew's profiles and catalog use base model names so `model` and `effort` stay orthogonal;
configuration validation rejects setting `effort` alongside a model slug that already carries
an effort suffix or an `orchestrator` model (which disables manual effort overrides).

See [models.md](models.md) for tier mappings (`gemini-4-argon`, `gemini-4-neon`,
`gemini-4-hydrogen`, `gemini-3.8-flash`) and [messaging.md](messaging.md) for the common
messaging contract.

## Live checks still recommended per installation

Offline unit tests cover configuration, option validation, hook merging, idempotent prompt
injection, `--prompt-interactive` launch flags, recovery hints and peer routing. On a live
machine with `agy` and `herdr`, verify:

1. `herdr integration install antigravity-cli` and `herdr-crew agy-install` are both active.
2. `agy --model <base> --effort <level>` matches your account's model entitlement.
3. Restored sessions (`agy --conversation <id>`) retain the injected role prompt and check
   whether model/effort revert to `settings.json` defaults after a Herdr restart.
4. Terminal input delivery (`herdr-crew send`) while an `agy` recipient is `working` vs.
   `blocked` at an approval dialog.
