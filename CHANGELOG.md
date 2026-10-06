# Changelog

All notable changes to herdr-crew. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). Before 1.0, a minor version may change
behavior; such changes are listed under **Changed**.

herdr-crew is experimental: configurations, commands and the board format may still
change between minor versions.

## [0.2.0] - 2026-10-06

### Added

- **Codex support alongside Claude Code.** A crew can use Claude Code, Codex or both,
  set per project or per role with `kind`, with structured Codex options (model, profile,
  reasoning effort, sandbox, approval policy, search, additional writable directories).
  Role instructions reach Codex through an additive `SessionStart` hook that
  `codex-install` merges into `$CODEX_HOME/hooks.json` and `codex-uninstall` removes.
  `codex-list` and `codex-resume` recover saved sessions with their role settings.
  Codex roles need herdr 0.9.3+, Codex CLI 0.160.1+ and the trusted crew hook. See
  `docs/codex-support.md`.
- **Setup wizard.** `herdr-crew init` creates `.herdr/crew.toml` from the presets `solo`,
  `review`, `parallel` and `research`, previews the result, refuses to overwrite an
  existing configuration and adds generated files to `.gitignore` (`--no-ignore` to skip).
  Non-interactive with `--yes`; `--dry-run` writes nothing.
- **Public CLI launcher.** Installing the plugin also installs `herdr-crew` in
  `~/.local/bin` (or `CREW_BIN_DIR`). It resolves the registered plugin on every call, so
  reinstalling updates it. `--uninstall-launcher` removes only a launcher it owns.
- **`check` reports what it verified.** It resolves `git`, `herdr` (`HERDR_BIN_PATH` when
  set) and the configured agents to executable files, prints their versions, requires
  herdr 0.9.1+, validates `worktrees.base` offline, says whether the base still needs a
  fetch, checks existing role worktrees, and names what it does not check (network
  access, agent sign-in).
- A stateful herdr simulator runs full bring-up, recovery and Codex flows in the regular
  test suite, and a weekly `real-herdr.yml` workflow tests against a pinned herdr 0.9.3.

### Changed

- **Project isolation is enforced before any change.** `up`, `startup`, `add` and `close`
  verify that a workspace with the project's label really belongs to this repository
  (main checkout or registered worktree) and stop on label collisions.
- **Generated files stay inside the project.** `board.file` and `worktrees.dir` may no
  longer be absolute or leave the project with `..`; symlinked destinations are
  rejected. Generated files are written through exclusive temporary files and replaced
  atomically. Configurations that pointed outside the project must be moved inside it.
- **Reused worktrees must be real.** A directory under `worktrees.dir` is reused only if
  it is a Git worktree registered to this repository; empty folders, files, links and
  stale or borrowed registrations are rejected instead of being used as agent
  directories.
- `up` validates `worktrees.base` (configured remote, valid branch name) before its
  first change, instead of failing after creating the workspace.
- The board viewer applies the generated schema's rules: `$schema`, `note` and a
  session's `updatedAt` must be strings when present (no `null`), and `version` accepts
  `1.0`. An equivalence test keeps the viewer and the schema aligned.
- The startup fallback is easier to discover: `check` explains that `herdr` only attaches
  to a running server and prints the exact `up --no-attach` command.

### Fixed

- `board --interval` no longer panics on `inf`, `NaN` or huge values; values that are not
  finite, round to zero or exceed 3600 seconds are usage errors (exit 2).
- Crew setup validates its roots and launcher destinations.

## [0.1.0] - 2026-09-26

First tagged version: a herdr plugin that starts one Claude Code session per role in a
herdr workspace from a versioned `.herdr/crew.toml`, with optional Git worktrees and
extra instances, a startup hook that brings a project up from plain `herdr` and repairs
it on restore, and a terminal status board fed by `.herdr/status.json`.

[0.2.0]: https://github.com/rarce/herdr-crew/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/rarce/herdr-crew/releases/tag/v0.1.0
