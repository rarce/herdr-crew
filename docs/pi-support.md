# pi.dev support and communication

Investigation: 2026-10-19. Documentation was inspected; no new live pi model
conversation was run for this investigation. Crew tests pi 1.1.0+ for the herdr
session-reporting contract, not every newer extension feature.

Pi's extension APIs can register tools and add user/custom messages to their current
session. `pi.events` connects extensions; it is not by itself a directory or
transport between independent processes. Do not equate extension `pi.sendMessage`
with Claude `SendMessage` or Codex `collaboration.send_message`. See the
[extension API](https://pi.dev/docs/latest/extensions). An external host can control
a Pi session through [RPC mode](https://pi.dev/docs/latest/rpc), but crew starts
interactive sessions and does not host that protocol.

The optional [pi-sessions package](https://pi.dev/packages/pi-sessions) documents
`session_search` for live discovery, `session_send_message` for live sessions/owned
subagents, and `session_cancel`. Its delivery can activate an idle live session or
steer a busy one; dormant owned subagents have separate resume behavior. These are
package capabilities, not tools crew installs or can assume every pi role has.
Native adoption would require checking installation, exact recipient identity,
delivery behavior and restoration before adding it as a supported transport.

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

After `up --agent pi`, new conversations get the generated pi communication block
even when their authored prompts still mention Claude or Codex tools. The saved
project prompts and inactive agent options are preserved. Existing conversations
keep their saved context; changing the default does not migrate it or replace live
roles. Pi's provider/model selection is separate from switching the coding agent.

See [Codex](codex-support.md), [Claude](claude-support.md), and the common
[messaging contract](messaging.md). Regression tests should cover legacy prompts,
mixed-agent targets and extras, with no assumption that an optional extension is
installed.
