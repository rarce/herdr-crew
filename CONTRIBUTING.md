# Contributing to herdr-crew

Thanks for your interest. herdr-crew is experimental and maintained by one person, so
small, focused changes with tests are the easiest to review and merge.

## Before you start

- **Security problems:** report them privately as described in [SECURITY.md](SECURITY.md),
  not in a public issue.
- **Bugs:** open an issue with the bug report template. A reproduction and the output of
  `herdr-crew check` save a lot of back and forth.
- **Small fixes** (typos, clearer messages, a missing test): open a pull request directly.
- **New behavior or larger changes:** open an issue first to agree on the approach.
  `docs/design.md` records the design and its decisions; the **Deferred** table at its end
  lists ideas that are postponed on purpose, each with the event that would justify it.
  Changes that touch herdr sessions, worktrees or plugin startup need the most care.

Out of scope for now: Windows support, task orchestration or messaging between sessions,
and anything that edits a user's agent configuration beyond the documented Codex hook.

## Setting up

You need Rust stable with `cargo`, and `git`. herdr 0.9.1 or newer (0.9.3 for Codex roles)
is only needed to try the plugin by hand or to run the real-herdr integration test.

```sh
git clone https://github.com/rarce/herdr-crew
cd herdr-crew
cargo build --release --locked
```

To try your build in herdr, link the checkout as a plugin. herdr then uses your checkout
instead of an installed herdr-crew; reinstall the plugin to go back:

```sh
sh scripts/build.sh
herdr plugin link "$PWD"
target/release/herdr-crew-launcher --install-launcher   # optional: herdr-crew on PATH
```

Run `sh scripts/build.sh` again after each change; `plugin link` does not build.

## Project layout

- `src/main.rs` parses arguments and dispatches commands.
- `config.rs`, `plan.rs`, `herdr.rs`, `git.rs`, `prompt.rs` and `files.rs` handle the
  configuration, session planning, herdr calls, worktrees, prompts and safe file writes.
  `src/codex/` holds the Codex hook and recovery.
- `src/board/` contains the status model, its JSON Schema, rendering and terminal UI.
- `tests/` holds integration tests, `tests/fixtures/` sample data and `tests/snapshots/`
  the expected board output.

The planning core is pure and the adapters run the commands; keep it that way, so most
behavior can be tested without herdr.

## Checks

CI runs these on Linux and macOS; run them before opening a pull request:

```sh
cargo fmt --check
cargo clippy --locked -- -D warnings
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
sh scripts/build.sh
```

`cargo test` includes `tests/execution.rs`, which drives the real CLI against a stateful
herdr simulator, real Git repositories and a local remote. It needs no herdr, agents or
network.

If you change the board output, update the snapshots and review the diff:

```sh
UPDATE_SNAPSHOTS=1 cargo test --locked
git diff tests/snapshots
```

The real-herdr integration test is ignored by default. It starts an isolated herdr server
with simulated agents; run it only on a machine where that is acceptable:

```sh
CREW_REAL_HERDR=1 cargo test --locked --test real_herdr -- --ignored --nocapture
```

The Development section of `README.md` describes two more opt-in tests for Codex.
Maintainers can also run the `Real herdr integration` workflow from the Actions tab.

## Writing changes

- Follow the existing style: Rust 2024 edition, `rustfmt`, `snake_case` functions and
  modules, `PascalCase` types, `SCREAMING_SNAKE_CASE` constants.
- Add focused `#[test]` cases next to the code they test, with descriptive names. Prefer
  a test that fails without your change.
- Update `README.md` and `docs/design.md` when behavior, commands or configuration change,
  and add an entry to the `Unreleased` section of `CHANGELOG.md` for user-visible changes.
- Keep `Cargo.lock` current when changing dependencies, and explain why a new dependency
  is worth it.

## Commits and pull requests

- Use short, imperative commit subjects, such as "Repair only the board on restore", and
  keep each commit focused on one change. Explain the reason in the body when it is not
  obvious.
- In the pull request, describe the behavior that changed, the tests you ran, and link the
  issue if there is one.
- For board or terminal UI changes, include before-and-after output or screenshots.
- Call out changes to plugin startup, worktree handling or files written in users'
  projects.

The maintainer reviews pull requests as time allows. A pull request needs green CI and an
approving review to merge; feedback may ask for smaller pieces or additional tests.

## License

By contributing, you agree that your contributions are licensed under the
[MIT License](LICENSE).
