# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

`AGENTS.md` and `CONTRIBUTING.md` hold the repository guidelines (style, commits, PRs); this file adds what you need to work in the code. `docs/design.md` is the authoritative design: invariants, the exact herdr call sequence for `up`, and a **Deferred** table of ideas postponed on purpose. Read the relevant section before changing behavior.

## What this is

`herdr-crew` is a Rust CLI packaged as a [herdr](https://herdr.dev/) plugin (`herdr-plugin.toml`). From a versioned `.herdr/crew.toml` in a user's repo it brings up a herdr workspace with one tab per role running a coding agent (Claude Code, Codex or pi), optionally each in its own git worktree, plus a tab that draws the `.herdr/status.json` board.

## Commands

```sh
cargo fmt --check
cargo clippy --locked -- -D warnings              # CI runs both clippy variants
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked                               # offline: no herdr, agents or network needed
sh scripts/build.sh                               # release build, copies binary to bin/herdr-crew (what the plugin runs)

cargo test --locked --test execution <name>       # one integration test file / filter
cargo test --locked --bin herdr-crew plan::       # unit tests in one module
UPDATE_SNAPSHOTS=1 cargo test --locked            # rewrite tests/snapshots/ board output; review git diff

# Opt-in, ignored by default; start isolated real herdr servers:
CREW_REAL_HERDR=1 cargo test --locked --test real_herdr -- --ignored --nocapture
CREW_REAL_HERDR=1 cargo test --locked --test launcher -- --ignored
CREW_REAL_HERDR=1 cargo test --locked --test codex_herdr -- --ignored --nocapture
CREW_REAL_HERDR=1 CREW_REAL_CODEX=1 cargo test --locked --test codex_herdr native_codex_queue -- --ignored --nocapture
```

`herdr-crew up --dry-run` prints per-role verdicts and planned steps without acting; `herdr-crew check` validates a project's config and dependencies.

## Architecture

**Pure core / adapter split (keep it).** The core runs no processes and reads neither clock nor disk except via arguments, so most behavior is testable without herdr:

- `config.rs` — `Config::parse(text, root) -> Result<Config, Vec<Error>>`; reports all errors at once with line/column; unknown keys are errors.
- `prompt.rs` — placeholder expansion (`{{STATUS}}`, `{{SCHEMA}}`, `{{LAUNCHER}}`, `{{PEERS}}`…) and the combined role prompt.
- `plan.rs` — `plan(&Config, &HerdrState, &Env) -> Plan`: per-role `Verdict`s plus a list of `Step`s (`CreateWorkspace`, `CreateWorktree`, `CreateTab`, `WritePrompt`, `StartAgent`, `RunInPane`, `SeedStatus`, `WriteSchema`, `Warn`…). Step ids are symbolic references resolved at execution time.
- `board/model.rs`, `board/schema.rs`, `board/view.rs` — status.json types/validation, JSON Schema generated from the config, ratatui drawing.

Adapters: `main.rs` (hand-rolled arg parsing — no clap — dispatch, exit codes: 1 plan stopped, 2 usage/config error), `herdr.rs` (runs the herdr CLI, parses its JSON `.result`/`.error.code`, executes `Step`s with retry policy), `git.rs` (main-checkout resolution, fetch, worktree add/verification), `files.rs` (safe generated-file writes, no symlink following via `rustix`), `lock.rs` (per-project OS file lock in the git common dir), `process.rs` (bounded child processes), `board/tui.rs` (event loop).

Agent-specific code: `agent.rs` (agent kind selection and typed Codex/pi options), `codex/` (SessionStart hook, activation snapshots/bindings under `.herdr/codex/`, recovery, native queue messaging), `pi.rs` + `src/pi/herdr-crew.ts` (pi extension that persists the role prompt in the pi session), `send.rs` (`send` command and `{{PEERS}}` channel list), `init.rs` (setup wizard and presets). `src/bin/herdr-crew-launcher.rs` is the public `~/.local/bin/herdr-crew` launcher that resolves the registered plugin on each call.

Deliberately avoided dependencies: `clap`, `anyhow`, `tokio`, `jsonschema`. Justify any new dependency.

## Invariants that must hold (from docs/design.md §1)

- Never start a second agent for a live role (a tab is occupied if any agent has its `tab_id`, named or not; agent names are global across the herdr server).
- `up` is idempotent: a second run only creates what is missing; a no-change `up` has no steps.
- Only touch the workspace with the project's label; verify panes resolve to this project's main checkout before mutating.
- Board and generated files live in the main checkout even when invoked from a worktree.
- Only type into a pane whose shell is in the foreground (`foreground_process_group_id == shell_pid`).
- Never remove a worktree or close a base role; `close` only closes an extra instance's tab.
- Write nothing outside the project except declared worktrees (and the explicit `codex-install`/`pi-install` targets).
- `startup` (herdr's `[[startup]]` hook) never waits on the lock and on restore repairs only the board.

## Tests

- `tests/execution.rs` drives the real CLI against `tests/support/herdr.sh`, a stateful herdr simulator, with real temp git repos and a local bare remote. `CREW_AGENT_CMD` replaces `agent start` with `pane run <cmd>` for tests.
- `tests/isolation.rs` (collisions, unsafe paths, symlinks), `tests/init.rs` (presets/wizard), `tests/launcher.rs`, `tests/codex_herdr.rs`.
- Board snapshots in `tests/snapshots/`; fixtures in `tests/fixtures/`.

## When changing behavior

Update `README.md` and `docs/design.md` when behavior, commands or configuration change, and add an entry under `Unreleased` in `CHANGELOG.md` for user-visible changes. Call out changes to plugin startup, worktree handling or files written in users' projects. Out of scope: Windows support, task orchestration, editing users' agent config beyond the documented Codex hook / pi extension.
