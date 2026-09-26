# herdr-crew

A herdr plugin that starts a project's Claude Code sessions, one tab per role, and draws the project's status board in another tab. Everything comes from one versioned file in the repository, `.herdr/crew.toml`.

- `up` creates the herdr workspace, a tab per role (in the main checkout or in its own git worktree), and starts `claude` with the role's prompt. It is idempotent: it never starts a second agent for a live role, and it only creates what is missing.
- `add` and `close` add or remove extra instances of a role (`dev-2`, `dev-3`…).
- `board` draws `.herdr/status.json`, the board the coordinating role keeps up to date, and redraws it when it changes.

The design, with its decisions and the herdr behaviour they rely on, is in [docs/design.md](docs/design.md).

## Install

You need Rust (stable), `git`, `claude` and herdr 0.9.1 or later.

```sh
git clone https://github.com/rarce/herdr-crew
cd herdr-crew
cargo build --release --locked
herdr plugin link "$PWD"
```

`plugin link` does not build anything, so rebuild after each update. `herdr-crew check` prints the binary's version.

The binary also works without the plugin: `target/release/herdr-crew up` from inside the project.

## Usage

```text
herdr-crew [--root DIR] <command>
  up [--no-attach] [--dry-run]   start or complete the project's sessions
  add <role>                     add an extra instance of a role with extra = true
  close <name>                   close the tab of an extra instance (keeps its worktree)
  board [--file PATH] [--once] [--interval S]
                                 draw the status board
  check                          validate the configuration and the dependencies
```

- **Where the project is.** The project root is the main checkout of the git repository around the current directory, or around `--root`, even when called from a worktree.
- **The `herdr-crew.up` action.** It works on the workspace you have focused in herdr.
- **Starting the server.** From a plain terminal, `up` starts a herdr server if none is running and then opens herdr. Pass `--no-attach` to skip opening it.
- **`--dry-run`** prints each role's verdict and the steps `up` would take, without doing anything.

**No-parameter actions.** herdr actions take no parameters, so `add` and `close` are not actions. There is also a trap: an action invoked with `herdr plugin action invoke` acts on whichever workspace is focused, not on the caller's. So agents and scripts run the binary directly: `{{LAUNCHER}} up` or `{{LAUNCHER}} add <role>` in a prompt (see `{{LAUNCHER}}` below), never `plugin action invoke`.

## `.herdr/crew.toml`

```toml
version = 1

# Optional: appended to every role's prompt, after a blank line.
common_prompt = '''
The status board is {{STATUS}} (format in {{SCHEMA}}); only globex-lead writes it.
'''

[workspace]
label = "globex"                  # the herdr workspace label

[board]
tab = "globex-status"             # the board tab's label
file = ".herdr/status.json"       # relative to the root (default)
writer = "globex-lead"            # the only role that writes the board

[worktrees]                       # only needed if a role has worktree = true
dir = ".worktrees"                # relative to the root; ignore it in git
base = "origin/main"              # fetched, then `git worktree add --detach` on it

[[roles]]                         # tab order and board order
name = "globex-lead"
prompt = '''
You are {{NAME}}, the lead, in {{REPO}}. When more hands are needed, and only after the user
agrees, run: {{LAUNCHER}} add globex-dev
'''

[[roles]]
name = "globex-dev"
prompt = '''
You are {{NAME}}, a developer working in your own worktree.
'''
worktree = true                   # works in .worktrees/globex-dev
extra = true                      # allows globex-dev-2, globex-dev-3…
```

**Keys:**

| Key                | Required         | Meaning                                                                                                                                                                                             |
| ------------------ | ---------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `version`          | yes              | Always `1`.                                                                                                                                                                                         |
| `common_prompt`    | no               | Text appended to every role's prompt.                                                                                                                                                               |
| `workspace.label`  | yes              | The workspace label; `up` only touches the workspace with this label.                                                                                                                              |
| `board.tab`        | yes              | The board tab's label; must differ from every role name.                                                                                                                                           |
| `board.file`       | no               | The board file, relative to the root. Default `.herdr/status.json`.                                                                                                                                 |
| `board.writer`     | yes              | The role that writes the board; must be one of `roles`.                                                                                                                                            |
| `worktrees.dir`    | with worktrees   | Where role worktrees live, relative to the root.                                                                                                                                                    |
| `worktrees.base`   | with worktrees   | `remote/branch` that new worktrees start from, detached.                                                                                                                                            |
| `roles[].name`     | yes              | The tab label and the agent name: `[a-z][a-z0-9_-]{0,31}`, at most 29 characters with `extra`. Prefix names with the project's so they don't clash with other projects' agents, which are global in herdr. |
| `roles[].prompt`   | yes              | The role's prompt.                                                                                                                                                                                 |
| `roles[].worktree` | no               | `true` to work in `<worktrees.dir>/<name>`.                                                                                                                                                         |
| `roles[].extra`    | no               | `true` to allow `add` and `close` of `<name>-N` instances.                                                                                                                                          |

