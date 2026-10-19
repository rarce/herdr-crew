# Claude Code support and communication

Investigation: 2026-10-19. This records documented capabilities and crew behavior;
it does not claim a new live Claude integration test.

Claude Code supports messaging between independent local sessions using
`ListAgents` and `SendMessage` on supported versions/providers. Discover the target
and check its directory/identity; resolve ambiguous names using the discovered
address rather than guessing. Native delivery can start an idle recipient or reach
a busy one between tool calls. Inbound controls may hold or refuse it. Local
cross-session support requires 2.1.224+ on macOS/Linux and 2.1.234+ on native Windows.
Use `/list-agents` to check availability. These capabilities differ from messaging
only within a subagent tree. See the [official cross-session documentation](https://code.claude.com/docs/en/cross-session-messaging).

Crew passes the role name to herdr's Claude launcher and delivers role instructions
through `--append-system-prompt-file`. A tab label, Claude display name and session
UUID are distinct. Native discovery must confirm the intended recipient, especially
after a rename or when another project uses the same role name. For a Claude peer,
prefer its native channel when available. For a Codex/pi peer use
`herdr-crew --root <project> send <role>`, reading the message from stdin. The helper
resolves inside the crew workspace; it is also the existing local fallback for
unavailable native messaging. Do not use a peer to bypass a permission restriction.

After `up --agent claude`, new roles receive the generated Claude communication
instructions even if their saved prompts mention Codex `collaboration` or pi
extension tools. The generated block owns communication routing; project tasks,
ownership rules and permissions remain intact. Explicit per-role kinds retain
priority. Live roles and recoverable conversations keep their original agent and
context; changing the default does not convert their histories.

The cross-agent correction does not install Claude, enable a provider capability,
change inbound settings, or create agent teams. It also does not assume a native
tool exists just because the selected executable is Claude. Where native discovery
is unavailable, use the generated supported fallback and report delivery failures.

See [Codex](codex-support.md), [pi](pi-support.md), and the common
[messaging contract](messaging.md). Offline regression tests cover generated routing
and selection; a real-agent check is needed to verify a particular installation's
native inbox and permission settings.
