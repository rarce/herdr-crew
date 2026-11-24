# herdr-core + Herdr app support

Investigation: 2026-11-24. Checked against these installed versions: **herdr-core 0.1.21**
(`hc`), **Herdr app 0.1.16** (`herdr-app`) and **Herdr TUI 0.9.5**. Sources:

- `hc --help`, `hc <cmd> --help`, `hc --skill`, `hc schema` (OpenRPC).
- `herdr-app --skill`, `herdr-app commands`, and the guide the app installs at
  `~/.config/herdr-app/AGENTS.md`.
- The live core state under `~/.local/state/herdr-core/default/`.
- Public docs: <https://herdr.dev/>, <https://herdr.dev/core/>,
  <https://github.com/herdrdev/herdr-core> (README, `docs/commands.md`, `templates.md`,
  `machines.md`, `security.md`, `schema/README.md`, `CHANGELOG.md`) and
  <https://github.com/herdrdev/herdr-app> (README, `docs/labels.md`, `mods.md`,
  `commands.md`).

This is research. Nothing in herdr-crew uses herdr-core yet.

## Context: the TUI is being replaced

Herdr TUI (`herdr`, the host of our `herdr-plugin.toml`) has been replaced by **herdr-core
+ the Herdr app**. The public pages do not say so yet: the herdr.dev FAQ still says
"Herdr TUI keeps working". But the TUI's last release is 0.9.5 (14 Oct, a hotfix), while
core shipped 0.1.10 → 0.1.22 between 2026-10-23 and 2026-11-22. `hc --skill` already
describes `herdr` as "the older terminal multiplexer". We should treat the TUI as frozen and
plan to move.

The two stacks share nothing: "They don't share agents, so what runs in one doesn't show up
in the other" (herdr.dev). A crew started on the TUI is invisible to the app.

## The new architecture

| Piece | Role | Interface |
|---|---|---|
| **herdr-core** (`hc`) | Runs terminals and keeps them alive. Draws nothing. | CLI `hc`, or JSON-RPC 2.0 over `<state>/<session>.sock` (`hc schema`) |
| **Herdr app** (`herdr-app`) | Window: spaces > tabs > panes. Built on core. | `herdr-app do <command>` (needs an open window), mods (trusted JS), layouts |

### herdr-core concepts

- **Terminal**: a persistent PTY. It has an address (`t7`, `vps/t7`) and an optional
  `--name` that is unique among live terminals (`a-z 0-9 - _`, at most 32 chars). JSON
  fields: `cwd`, `argv`, `foreground {argv, cwd, pid}`, `state` (live/finished),
  `status {app, state}`, `turn`, `labels`, `parent`, `keep`, `first_message`.
- **Status** is reported by the program with OSC 7501
  (`printf '\e]7501;state=working:app=x\e\\'`). The states are `idle`, `working`, `done`,
  `blocked` and `error`. "hc never infers status from the screen." A program that reports
  nothing has no status. Observed: Claude Code 2.1.x reports `app=claude-code` under core
  without the TUI hook (`~/.claude/hooks/herdr-agent-state.sh` only acts when
  `HERDR_SOCKET_PATH` is set). The core binary contains no Claude detection. The spike
  below confirms that Codex, pi and agy report status natively too.
- **Labels** are free `k=v` pairs (`hc run --label`, `hc label`). `hc ls --label`,
  `hc events --label` and `events.subscribe` filter on them. No reserved namespace is
  documented, except the app's `herdr.*` labels.
- **Parent/child**: a terminal started from inside another hc terminal becomes its child.
  It **dies with its parent** unless started with `--outlive-parent` or `--no-parent`, and
  it types notes into the parent unless started with `--no-notify`.
- **Sending text**: `hc send <t> <text>` pastes the text and presses Enter. It waits up to
  60 s if a person has an unfinished draft. `--if idle|working` sends only in that state.
  It refuses on `blocked` unless `--force`. `hc keys`, `hc interrupt`, `hc wait`.
- **Templates**: `~/.config/herdr-core/templates/<name>.md`. The frontmatter has
  `command` (required), `description`, `ready` (`status` | `quiet`), `submit`, `stop` and
  `paste_gap_ms`. The body is the opening instructions, and core appends the task after a
  blank line. Templates are user-global and have no cwd, env or labels fields.
