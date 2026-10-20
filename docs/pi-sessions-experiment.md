# Pi sessions in separate Herdr tabs

This opt-in experiment keeps Herdr responsible for the role tabs and process
lifecycle. Two independent Pi conversations communicate through the optional
`pi-sessions` 0.20.0 extension's peer tools. A shared parent conversation is not
required. Production `up`, `send` and `pi-install` do not enable this package.

## Scope and isolation

`experiments/pi_sessions.py` creates a temporary Git checkout, a named Herdr server,
Pi settings, session files, extension loader and broker under `/tmp`. It loads the
crew extension and a copy of the already-installed Herdr state extension explicitly.
It never loads personal Pi settings, authentication or other extensions. It uses
`--approve` only for the disposable folders it creates.

`experiments/pi_sessions_scope.ts` wraps the package's public tool registrations:

- `session_reachable` lists only UUIDs in the explicit test crew roster whose live
  checkout matches the roster.
- `session_send_message` revalidates the caller, live recipient UUID and checkout
  on every invocation, including after restart.
- `session_cancel` stays inactive. Handoffs, subagents, search, auto-titling and
  background model features are disabled in the temporary settings.

The adapter does not claim the package's host API: a peer-claiming host suppresses
the very transport tools used by this experiment. It wraps their registration
instead. This is an experimental adapter coupled to 0.20.0, not a stable production
integration or an access boundary against other extensions. See the
[package's host and messaging contracts](https://pi.dev/packages/pi-sessions).

Herdr starts and restores the two role TUIs in separate tabs. The package does not
create tmux/Ghostty panes in this configuration. A third standalone Pi conversation
in another checkout shares the broker so the test can verify recipient filtering.

## Run

Requirements: Unix, Python 3, Herdr 0.9.5 with its Pi integration installed, Pi
1.3.0, and Node 24 or 25. The package requires Node >=24 <26; a global Node 26
installation is not suitable for this experiment. Pass a supported binary explicitly.

Unpack the pinned package into a scratch directory without installing it globally:

```sh
mkdir -p /tmp/crew-pi-deps/sessions
npm pack pi-sessions@0.20.0 --ignore-scripts --pack-destination /tmp/crew-pi-deps
tar -xzf /tmp/crew-pi-deps/pi-sessions-0.20.0.tgz -C /tmp/crew-pi-deps/sessions
cargo build --locked

python3 experiments/pi_sessions.py target/debug/herdr-crew \
  --package /tmp/crew-pi-deps/sessions/package \
  --node /absolute/path/to/node24
```

The default uses real Pi and Herdr with a deterministic local HTTP provider. It
makes no external model requests and uses no credentials. Socket/PTY operations
require an environment that allows them. The script prints its evidence directory,
retains it for inspection and stops/deletes only its own named Herdr session.

For a real local model already serving an OpenAI-compatible API:

```sh
python3 experiments/pi_sessions.py target/debug/herdr-crew \
  --package /tmp/crew-pi-deps/sessions/package \
  --node /absolute/path/to/node24 \
  --live-model Qwen3.8-27B-OptiQ-4bit \
  --local-url http://127.0.0.1:8000/v1
```

Live mode accepts loopback endpoints only and has no remote fallback. Both modes
use the same delivery/restoration assertions. `herdr-crew send` injects the test
requests into a role; the role then calls `session_send_message`, and the test
verifies the recipient's persisted incoming entry and assistant response. The peer
delivery being tested does not use terminal typing.

Run the adapter's independent checks with Node 24/25:

```sh
/absolute/path/to/node24 tests/support/test_pi_sessions_scope.mjs
```

## Evidence and limits

Verified with Pi 1.3.0, Herdr 0.9.5, Node 24.22.0 and pi-sessions 0.20.0:

- Separate `lead`, `dev` and status tabs in Herdr.
- Idle messages in both directions, with the recipient actually responding.
- Accepted delivery while `dev` runs a held tool; the response follows release.
- Scoped discovery and rejection of a foreign session UUID.
- The same native session references after a cold Herdr server restart, followed
  by successful messages in both directions.
- A tool activated at runtime survives restoration without being reactivated by
  the fresh process. Role prompts remain present on every observed user/custom turn.

The real-model run also passed with local `Qwen3.8-27B-OptiQ-4bit` served by oMLX.
Its report is `/private/tmp/cp6uo5o6cx/report.json`; the deterministic run's report
is `/private/tmp/cpctlrz3fo/report.json`. Test probe events and native Pi JSONL
sessions are retained alongside them. The independent
adapter test also checks a live UUID in the wrong checkout, a disappeared recipient
and a stale caller binding.

This experiment supplies a fixed roster and launch wrapper. Shipping it in crew
still needs roster generation from real role bindings, handling `/new` and `/fork`,
extras/worktrees and mixed-agent recipients, package/version setup, and deciding
how the optional tools appear in the generated role instructions. The current
`herdr-crew send` path remains available.