**Prompts.**

- A session's prompt is its role's `prompt`, then `common_prompt`, with these placeholders replaced:

  | Placeholder    | Value                                   |
  | -------------- | --------------------------------------- |
  | `{{NAME}}`     | the session name, such as `globex-dev-2` |
  | `{{REPO}}`     | the root                                |
  | `{{STATUS}}`   | `board.file`                            |
  | `{{SCHEMA}}`   | `.herdr/status.schema.json`             |
  | `{{LAUNCHER}}` | the binary's absolute path              |

- The result is written to `.herdr/prompts/<name>.txt` and passed with `claude --append-system-prompt-file`.
- Use literal strings (`'''…'''`): they keep backslashes as written. Basic strings (`"""…"""`) are accepted too.

**Validation.** Unknown keys and unknown placeholders are errors, and every error is reported at once with its line and column:

```text
herdr-crew: /path/to/repo/.herdr/crew.toml:22:9: roles[0].prompt: unknown placeholder {{LAUNCH}}
```

Add the generated files to `.gitignore`:

```gitignore
/.herdr/status.json
/.herdr/status.json.tmp
/.herdr/status.schema.json
/.herdr/prompts/
```

## The status board

The coordinating role (`board.writer`) keeps `.herdr/status.json` up to date, writing it atomically (to a `.tmp` file, then renaming). `up` generates `.herdr/status.schema.json` from `crew.toml`, and the descriptions in that schema tell the writer what each field means. If the file is missing, `up` seeds it.

```json
{
  "$schema": "status.schema.json",
  "version": 1,
  "updatedAt": "2026-09-25T10:40:00-03:00",
  "updatedBy": "globex-lead",
  "sessions": [
    {
      "role": "globex-lead",
      "state": "working",
      "now": "Planning the next step",
      "next": ["Review globex-dev's change"]
    },
    {
      "role": "globex-dev-2",
      "state": "waiting",
      "now": "Waiting for review",
      "next": [],
      "note": "Blocked on the fixture question",
      "updatedAt": "2026-09-25T10:30:00-03:00"
    }
  ],
  "owner": {
    "urgent": [],
    "beforeMain": ["Approve the release notes"],
    "optional": []
  },
  "recent": [{ "at": "2026-09-25T10:15:00-03:00", "text": "globex-dev pushed the fix" }]
}
```

The fields follow these rules:

- **Each session** needs `role`, `state`, `now` and `next` (a list, possibly empty); `note` and `updatedAt` are optional.
- **`state`** is `working`, `waiting`, `idle` or `blocked`.
- **`role`** is a role name, or `<extra role>-N` with N ≥ 2. Each role appears at most once.
- **`updatedBy`** must be `board.writer`.
- **Dates** are RFC 3339 with an offset.
- **Sessions** are drawn in `roles` order, each extra instance after its base role.
- **`owner`** lists what the sessions are waiting on from the user; the board shows it as "Waiting on you".

When the file is missing, has invalid JSON or does not match the schema, the board shows why instead of the content. Keys: `q`/`Esc` quit, `r` redraws.

## Development

```sh
cargo fmt --check
cargo clippy --locked -- -D warnings
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

**Board snapshots.** Board drawings are compared with `tests/snapshots/`; `UPDATE_SNAPSHOTS=1 cargo test` rewrites them.

**Real-herdr test.** `tests/real_herdr.rs` is ignored by default. It runs against a real herdr with its XDG directories in a short temporary directory. It starts its own `herdr --session crewtest` server there, uses `cat` instead of `claude`, and removes everything when it ends:

```sh
CREW_REAL_HERDR=1 cargo test --test real_herdr -- --ignored --nocapture
```

The test clears the environment of the processes it starts, so no `HERDR_*` or `CLAUDE*` variable reaches them. It aborts before starting anything if its socket would not be under its temporary directory, and it never touches the user's herdr configuration.

## License

[MIT](LICENSE)