- **First message**: `hc run <cmd> "<task>"` waits for readiness (the first OSC 7501
  report, up to 60 s) and then sends the task. It records `first_message.sent` and the
  `reason` if it was not sent.
- **Keep and restore**: `--keep` replaces a lost program (core crash, reboot, logout) with
  a **shell in its last folder, at the same address**. "The original command is never
  re-run." There is **no agent resume**.
- **Documents**: named JSON values with versions (`doc.get` / `doc.set`, compare-and-set,
  up to 1 MiB each). They are API only, with no CLI verb. The app keeps its whole layout in
  the doc `herdr-app` (spaces > tabs > split tree of `{terminal}` or `{mod, props}`).
  That format is internal, so we should not write it.
- **Sessions**: `HC_SESSION=<name>` (or `--session`) runs an isolated core with its own
  state under `~/.local/state/herdr-core/<session>/`. Config and templates are shared
  between sessions. `HC_SOCKET` overrides the endpoint. A session is not a security
  boundary.
- **Environment** inside a terminal: `HC_TERMINAL_ID`, `TERM=xterm-256color`,
  `TERM_PROGRAM=ghostty`. There is no `HERDR_ENV`, `HERDR_PANE_ID` or `HERDR_SOCKET_PATH`.
- **Events**: `hc events` / `events.subscribe` stream created, lost, restarted, keep,
  finished, removed, status, turn, title, labels, foreground and doc events.
- **Security**: core runs with the account's access. There is no per-client permission
  check, and `blocked` "is not an access boundary". State, including saved screens, is
  not encrypted.

### Herdr app concepts

- The window has **spaces > tabs > panes**. A pane is a core terminal or a **mod** (a
  JS view with `props`, such as an editor, a board or a git panel).
- **Label adoption** is documented in `~/.config/herdr-app/AGENTS.md` and
  `herdr-app --skill`:
  > Herdr labels the ones it starts with `herdr.space=<id>` and `herdr.tab=<id>`. **A
  > terminal with those labels shows up there**, even one you start yourself:
  > `hc run --keep --label herdr.space=<id> --label herdr.tab=<id> -- claude`. …
  > An id Herdr doesn't know becomes a new space or tab (unnamed: Herdr's names).

  So we can create spaces and tabs **without an open window**, using only core labels.
  Adoption also turns on `keep`. Unlabeled terminals stay hidden, which suits helpers.
- **Agents** are terminals that report OSC 7501. A tab's and a space's status is the most
  urgent of its agents (blocked > done > working > idle). Sub-agents started with `hc run`
  are listed under their parent's tab without a pane.
- **Commands** (`herdr-app do`, which needs a running window): `space.new|rename|close`,
  `tab.new|rename|close` (`tab.new` accepts `mod` + `props`), `pane.split`,
  `terminal.open|keep|park|pin|kill`, `agent.focus`, `layout.*`, `mod.open` and others.
  The full list is in `herdr-app commands`.
- **Mods** live in `~/.config/herdr-app/mods/<name>/` (`main.js` and optional
  `mod.json`). They are trusted JS that can run programs, read files, use the network,
  call `hc`, watch folders, add command-palette entries and draw panes. Check one with
  `herdr-app check <dir>`. The API is in `~/.config/herdr-app/herdr.d.ts`.
- **Worktrees**: a shipped `worktree` mod shows git worktrees as spaces nested under their
  repo. New checkouts go in `worktrees.directory` (default
  `~/.herdr/worktrees/<repo>/<branch>`).
- **Plugins**: none. There is no manifest and nothing like `[[build]]`, `[[startup]]` or
  `[[actions]]`. Mods are the extension point.

## What herdr-crew uses today, and its replacement

herdr-crew calls these TUI commands: `workspace list|focus|rename`,
`tab list|rename|file|close`, `pane list|get|run|process-info`, `worktree list|add`,
`agent start|list|get|prompt`, `notification show` and `plugin list`. It also relies on the
plugin manifest (`up` action, `startup` hook, build) and on TUI agent resume.

