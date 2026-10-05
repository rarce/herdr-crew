# Repository Guidelines

## Project Structure & Module Organization

This repository builds `herdr-crew`, a Rust CLI and herdr plugin. `src/main.rs` parses commands and dispatches them; `config.rs`, `plan.rs`, `herdr.rs`, `git.rs`, and `prompt.rs` handle configuration, session planning, external commands, worktrees, and prompts. `src/board/` contains the status model, schema, rendering, and terminal UI. `tests/fixtures/` holds sample crew and status data, while `tests/snapshots/` holds expected board output. `tests/real_herdr.rs` is an optional integration test. User documentation and images are in `README.md` and `docs/`; `herdr-plugin.toml` defines the plugin build and actions.

## Build, Test, and Development Commands

- `cargo build --release --locked` builds the binary used by the plugin.
- `cargo fmt --check` checks Rust formatting; run `cargo fmt` to apply it.
- `cargo clippy --locked --all-targets -- -D warnings` checks production and test code with warnings treated as errors.
- `cargo test --locked` runs the normal test suite.
- `target/release/herdr-crew check` validates a project's `.herdr/crew.toml` and dependencies. Run it from that project directory.

CI also runs Clippy without `--all-targets` on macOS and Linux. Keep `Cargo.lock` current when changing dependencies.

## Coding Style & Naming Conventions

Use Rust 2024 edition and standard `rustfmt` formatting (four-space indentation). Name modules and functions in `snake_case`, types in `PascalCase`, and constants in `SCREAMING_SNAKE_CASE`. Keep command handling in `main.rs` and put board-specific behavior in `src/board/`. Follow existing error handling and document behavior that affects herdr sessions or worktrees.

## Testing Guidelines

Add focused `#[test]` cases near the module under test, using descriptive `snake_case` names. Use `tests/fixtures/` for input examples and update board snapshots with `UPDATE_SNAPSHOTS=1 cargo test` when output changes; review the resulting diff. The real-herdr integration test is ignored by default. Run it only in a suitable local environment with `CREW_REAL_HERDR=1 cargo test --test real_herdr -- --ignored --nocapture`.

## Commit & Pull Request Guidelines

Recent commits use short, imperative subjects such as “Repair only the board on restore.” Follow that style and keep each commit focused. In pull requests, describe the behavior changed, note relevant tests, and link an issue when one exists. Include before-and-after terminal output or screenshots for board UI changes, and call out changes to plugin startup or worktree behavior.
