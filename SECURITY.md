# Security policy

## Reporting a vulnerability

Please do not open a public issue for security problems. Report them privately through
GitHub: [report a vulnerability](https://github.com/rarce/herdr-crew/security/advisories/new)
(also under the repository's **Security** tab).

Include what an attacker controls, what they gain, the herdr-crew and herdr versions, and
the steps or a proof of concept. Leave out real credentials and private data.

herdr-crew is maintained by one person on a best-effort basis. The aim is to acknowledge
a report within a week, agree on a fix and disclosure date with you, and credit you in the
advisory unless you prefer otherwise.

## Supported versions

Only the latest release receives security fixes. herdr-crew is experimental (0.x), so a
fix ships as a new release rather than a backport.

| Version | Supported |
| ------- | --------- |
| 0.2.x   | Yes       |
| < 0.2   | No        |

## Dependencies

`Cargo.lock` is checked against the [RustSec advisory database](https://rustsec.org/) with
`cargo audit` when dependencies change and weekly, and Dependabot proposes updates for
crates and GitHub Actions every week. A vulnerable dependency that affects herdr-crew is
fixed in a new release.

## What herdr-crew can do on your machine

These are the boundaries a report would most likely cross:

- **Files.** It writes only inside the project: `.herdr/` (board, schema, prompts, Codex
  snapshots and bindings), `.gitignore` during `init`, Git worktrees under
  `worktrees.dir`, and the project lock `herdr-crew.lock` in the repository's Git
  directory. Outside the project it writes the public launcher (`~/.local/bin` or
  `CREW_BIN_DIR`) and, only through `codex-install`, one entry in
  `$CODEX_HOME/hooks.json`. Paths that leave the project, absolute paths and symlinked
  destinations are rejected.
- **Processes.** It runs `git`, `herdr` and the configured agent CLIs, and starts agents
  through herdr with the role prompts from `crew.toml`.
- **Sessions.** It acts only on the herdr workspace whose label and panes belong to the
  project, and stops on collisions.

Examples of vulnerabilities: a write, deletion or worktree outside those locations; a
command or argument injected from `crew.toml`, the board or a branch name; acting on
another project's workspace; or the Codex hook leaking or replacing configuration it does
not own.

## Treat `crew.toml` like code

A `crew.toml` decides which agents run, with which prompts, and, for Codex, with which
`sandbox`, `approval_policy` and `additional_dirs`. A repository can set
`sandbox = "danger-full-access"` with `approval_policy = "never"`. Read a project's
`.herdr/crew.toml` before running `herdr-crew up` in a repository you did not write, just
as you would read its build scripts. That herdr-crew honors such a configuration is
documented behavior, not a vulnerability.

## Out of scope

- Vulnerabilities in herdr, Claude Code, Codex or Git themselves: report them to those
  projects.
- What an agent does with the prompts and permissions its configuration gives it,
  including prompt injection through repository content.
- Attacks that require control of your user account, shell startup files or `PATH`.
- Plugins in the herdr marketplace are not reviewed by herdr; read the source before
  installing any plugin, this one included.