| herdr-crew need | Herdr TUI (today) | Core + app |
|---|---|---|
| Project workspace with a label | `workspace` with label, `workspace list` | One space per project: every crew terminal carries `herdr.space=<crew space id>` and `crew.project=<project id>`. Its name defaults to the first terminal's folder; renaming needs `herdr-app do space.rename` (window open) |
| One tab per role | `tab create/rename` | `herdr.tab=<crew tab id>` per role. **Unverified:** how an adopted tab gets its name (it may need `tab.rename` with a window open) |
| Find live roles (never two agents per role) | `agent list` by `tab_id` | `hc ls --label crew.project=P --label crew.role=R`, plus `--name crew-<p>-<r>`, which core keeps unique among live terminals |
| Start an agent and wait until ready | `agent start --kind …` | `hc run --bg --keep --no-parent --name … --cwd <worktree> --label … -- <agent cmd> "<start message>"`; readiness through OSC 7501 / `first_message.sent` |
| Type only into an idle foreground shell | `pane process-info` (fg pgid == shell pid) | `terminal.get` `foreground` (argv/pid) together with `pid`; for agents, `hc send --if idle` |
| Peer messages (`send`, `{{PEERS}}`) | `agent prompt`; Codex native queue | `hc send <name> "<text>"`, which waits for drafts and refuses on `blocked`. Agents can also report back with `hc send $HC_TERMINAL_ID` |
| Board tab | Tab with `pane run herdr-crew board` | Option A: a labeled terminal that runs `herdr-crew board` (works as is). Option B: a `crew-board` mod pane that watches `.herdr/status.json` (native, no PTY) |
| Worktrees | `worktree add/list` | Unchanged: our `git.rs` creates them and `--cwd` points at them. Optionally place them where the app's `worktree` mod finds them |
| `up` action in the UI | `[[actions]]` | A `herdr-crew` mod with a palette entry ("Crew: up") that runs `herdr-crew up` in the focused space's folder |
| `startup` hook (repair the board on restore) | `[[startup]]` | Same mod's `init()` (guarded with `checking()`), or nothing: core `keep` keeps the terminals |
| Agent restore after restart | TUI native resume (`claude --resume`, `codex resume`, `pi --session`, `agy --conversation`) | **Not provided.** A kept terminal comes back as a shell. `up` must detect it (live, foreground = shell, crew labels) and type the resume command (Claude, Codex, pi and agy session ids) |
| Notifications | `notification show` | None needed: the app shows attention from the agent status |
| Install | `herdr plugin install` + `[[build]]` | An explicit `herdr-crew app-install` (copies the mod to `~/.config/herdr-app/mods/herdr-crew`), like `codex-install`. It writes outside the project, so it must stay opt-in |
| Test isolation | Isolated `herdr` server | `HC_SESSION=crew-test-<n>` gives a separate core; `herdr-app` is not needed for core-only tests |

Invariants (`docs/design.md` §1) with core + app:

- **Never start a second agent for a live role**: holds, using the label query plus the
  unique name. Agent names stay global per core, as today.
- **Idempotent `up`**: holds. State lives in core labels, and finished terminals are
  ignored or removed.
- **Only touch this project's space**: holds, using `crew.project`. We never write the app's
  `herdr-app` doc.
- **Only type into a foreground shell**: holds, using `foreground` and `pid` from
  `terminal.get`. This needs a check that `foreground.pid` matches the shell's pid.
- **Never close a base role or remove a worktree**: unchanged.
- **Write nothing outside the project**: holds, except the opt-in mod install.

## Gotchas

1. **Parent capture.** If `herdr-crew up` runs inside an hc terminal (it always does under
   the app), the agents it starts become its children. They die when `up` exits and send
   notes into the caller. Always pass `--no-parent` (or `--outlive-parent --no-notify`).
2. **No resume.** `keep` gives back a shell, so after a reboot every role is a shell until
   `up` runs again. Crew must store each role's session id (Claude `session_id`, Codex
   thread, pi session path, agy conversation id). Today the TUI does this for us.
3. **Status depends on the agent.** Without OSC 7501, `hc wait`, `send --if idle` and the
   app's attention never work for that role. Templates with `ready: quiet` only cover
   startup. All four supported agents report it (see the spike), but Codex is silent while
   its trust dialog is shown.
4. **Space and tab ids.** Their format is undocumented (the app's own are `spa<hex>` and
   `tb<hex>`), but the spike showed that readable ids chosen by crew (`crew-spike`,
   `crew-spike-lead`) are accepted. Tabs lose their name and position whenever they are
   recreated.
5. **The window is optional.** Core-only operations work with the app closed. Anything
   that needs `herdr-app do` (rename, focus, open a mod tab) has to degrade gracefully.
