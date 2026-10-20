# pi.dev support and communication

Investigation updated: 2026-10-19. The installed CLI and package both report
**pi 1.3.0** (`@earendil-works/pi-coding-agent`), released 2026-10-15. The previous
crew compatibility baseline was 1.1.0. This update checks the installed help,
bundled changelog and extension types against the official release notes. The
[isolated experiment](pi-sessions-experiment.md) also verifies Pi 1.3.0 with real
Herdr processes, including messaging, role context and restoration.

## Changes since the 1.1.0 baseline

| Release | Relevant change | Effect on crew |
| --- | --- | --- |
| 1.2.0 (2026-10-12) | `before_agent_start` now also runs for idle custom-message starts; the event distinguishes user and custom messages | The crew handler uses `event.systemPrompt`, not `event.prompt`, so the new union is compatible by inspection |
| 1.2.0 | Fixed custom-message runs losing system-prompt additions after their first tool call | Relevant to keeping the role prompt stable when another extension starts a turn |
| 1.2.0 | Fixed inline image rendering in Herdr | Display correction, separate from role/session binding |
| 1.2.0 | Removed the unreleased experimental server/client/plugin stack | Do not base a new crew transport on those old source-only APIs |
| 1.3.0 (2026-10-15) | Restores tools activated during a saved session, with additive CLI tool selections applied afterward | Relevant to plain `pi --session <file>` recovery; explicit tool selections can still replace the saved set |
| 1.3.0 | Improved provider selection for ambiguous model matches and drops another model's reasoning during model switches | Keep provider/model choices explicit when changing a crew role's model |
| 1.3.0 | GPT-6 API-key thinking changes preserve cache; Kimi Code uses Chat Completions and custom base URLs need `/v1` | Provider-specific behavior, not a new inter-session transport |
| 1.3.0 | Corrected Pi's `openai-codex/gpt-6.1-sol` context metadata from 272K to 500K | Pi's provider metadata changed; the effective model/provider still determines available context |

Sources: [Pi 1.2.0](https://pi.dev/changelog/releases/1.2.0) and
[Pi 1.3.0](https://pi.dev/changelog/releases/1.3.0). These changes do not automatically
put independent crew roles in a shared collaboration tree.

## Extension compatibility and configuration validation

The installed types retain `ExtensionContext.mode`, which Herdr's pi integration
uses to distinguish the interactive TUI from headless runs. The minimum remains
1.1.0; the isolated experiment additionally passes with 1.3.0. This establishes
the tested lifecycle behavior, not compatibility with every optional Pi feature.

The crew extension's `before_agent_start` handler appends the saved role to
`event.systemPrompt` and does not assume a user `prompt` field. It can handle both
`messageType = "user"` and `"custom"` by that contract. It also does not call
`ctx.isIdle()` during startup. The experiment confirms the role prompt on both
user and custom-message turns, including after restoration. See the
[extension event contracts](https://pi.dev/docs/latest/extensions#events-and-concurrency).

Two CLI differences are now handled by crew configuration:

- **Provider requires a model.** An isolated, offline invocation of installed
  1.3.0 with `--provider anthropic` and no model exits 1 with
  `--provider requires --model`. Crew validates active Pi roles after inheritance,
  so defaults may supply the provider and the role may supply the model, or vice
  versa. A missing effective model fails before launch. Partial inactive Pi options
  are preserved; switching to Pi applies the launch requirement.
- **Crew accepts `max`.** Typed options now accept all levels listed by installed
  help: `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`. Pi clamps the request
  to the selected model's capabilities; accepting the flag is not proof that every
  model offers that reasoning level.

Focused tests cover inheritance, inactive settings, switching agents, invalid
thinking values and forwarding the launch flags. The installed package's
`dist/cli/args.js`, `dist/core/extensions/types.d.ts`, and this repository's
`src/agent.rs` / `src/pi/herdr-crew.ts` were inspected. The provider-only CLI probe
used a temporary HOME and `PI_CODING_AGENT_DIR`, offline mode, no extensions/MCP,
and empty stdin; it made no provider request and touched no real credentials.

Pi has project trust controls (`--approve` / `--no-approve`) for loading local
resources. They are separate from per-tool approval or an operating-system sandbox;
enabled tools run with the pi process's permissions. See
[Run Pi safely](https://pi.dev/docs/latest/security).

## Communication between roles

Pi's extension APIs can register tools and add user/custom messages to their current
session. `pi.events` connects extensions; it is not by itself a directory or
transport between independent processes. Do not equate extension `pi.sendMessage`
with Claude `SendMessage` or Codex `collaboration.send_message`. See the
[extension API](https://pi.dev/docs/latest/extensions). An external host can control
a Pi session through [RPC mode](https://pi.dev/docs/latest/rpc), but crew starts
interactive sessions and does not host that protocol.

The optional [pi-sessions package](https://pi.dev/packages/pi-sessions), documented
as **0.20.0** on this investigation date, separates these channels:

| Target | Tools |
| --- | --- |
| Historical sessions | `session_search`, `session_ask` |
| Live user-facing sessions | `session_reachable`, `session_send_message`, `session_cancel` |
| Background subagents | `subagent_launch`, `subagent_list`, `subagent_send`, `subagent_cancel` |
| Subagent to parent | `submit_task_report`; direct parent messaging/cancellation is disallowed |

Incoming messages can activate idle recipients or steer busy ones; owned dormant
workers can be woken. Handoffs use tmux or Ghostty; background workers use tmux,
not native Herdr tabs. The package lists Pi >=0.99.1 and Node >=24 <26. Its host
API can yield peer coordination to a host extension while retaining subagent tools.
These are optional package contracts, not capabilities supplied by pi 1.3.0 or
installed by crew. The [experiment](pi-sessions-experiment.md) scopes the package's
peer tools to an explicit crew UUID/checkout roster and verifies delivery and
restoration. It remains separate from production startup. `session_search` is not
the live-recipient directory.

Today pi roles use `herdr-crew --root <project> send <role>` with message text on
stdin. Recipient resolution stays inside the crew workspace. Bound Codex targets
use their native queue where supported; Claude/pi targets use herdr terminal input.
Successful submission is not a processing receipt. The local helper needs access
to herdr; an environment that blocks that access must report the failure.

The crew extension persists the role name and full rendered prompt in a custom
session entry. It supplies a session name when absent and restores the role prompt
on each turn, including after herdr resumes the session by file. `/new` and `/fork`
carry the role; an unrelated resumed session is left alone. This extension does not
register cross-session messaging tools. See `src/pi/herdr-crew.ts` and `src/pi.rs`.

The [native Codex tree experiment](codex-tree-experiment.md) therefore does not
extend to pi by changing `kind`. A comparable pi mode would need an explicit
extension/package integration and lifecycle tests, including parent reports and
the distinction between background workers and user-visible Herdr roles.

After `up --agent pi`, new conversations get the generated pi communication block
even when their authored prompts still mention Claude or Codex tools. The saved
project prompts and inactive agent options are preserved. Existing conversations
keep their saved context; changing the default does not migrate it or replace live
roles. Pi's provider/model selection is separate from switching the coding agent.

See [Codex](codex-support.md), [Claude](claude-support.md), and the common
[messaging contract](messaging.md). Regression tests should cover legacy prompts,
mixed-agent targets and extras, with no assumption that an optional extension is
installed.