6. **`TERM_PROGRAM=ghostty`, no `HERDR_*` variables.** Our hooks and the pi extension that
   check `HERDR_ENV` / `HERDR_PANE_ID` will treat a core terminal as "not under herdr".
   They need an `HC_TERMINAL_ID` path.

## Spike results (2026-11-24, core only)

Run in an isolated core (`HC_SESSION=crew-spike`) with a scratch git repo. Agents were
started with `hc run --bg --no-parent --keep --name … --label crew.project=… --label
crew.role=… -- <agent> "<prompt>"`. Versions: Claude Code 2.1.331, Codex 0.172.0, pi 1.6.0,
agy 1.5.15.

| Check | Claude Code | Codex | pi | agy |
|---|---|---|---|---|
| Reports OSC 7501 with no hooks | yes (`blocked` on the trust dialog, then `working` → `done`) | yes, but **only after** the trust dialog (no status while it shows) | yes (`working`, `done`, `error` with `msg`) | yes, as `app=antigravity` (`blocked` on the trust dialog, `idle`, `done`, with `msg`) |
| Prompt as a launch argument | works | works | works | needs `--prompt-interactive` (as with the TUI) |
| Turn counter on the launch prompt | turn 1 | turn 1 | turn 1 | turn 1 when launched from a shell; stayed 0 when the launch went through the trust dialog |
| `hc send --wait` | `done`, turn +1 | `done`, turn +1 | not tested | `done`, turn +1 |
| Resume in a kept shell after SIGKILL | `claude --resume <id>` (launched with `--session-id <id>`) recalled the code word | `codex resume --last` recalled the conversation | `pi --session-id <id>` at launch and again on resume recalled the code word (`--model openai-codex/gpt-6-luna`) | `agy --continue` recalled the code word |

Other results:

- **Parent capture confirmed.** A terminal started from inside another one with the default
  parent ended when the outer terminal exited. One started with `--no-parent` survived.
- **Keep confirmed.** After a SIGKILL of the root program, or a core stop/start, every kept
  terminal came back as `zsh -l` with the **same address, name and labels**. The status
  became `null` and `foreground.pid == pid`. **A core restart loses every agent.** A role
  that needs recovery can be recognised as: crew labels, live state, a shell as the root
  `argv`, the shell in the foreground, and no status.
- **`hc send` types into a plain shell** without checking what is in the foreground.
  Crew must keep checking the foreground before typing a resume command. This happened in
  the spike: a resume command sent while the old pi was still in the foreground was typed
  into pi as a prompt.
- **`hc wait` right after typing into a shell returns the stale status** of the previous
  program. Use `hc send --wait`, or wait for the turn to advance, or for a status newer
  than the send.
- Ids crew can store for resume: Claude and pi accept `--session-id <uuid>` at launch,
  so crew can choose the id up front. Codex and agy pick their own; `--last` / `--continue`
  is per directory, which works when each role has its own worktree. Otherwise crew must
  capture the id with a hook, as `codex/` does today.
- pi uses its configured default model (`defaultProvider` in `~/.pi/agent/settings.json`),
  so crew's per-role `--model` matters under core just as it does today.
- **The core inherits the environment of whoever starts it**, and every terminal inherits
  it in turn. A core auto-started from inside a TUI pane passed `HERDR_ENV`,
  `HERDR_PANE_ID` and `HERDR_SOCKET_PATH` to its terminals, so our hooks would believe they
  run under the TUI. A core started from inside Claude Code passed
  `CLAUDE_CODE_CHILD_SESSION`, which turns off transcript saving and so breaks
  `--continue`/`--resume`. Crew must never auto-start a core (any `hc` call does that),
  or must start it with a clean environment. It should also strip these variables from
  what it passes to agents.
- **Trust dialogs** (Claude, Codex, agy) block the first launch in a new folder, worktrees
  included. Claude and agy report `blocked`. Codex reports nothing, so `hc wait` times
  out. `up` must surface this, as it does today.
- Terminal environment from a clean core: `HC_SESSION`, `HC_TERMINAL_ID`,
  `HC_TERMINAL_SOCKET`, `HC_SCOPE`, `TERM=xterm-256color`, `TERM_PROGRAM=ghostty`.

### App adoption (Herdr app 0.1.16, default core)

Plain shells were started with `hc run --bg --no-parent --keep --label
herdr.space=crew-spike --label herdr.tab=crew-spike-lead|crew-spike-board`.

- **Ids chosen by crew work.** The app created the space `crew-spike` and the tabs
  `crew-spike-lead` / `crew-spike-board` with exactly those ids. `herdr-app ls` reports
  them, so crew can address them later.
- **Default names**: the space is named after the folder (`repo`) and the tabs by their
  position (`1`, `2`). The terminal title (OSC 2) is ignored, and so is `--name`, which
  only shows up as the terminal's name.
- `herdr-app do space.rename --id crew-spike --name …` and `tab.rename --id … --name …`
  work (`{"ok": true}`). They need the window to be open.
- **Killing a tab's only terminal closes the tab.** Relaunching with the same
  `herdr.tab` id recreates it **unnamed and at the end of the list**. Closing the last tab
  removes the space. So `up` must rename and reorder (`tab.move`) every time it recreates
  something, and must accept default names when the window is closed.
- When the spike terminals were removed, the space disappeared and nothing was left in the
  app.

Still to test:

- a board mod pane (`tab.new --mod`);
- the same checks against Herdr 0.1.18 / herdr-core 0.1.22, which are available as updates.

## Proposed plan

1. **Spike: done** (see "Spike results"). What is left: a board mod pane, and a re-check
   on Herdr 0.1.18 / core 0.1.22.
2. **Backend seam: done.** `src/backend.rs` defines `Backend`: `load` (state, with a
   typed `LoadError::Unreachable { denied }`), `foreground`, `require_native`,
   `create_workspace`, `rename_workspace`, `rename_tab`, `create_tab` (with pane env),
   `start_agent` (`Started::Ready | Waiting`), `run_in_pane`, `close_tab`, `agent_state`,
   `prompt`, `will_attach` / `focus_and_attach` and `notify`. `Herdr` implements it with the
   same CLI calls as before. `execute`, `up`, `add`, `close` and `send` take
   `&dyn Backend`. TUI-only features go through `Backend::tui()`: the Codex pane bindings
   and native queue, agy's pane-keyed launch slot, and `startup` (which still takes
   `&Herdr`). The behaviour is unchanged: the full suite and the real herdr test pass.
3. **`HcBackend`: done** (`src/hc.rs`).
   - **Selection.** `Host::detect` takes `CREW_HOST=herdr|herdr-core`, then a TUI plugin
     context (TUI), then `HC_TERMINAL_ID` (core), and the TUI otherwise.
   - **What works.** `up`, `add`, `close`, `send` and `check` work for Claude and pi roles.
     Codex and agy roles are refused before the first mutation.
   - **Environment.** Every `hc` call drops `HERDR_*`, `CLAUDE_CODE_*`, `CLAUDECODE`,
     `CODEX_THREAD_ID`, `CREW_CODEX_BINDING` and `HC_*` (except `HC_SESSION` / `HC_SOCKET`).
   - **Agent start.** `start_agent` waits up to 30 s for the new shell to reach the
     foreground, labels the terminal and types the agent's command line. It then waits up to
     60 s for the first status report.
   - **Tests.** `tests/support/hc.py` simulates `hc`, and `tests/hc.rs` covers up,
     idempotence, dry run, send, refusals, add/close and the Codex refusal.
   - **Real smoke test** (isolated `HC_SESSION`, two Claude roles and the board): `up` in
     7 s; no `HERDR_*` or `CLAUDE_CODE_*` variables in the terminals, although crew started
     the core from inside a TUI pane and a Claude session. A second `up` changed nothing.
     `send` from lead was delivered framed, and dev answered with its role name.
4. **Restore**: `up` repairs roles that came back as shells, using stored session ids.
5. **App mod** (`herdr-crew app-install`): a palette "Crew: up" entry, and a board pane
   that replaces the board terminal.
6. **Retire the TUI backend** once core + app cover everything. Remove
   `herdr-plugin.toml`, `startup` and the TUI-specific code (Codex native queue, `agent
   prompt`). Update README, `design.md` and the CHANGELOG.

## Open questions

- Will the app get a plugin or actions manifest that replaces `herdr-plugin.toml`?
- Will core get agent resume (it would remove step 4), or is that meant to be done by
  callers?
- Is the `herdr.space` / `herdr.tab` label contract stable? The app guide documents it,
  but the public `docs/labels.md` only lists `herdr.seen` and `herdr.pinned`.
- Codex, pi and agy status under core: native OSC 7501, or does each one need a hook?
