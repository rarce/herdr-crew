# herdr-crew: design of a herdr plugin for role-based Claude Code sessions

**Date:** 2026-09-26, updated 2026-10-05. **Status:** the design the code honours. Step 1 of the construction table (§11) is built: the binary, its tests and a real test against an isolated herdr. The step-1 hypotheses are verified, except the implicit `.exe` on Windows. The startup hook (§4.4), plugin artifact preparation and public CLI launcher (§8) are built too.

Where claims come from:

- **[verified]**: checked on 2026-09-26 with herdr 0.9.1 on macOS. The checks used read-only commands (`herdr --version`, the help of `herdr <group>`, `herdr plugin … --help`, `herdr api schema --json` with protocol 22) or ran in an isolated herdr test session (§7.2).
- **[vendor]**: herdr 0.9.1 documentation or source code.
- **[E]**: our own estimate.
- **[H]**: a hypothesis checked in step 1 of §11 before relying on it.

## 1. Summary and scope

`herdr-crew` is a Rust binary packaged as a herdr plugin. It reads one versioned file in each repository, `.herdr/crew.toml`, and does two things:

- it starts, in a herdr workspace, one tab per Claude Code session with its role prompt;
- it draws the `.herdr/status.json` board in another tab.

It replaces the per-project launcher scripts and board viewers that teams tend to copy from project to project. Those copies diverge: role order, schema, one flavour per shell. What really differs between projects is data:

- the workspace label and the role names;
- the role prompts;
- whether a role works in its own git worktree;
- whether a role allows extra instances;
- who writes the board.

That goes into the configuration file; the logic lives in one binary.

**Out of scope:**

- **Installing dependencies.** The plugin runs inside herdr and only needs `git` and `claude`; `check` reports what is missing with its install command, without running it.
- **Writing the board.** Only the coordinating role writes it, as its role prompt says.
- **Reading agent state for the board.**
- **Agents other than Claude Code.**
- **Touching `~/.config/herdr/config.toml` or `plugins.json`.** Linking or installing the plugin is the user's decision.
- **`[[events]]` and `[[link_handlers]]`.** The only hook is `[[startup]]` (§4.4).

**Invariants:**

1. **Never a second agent for a live role.** `up` does not start an agent in a tab that already has one, named or not, nor for a role whose name a live agent carries (§4.2 step 3).
2. **Idempotency.** Running `up` twice duplicates no workspace, tab or agent; the second run only creates what is missing.
3. **Isolation between projects.** `up` only creates or changes things in the workspace with the project's label, and never reuses another project's workspace or tab. herdr agent names are global, so a live agent carrying a role's name, in any workspace, blocks that role (§10).
4. **The main checkout.** The board and the generated files live there even when the call comes from a worktree.
5. **Project-local runtime.** Crew commands write nothing outside the project's repository, except the worktrees the project declares. Installation separately prepares the plugin artifact and installs its public launcher in the user-selected bin directory (§8).
6. **Only an idle shell.** It never types into a pane that runs anything other than its shell (§4.2 step 9).
7. **A robust viewer.** An unreadable or off-schema board is shown as such; it never brings the viewer down.
8. **Nothing base is closed.** It never removes a worktree or closes a base role; `close` only closes the tab of an extra instance.

## 2. Configuration file

### 2.1 Name and discovery

The file is `<root>/.herdr/crew.toml` and it is versioned. Projects ignore only the generated files in `.herdr/`: `status.json`, its `.tmp`, `status.schema.json` and `prompts/`.

The root is resolved as follows; the first rule that yields a directory wins:

1. `--root <dir>` on the command line.
2. As an action (`HERDR_PLUGIN_ID` set): `workspace_cwd` from `HERDR_PLUGIN_CONTEXT_JSON` [verified: the `PluginInvocationContext` type in `herdr api schema --json`], and only that field.
   - `focused_pane_cwd` is not used, so a pane focused on another repository inside the same workspace does not switch projects.
   - If `workspace_cwd` is missing, it fails with "don't know which project: focus a workspace of the project". The current directory of an action is the plugin's own [vendor], so it can't be used.
   - When the action is invoked through the CLI, herdr does fill these fields, but with the server's **focused** workspace and pane, not the pane the command was typed in [verified: `plugin action invoke` typed in another tab received the focused pane's `focused_pane_id`; the same happens outside any pane]. That is enough because the user invokes the action on the workspace in front of them.
   - Agents, scripts and any other project use the binary directly (`{{LAUNCHER}} up`), never `plugin action invoke`.
3. Outside an action, the current directory.

From that directory, Git's absolute `git-dir` and `git-common-dir` distinguish a main checkout from a linked worktree. Main checkouts, including submodules and repositories with separate metadata, use `git rev-parse --show-toplevel`. For a linked worktree, the parent of the common directory is a candidate only: its working checkout must use that same common directory as its own Git directory. If it cannot be verified, the command fails and asks to run from the main checkout rather than writing in metadata or an unrelated repository. Bare repositories are rejected. Without a git repository it fails, because worktrees require one. Except for `init`, a missing `<root>/.herdr/crew.toml` fails naming the path it looked for.

### 2.2 Schema

```toml
version = 1                              # required; only 1 today

# Optional: appended to every role's prompt, after a blank line.
common_prompt = '''
The status board is {{STATUS}} (format in {{SCHEMA}}); only globex-lead writes it.
'''

[workspace]
label = "globex"                         # herdr workspace label; the idempotency key

[board]
tab = "globex-status"                    # label of the board tab
file = ".herdr/status.json"              # relative to the root; this is the default
writer = "globex-lead"                   # the only role that writes the board; must be a role

[worktrees]                              # required only if some role has worktree = true
dir = ".worktrees"                       # relative to the root; the project ignores it in git
base = "origin/main"                     # remote/branch: fetch that branch and worktree --detach on it

[[roles]]                                # the order is the tab order and the board order
name = "globex-lead"
prompt = '''
You are {{NAME}}, the lead. For more hands, once the user agrees: {{LAUNCHER}} add globex-dev
'''

[[roles]]
name = "globex-dev"
prompt = '''
You are {{NAME}}, a developer working in your own worktree.
'''
worktree = true                          # works in <worktrees.dir>/<name>
extra = true                             # allows add/close: globex-dev-2, globex-dev-3…
```

A project without worktrees uses the same schema without `[worktrees]`, `worktree` or `extra`.

**Prompts live in `crew.toml`.** `prompt` is a string, and so is the optional top-level `common_prompt`.

- Examples use TOML literal strings (`'''…'''`) because basic strings (`"""…"""`) process backslash escapes, for example in a Windows path. Basic strings are still accepted.
- The session's prompt is its role's prompt, then `common_prompt`, separated by a blank line, with the placeholders replaced.
- The result is written to `.herdr/prompts/<name>.txt` (ignored) and passed as a file, not inline. herdr starts the agent by typing the command into the pane's shell: an inline prompt of about 1.7 KB was cut mid-quote there, and herdr rejects newlines outright [verified].
- An extra instance uses its base role's prompt.

**First message: `start_message`.** Optional, at the top level and per role; a role's value replaces the top-level one, and an empty role value means none for that role. Extra instances use their base role's.

- It is passed to `claude` as its positional initial prompt, after the other arguments. herdr quotes it like them: a message with accents, «», an apostrophe, `$HOME` and a backtick reached `claude` whole, with nothing expanded [verified in the real test, §7.2].
- It is sent only when a tab is created and its agent started: `up` over a missing role, adoption (§4.4) and `add`. Never on a resume, a repair, or an `up` over a tab without an agent.
- **Why.** Claude records a conversation only on its first request; a role nobody wrote to has no transcript and is lost on a herdr restart (§10). The first message gives it one.
- **Cost.** One request per role on every fresh start. The message must not ask for anything that changes the tree (files, git): the agent runs it with no human looking yet.
- **Validation.** No control character (`char::is_control`: newlines, tabs, escapes), so one line; herdr types the command into the shell and rejects newlines [verified]. At most 200 bytes, and not starting with `-`, which `claude` would read as an option. No placeholders.
- **The folder-trust dialog.** In a folder `claude` does not trust yet, `agent start` returns `agent_not_ready` (§4.2 step 8). That the positional prompt is still sent once the user accepts the dialog is [H]: the real test stops at the dialog without answering it, so as not to trust a temporary folder in the user's `claude` configuration.

```toml
start_message = '''Confirm your role in one line and wait for instructions from globex-lead.'''

[[roles]]
name = "globex-lead"
start_message = ""                       # none for this role
```

Placeholders:

| Placeholder    | Value                                                                                             |
| -------------- | ------------------------------------------------------------------------------------------------- |
| `{{NAME}}`     | the session name, with its number for an extra instance                                           |
| `{{REPO}}`     | the root                                                                                          |
| `{{STATUS}}`   | `board.file`                                                                                      |
| `{{SCHEMA}}`   | `.herdr/status.schema.json` (§5.1)                                                                |
| `{{LAUNCHER}}` | the binary's absolute path, so that a coordinating role can run `{{LAUNCHER}} add <role>` itself |

**Agent invocation, fixed in the code:** `herdr agent start <name> --kind claude --pane <P> --timeout 60000 -- -n <name> --append-system-prompt-file <file> [<start_message>]`. There are no `kind` or `args` keys; they come when a project has a role that is not Claude Code (§11).

### 2.3 Validation and messages

Every table is parsed with `toml` and `serde` with `deny_unknown_fields`, so an unknown key is an error, never ignored. Then come our own checks, all before calling herdr:

| Rule                                                                                                                                          | Message (example)                                                            |
| --------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------- |
| Valid TOML and known keys                                                                                                                     | `.herdr/crew.toml:14:1: unknown key "worktre" in roles[1]`                   |
| `version = 1`                                                                                                                                 | `version 2 is not supported by herdr-crew 0.1.0 (supports 1)`                |
| A role name has the shape of a herdr agent name, `[a-z][a-z0-9_-]{0,31}` [vendor]; with `extra`, at most 29 characters to leave room for `-NN` | `roles[0].name "Lead_1": must match [a-z][a-z0-9_-]{0,31}`                   |
| Unique names; none of the form `<extra role>-N`; `board.tab` differs from every role name                                                     | `roles[3].name "globex-dev-2" clashes with the extra instances of globex-dev` |
| At least one role; `board.writer` is a role                                                                                                   | `board.writer "boss" is not a role in roles[]`                               |
| `worktree = true` requires `[worktrees]`; `base` looks like `remote/branch`                                                                   | `roles[1] globex-dev asks for a worktree but [worktrees] is missing`         |
| `start_message` (top level and per role): no control character (so one line), at most 200 bytes, not starting with `-`                        | `.herdr/crew.toml:3:17: start_message: must be one line, without control characters` |
| Every `{{…}}` in `common_prompt` and each `roles[i].prompt` is a known placeholder                                                            | `.herdr/crew.toml:22:9: roles[0].prompt: unknown placeholder {{LAUNCH}}`     |

- **Format.** Every message starts with `herdr-crew:` and the absolute path of `crew.toml`, with line and column when they are known.
- **Placeholder positions.** An unknown placeholder is located in the source text of its prompt, so the line and column point at it inside `crew.toml`.
- **All errors at once.** Every validation error is reported, not only the first.
- **`herdr-crew check`.** It validates the configuration, checks that `git`, `herdr` and `claude` are on the `PATH`, prints the official installer of what is missing without running it, and prints the binary's version and location. If a herdr server answers, it also explains that `herdr` only attaches and prints the full binary path for `up --no-attach`.

## 3. Manifest

```toml
id = "herdr-crew"
name = "herdr crew"
version = "0.1.0"
min_herdr_version = "0.9.1"
platforms = ["macos", "linux"]

[[build]]
command = ["sh", "scripts/build.sh", "--install-launcher"]

[[startup]]
command = ["bin/herdr-crew", "startup"]

[[actions]]
id = "up"
title = "Start or complete the project's sessions"
contexts = ["workspace", "pane"]
command = ["bin/herdr-crew", "up"]
```

The id is `herdr-crew`, like the repository; the action is `herdr-crew.up`. The `[[startup]]` hook runs `startup` (§4.4).

herdr reads a linked plugin's manifest from its `manifest_path` when the server starts, not only the copy in `plugins.json`: a `[[startup]]` added to a manifest after `plugin link` ran at the next server start, while `plugins.json` still lacked it [verified]. An already linked checkout needs no new `link`.

**Variants are not actions.** herdr actions take no parameters [vendor: issue #3603, _not planned_]. What a user starts by hand is "bring the project up".

- `add` and `close` are run by the coordinating role from its shell with `{{LAUNCHER}}`, once the user agrees. So the manifest declares a single action, `up`, and `add`/`close` are subcommands of the same binary.
- A picker pane (an `overlay` `[[panes]]` listing "add <extra role>" and "close <instance>") is deferred until a user wants it from a shortcut (§11).
- There is no `[[panes]]`: the board runs in a normal tab (§4). That avoids three problems:
  - a `popup` starting with the server's `cwd` (#2050);
  - `pane open --placement tab --focus` not moving the view (#4283);
  - an unlabelled tab, since `plugin pane open` takes no label [verified: `PluginPaneOpenParams`].
- `selected_text` is not used, so #3380 does not apply.

The `command` is a path relative to the plugin directory, where herdr runs the commands [vendor]. herdr resolves a relative `argv[0]` against that directory and also uses it as the current directory [verified with `plugin link` and `plugin action invoke` in an isolated XDG: the process got an absolute `argv[0]` under `HERDR_PLUGIN_ROOT`, and that `cwd`].

The manifest declares only macOS and Linux. Artifact preparation uses a POSIX shell and the launcher uses Unix `exec`. The runtime keeps its Windows branches; adding Windows support requires a platform-specific build step, executable paths and lifecycle validation. Action ids are unique even across platforms [vendor], so one action per platform is not possible.

## 4. Launcher behaviour

### 4.1 Subcommands

- `init [--preset solo|review|parallel|research] [--name NAME] [--base REMOTE/BRANCH] [--shared-checkout] [--no-ignore] [--yes] [--dry-run]`: initial setup without loading an existing configuration (§4.1.1).
- `up [--no-attach] [--dry-run]`
- `add <role>`
- `close <name>`
- `board [--file PATH] [--once] [--interval S]`
- `check`
- `startup`: only herdr's `[[startup]]` hook runs it (§4.4); it takes no option, not even `--root`.

Global option: `--root DIR`. `--dry-run` prints the plan (§6.2) without running it; it is meant for migrations (§9) and debugging.

### 4.1.1 Initial setup

`init` resolves the main Git checkout like other commands but runs before `load_config`. Its line-based wizard asks for a project prefix, one of four software workflows, an optional remote worktree base and ignore rules. `--yes` uses explicit arguments and defaults without reading stdin; `--dry-run` previews the generated configuration and missing ignore entries without writing.

The generated TOML uses the existing version-1 schema and passes `Config::parse` before any write. Solo development is the default. Reviewed delivery separates coordination, implementation and review; parallel delivery uses two developers and requires worktrees; investigation uses two read-only researchers. The mappings and evidence are in [workflows.md](workflows.md). These are prompt protocols, not runtime task orchestration or messaging.

Worktree defaults come from locally available remote refs, preferring the current upstream and then origin's default branch. Explicit bases require a configured remote and valid branch syntax; setup does not fetch or check the remote server. Shared-checkout review is supported, with one source-code editor and read-only review. Workers in isolated worktrees are instructed to confirm their assigned base, create a branch before committing and validate the exact delivered commit.

The wizard previews the result and asks before saving. It refuses an existing configuration, including a symlink, and opens a new file with `create_new` so a concurrent file is preserved. It appends only missing generated-file ignore entries, without ignoring `crew.toml`; `--no-ignore` disables that update. Cancellation or EOF before saving creates no files. If updating `.gitignore` fails after saving, it reports the valid configuration already created and requests a manual ignore update. It starts no sessions or worktrees and changes no Git refs.

herdr is invoked as `$HERDR_BIN_PATH` when set (herdr actions), else as `herdr` from the `PATH`. Every answer is read as JSON: `.result` on success, `.error.code` on error [vendor]. Some commands, such as `pane run`, print nothing on success [verified].

### 4.2 `up`: calls in order

1. **Server.** `herdr workspace list`. If it does not answer and the process is **not** running as an action, it starts `herdr server` and retries `workspace list` every 250 ms for up to 10 s. The server is started:
   - with its `cwd` in the root;
   - without `CLAUDECODE`, `CLAUDE_CODE_CHILD_SESSION` or `CLAUDE_CODE_ENTRYPOINT` in its environment. A server spawned from a Claude Code session would otherwise start every `claude` as a child session without a saved transcript [verified];
   - with the three streams set to null;
   - in its own process group (Unix) or detached (Windows).
2. **State.** It finds the workspace by `label` in the answer of `workspace list`. When it exists, it runs `herdr tab list --workspace <W>` and `herdr pane list --workspace <W>`; it always runs `herdr agent list`. That makes `HerdrState`:
   - tabs by label;
   - panes by tab, with their `agent_session` when herdr reports it;
   - live agents with `name`, `tab_id` and `pane_id`.
3. **Plan.** `plan(config, state)` returns the steps (§6.2), applying these rules in this order:
   - **Occupied tab.** A tab is occupied if some agent in `agent list` has its `tab_id`, **named or not**. Sessions started by hand have `name: null` [verified], so the name is not enough.
   - **Known tabs.** These are the roles' tabs, the `board.tab` tab and those of the form `<role with extra>-N` with N ≥ 2: the same rule as §2.3 and §5.1.
   - **Foreign agent in the workspace.** If a live agent sits in an unknown tab of the project's workspace, `up` starts **no** agent at all.
     - It warns, naming that tab, and asks for it to be renamed with its role's label (`herdr tab rename <tab> <role>`).
     - Missing roles get no tab either; their verdict is `leave alone (foreign agent in "<tab>")`.
     - It may still create the board tab.
     - A foreign tab without an agent is ignored.
   - **Each base role, in order:**
     - if a live agent carries its name in any workspace, nothing happens (names are global [vendor]);
     - if a tab with its label exists and is occupied, nothing happens;
     - if a tab with its label exists without an agent, nothing is relaunched, because relaunching blindly would open a new conversation. It warns "tab X has no agent; restart it there with `claude -r <agent_session.value> -n X`" when `pane list` still reports that value for its pane, and, when it does not, "tab X has no agent and herdr knows no session; restart it there with `claude -r X` and pick its conversation, or, if it never had one, close the tab and run `herdr-crew up`". Roles start with `-n <role>`, so `claude -r <role>` opens the picker searching for that name: it lists every conversation with that name, older ones included, and the user picks one. That the search term matches the `-n` name is [H]: `claude --help` only says "open interactive picker with optional search term", and it was not tried, because trying it runs a real `claude` over the user's conversations;
     - otherwise the tab is created and the agent started.
   - **Board.** Without a `board.tab` tab it is created; with one, it is repaired (step 9). Extra instances are left alone.
4. **Workspace**, when missing: `herdr workspace create --cwd <root> --label <label> --no-focus` → `.result.workspace.workspace_id`, `.result.tab.tab_id`, `.result.root_pane.pane_id` [verified].
   - The initial tab is reused with `herdr tab rename <tab> <name>`. It goes to the first role to create that works in the root (no worktree), or to the board if every role works in a worktree.
   - Then all tabs are created (steps 5 and 6), and only after that are the prompts written and the agents started (steps 7 and 8). That gives the new shells time to reach their prompt.
5. **Worktree**, when the role asks for one and `<dir>/<name>` does not exist:
   - `git -C <root> fetch --quiet <remote> <branch>`; on failure it warns and goes on with the local ref;
   - then `git -C <root> worktree add --quiet --detach <dir>/<name> <base>`.

   An existing worktree is reused as it is.
6. **Tab** of each remaining role: `herdr tab create --workspace <W> --cwd <root or worktree> --label <name> --no-focus` → `.result.root_pane.pane_id`.
7. **Prompt**: writes `.herdr/prompts/<name>.txt` (§2.2).
8. **Agent**: the fixed invocation of §2.2.
   - On `agent_pane_busy` (the pane's shell has not reached its prompt yet) it retries every second for up to 30 s.
   - On `agent_not_ready` (for example the folder-trust dialog) it warns and goes on.
   - Any other error stops the plan. Stopping is safe because `up` is re-entrant: the next run creates only what was missing.
9. **Board.** Before the tab itself:
   - It writes `.herdr/status.schema.json` (§5.1) when missing or when its content differs from the generated one, so that an `up` with no changes has no steps.
   - It seeds `board.file` when missing, with empty `sessions` and `owner`, as an atomic write (`.tmp` and rename).
   - **Missing tab:** it is created as in step 6.
   - **Existing tab: repair.** It takes the tab's pane and asks `herdr pane process-info --pane <P>`; the positional form `pane process-info <id>` is rejected (`unknown option`) [verified].
     - It types only if the shell itself is in the foreground; anything else (the viewer, an editor, a hung process) gets nothing typed. The typical case is a herdr restart, which restores the tab as an empty shell because it does not resume processes that are not agents [vendor].
     - The shell is in the foreground when `foreground_process_group_id == shell_pid` [verified in the real test (§7.2 item 2): with the shell idle both are the shell's `pid`; with `cat` running, the group becomes `cat`'s].
     - The other candidate check was discarded: all `foreground_processes` having the shell's `pid`. While the shell draws its prompt, `foreground_processes` includes the prompt's own processes (`starship`, `git`) in the shell's group, so it failed with the shell ready [verified]. Typing at that moment is safe, because the text waits for the prompt.
   - **Command.** After 1 s if the tab is new, it runs `herdr pane run <P> "<binary> board --root <root> --file <absolute path>"`, quoted for the platform's shell (`'…'` on Unix, `& '…'` in PowerShell). `--root` keeps the viewer independent of the pane's current directory, which the user may have changed before a repair.
10. **Focus**: `herdr workspace focus <W>`.
    - It is not a plan step: the adapter always does it at the end, so "plan: no steps" does not count it.
    - From a plain terminal (no `HERDR_ENV=1`, no `--no-attach`, stdin and stdout on a TTY) it then opens the UI: `exec herdr` on Unix, a waiting `herdr` on Windows.

**`add <role>`**:

- It requires a role with `extra = true` and an existing workspace.
- The number is the highest among the `<role>` (which counts as 1) and `<role>-N` labels of `herdr tab list --workspace <W>`, plus one.
- Then it runs steps 5 to 8 with that name and prints the name it assigned.

**`close <name>`**:

- It requires the form `<extra role>-N`, finds the tab by label and runs `herdr tab close <tab>`.
- It keeps the worktree, which may hold unpushed work, and prints its path.
- It also says that a later `add` that gives the same number again reuses that worktree (step 5 reuses an existing worktree).

**Test variable:** `CREW_AGENT_CMD` replaces `agent start` with `herdr pane run <P> <cmd>` (for example `cat`). The real test isolates herdr through XDG (§7.2), where a test `crew.toml` with its own label is enough.

### 4.3 Reporting failures

- **Output.** Each step is printed on stdout (`herdr-crew: globex-dev in w3:p7`), and each warning or error on stderr.
- **Exit codes.** 1 if the plan stopped; 2 if the command line or the configuration is invalid.
- **As an action.** herdr keeps stdout, stderr (up to 64 KiB) and the exit code in `plugin log list` [vendor]. The action runs without a visible terminal, so it also calls `herdr notification show "herdr-crew" --body "<message>"` on an error, an `agent_not_ready` or a step-3 warning. That way the user sees it without opening the log. `startup` is the exception: it writes only to the log (§4.4).

### 4.4 `startup`: the `[[startup]]` hook

With the plugin linked, `cd repo && herdr` brings the project up with nothing else to type. The project is the one herdr was launched from; there is no global list of projects.

**What herdr does** [verified on 2026-09-26 with herdr 0.9.1 in an isolated XDG]:

- A `herdr` client that finds no server starts one that creates an **initial workspace** in the client's directory: label = the directory's name, one tab «1», one pane in that directory (server log: `created startup workspace cwd=…`). The client hands the directory over in `HERDR_STARTUP_CWD`; a headless server started with that variable does the same.
- `[[startup]]` runs once per server, **after** that workspace exists (0.35 s later in the check), in the plugin directory, with `HERDR_PLUGIN_CONTEXT_JSON` naming it: `workspace_id`, `workspace_label`, `workspace_cwd` = the launch directory, `invocation_source: "startup"`.
- A headless `herdr server`, as `up` starts it (§4.2 step 1), creates **no** workspace; startup then runs with a context without workspace (`{"invocation_source":"startup",…}`), and a client attaching later to that empty server creates none either.
- A client attaching to a running server, from any directory, creates no workspace and runs no hook.
- On a restart with a saved session, herdr restores the workspaces, ignores the launch directory (log: `restored session already has workspaces; ignoring startup cwd`) and runs startup once, with the **focused** workspace as its context. Restored workspaces fire no `workspace.created`.
- Plugin commands inherit the server's environment (`CREW_AGENT_CMD` reached the hook in the real test).

**Roots.** `startup` lists the workspaces and takes each one's **first pane** `cwd`; the root is its verified main Git checkout (§2.1) when `<root>/.herdr/crew.toml` exists. Roots are deduplicated. The hook's own directory and `workspace_cwd` are not used to find roots.

**Per root**, with its configuration and the state of §4.2 step 2:

1. **Adoption**, the only path that creates a project from nothing. The context's workspace is adopted only if all of these hold:
   - it is the context's workspace, and it is the only workspace in herdr, so no other one carries the project's label;
   - its label is its pane's directory name, and it has one tab, labelled «1», with one pane;
   - that pane has no `agent_session`;
   - the pane's `cwd` is the root, both with symbolic links resolved;
   - the pane's shell is in the foreground (§4.2 step 9), waiting up to 2 s while the shell sources its rc files.

   Then the plan is `up`'s for a missing workspace, with `AdoptWorkspace` instead of `CreateWorkspace`: it renames the workspace to the project's label (unless the directory already has that name) and takes tab «1» as the initial tab, which `RenameTab` gives to the first role without a worktree (§4.2 step 4). The rest follows `up`: tabs, agents, board. A directory whose name is the project's label is adopted too, without renaming.
2. **Repair**, when adoption does not hold and a workspace with the project's label exists: only the board is repaired. It is typed again only when its pane is a bare shell (§4.2 step 9). A role tab that exists is never relaunched, occupied or not (the `no agent` verdict only warns, §4.2 step 3). A missing tab, of a role or of the board, is not created: it may have been closed on purpose, and recreating it on every restart would open a fresh conversation. Its verdict is `leave alone (missing after a restore)`, with the warning "tab X is missing; `herdr-crew up` creates it"; an explicit `herdr-crew up` creates it.
3. **Nothing**, otherwise: a project whose workspace was closed is not brought back. The reason goes to the log.

**Output.** For each project it processes, `startup` first prints its verdicts on stdout in the `--dry-run` format (`herdr-crew: <label>`, then one line per role and one for the board), so the log shows what it saw. Then come the steps on stdout and the warnings on stderr, all kept by herdr in `plugin log list`. Warnings never become notifications. On the adoption path the user is watching herdr open, so an error that makes `startup` exit 1 (an invalid `crew.toml`, an agent that fails to start) is also shown with `herdr notification show`. A restore stays fully silent, errors included. It focuses the adopted workspace and nothing else, and never attaches. It exits 0 when there is nothing to do and 1 when a root failed; the other roots still run.

**Cold `up`.** When `up` starts the server itself, that server is headless and creates no workspace, so startup finds nothing to adopt and does nothing, and `up` plans as always. There is no race between the two and `up` does not wait for startup [verified: `up --no-attach` with the plugin linked left one workspace, and startup's log was empty with exit 0]. If herdr ever made a headless server create the initial workspace, `up` would stop planning when it has just started the server with the plugin linked and enabled, and leave the bring-up to startup; that is not built.

**Limits**, both covered by running `herdr-crew up` in the project:

- launching `herdr` from project E while a restored session has no workspace for E: herdr ignores the launch directory on restore, so startup never sees E;
- attaching to a server that is already running: no hook runs.

The server receives `HERDR_STARTUP_CWD` when it starts from a client, but the startup plugin process does not receive it [verified with the isolated real-herdr test]. A plugin-only startup change therefore cannot detect project E in the first case. Herdr would need to pass the launch directory to the hook and run a hook on client attach to cover both automatically.

## 5. The board

### 5.1 `status.json` and its schema

Fields:

| Field        | Content                                                         |
| ------------ | --------------------------------------------------------------- |
| `version`    | always 1                                                        |
| `updatedAt`  | time of the last write                                          |
| `updatedBy`  | the writer                                                      |
| `sessions[]` | one entry per session: `role`, `state`, `now`, `next`, and optionally `note` and `updatedAt` |
| `owner`      | what the sessions wait for from the user: `urgent`, `beforeMain`, `optional` |
| `recent[]`   | optional: the latest events, each with `at` and `text`          |

Three things are derived from the configuration:

- `updatedBy` is `const: <board.writer>`;
- `role` is a role name or `<extra role>-N` with N ≥ 2, with unique roles and no `maxItems`;
- the order is `roles[]`, each extra instance after its base role by number.

A repeated `role` is rejected. `$schema` is accepted with any value, so boards that point at an older schema file still load.

The JSON Schema (2020-12) is generated from the configuration into `.herdr/status.schema.json` by `up`, and the seeded file points at it with `"$schema": "status.schema.json"`. Its descriptions are the instructions for the agent that writes the board. `{{SCHEMA}}` names it in the prompts. Each project adds `/.herdr/status.schema.json` to its `.gitignore`.

### 5.2 Validation in the viewer

There is no JSON Schema validator:

- `serde` with typed structs and `deny_unknown_fields` covers types, required fields, the `const` of `version` and the `state` enum.
- Our own checks follow: `updatedBy` equals `board.writer`, `role` is valid and unique, required strings are not empty, and dates are RFC 3339 **with** an offset (a date without an offset fails).

The generated schema is the instruction for the writer, and the validator is what the viewer decides with. They may diverge; that is accepted as long as a divergence does not affect the writer (§10). Targeted accept and reject tests on `board::load` pin the cases that matter (§7.1).

### 5.3 Drawing

1. The header "Project <root directory name>", in cyan.
2. One box per session, in the order of §5.1:
   - title `<role>  ● <state>`, with `working` green, `waiting` yellow, `idle` dim and `blocked` red, and the border in the same colour;
   - rows "Now", then "Next" (numbered) when there is a queue, then "Note" in italics when present;
   - the entry's age at the bottom right: `<1 min ago`, `N min ago`, `N h MM min ago`, `N d ago`.
3. "Waiting on you":
   - "Urgent" (red), "Before main" (yellow) and "Optional" (dim), only the non-empty ones, as bullets;
   - "Nothing." when all are empty;
   - a red border when something is urgent, cyan otherwise.
4. "Recent", when present: up to five lines `MM-DD HH:MM  text`, in local time.
5. The footer "Updated YYYY-MM-DD HH:MM by <writer> (… ago)", then "<path>  ·  checked HH:MM:SS".

When the file is missing, half-written or off-schema, the yellow "Status unavailable" box replaces items 2 to 5, with one of these reasons:

- "<path> does not exist. <writer> creates it when it updates the status.";
- "Unreadable JSON in <path>: <error with line and column>";
- "Does not match the schema:", with up to five errors `• <field path>: <message>`.

Content taller than the terminal is cut at the bottom.

### 5.4 Refresh and keys

- **Screen.** Alternate screen and raw mode, restored also on a panic.
- **Loop.** It waits for key events with `crossterm::event::poll` and a 1.5 s timeout (`--interval`). On every turn it compares the file's `mtime` and size, and redraws when:
  - they changed;
  - 30 s passed since the last draw, to refresh the ages;
  - the terminal was resized.
- **Keys.** `q`, `Esc` and `Ctrl-C` quit; `r` rereads and redraws.
- **`--once`.** It draws once to stdout without the alternate screen and exits, for tests and diagnosis.

## 6. Rust architecture

### 6.1 Crate and dependencies

One crate with one binary, `herdr-crew`, and subcommands:

```text
herdr-plugin.toml  Cargo.toml  Cargo.lock  README.md  LICENSE  docs/design.md
src/main.rs            arguments, dispatch, exit codes (adapter)
src/config.rs          parsing and validation of crew.toml (pure)
src/prompt.rs          placeholders and the combined prompt (pure)
src/plan.rs            HerdrState, Step and plan(config, state, env) (pure)
src/board/model.rs     status.json: types, validation, order, age (pure)
src/board/schema.rs    JSON Schema generated from the configuration (pure)
src/board/view.rs      drawing into a ratatui::Frame (pure)
src/board/tui.rs       event loop and polling (adapter)
src/herdr.rs           runs the herdr CLI, reads its JSON, runs Steps (adapter)
src/git.rs             main root, fetch and worktree add (adapter)
tests/                 fixtures, snapshots and the real test of §7.2
```

| Crate                        | Why                                                                                                                                                                                                                                                                                 |
| ---------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `ratatui` (with `crossterm`) | Boxes with titles, colours, clipping, and `TestBackend` to test drawing without a terminal. `crossterm` is its default backend and works on Windows. Default features are off (calendar, macros); it enables `crossterm` and `unstable-rendered-line-info`, which `Paragraph::line_count` needs. |
| `serde`, `serde_json`        | Types of `status.json` and of herdr's answers; `deny_unknown_fields` replaces a JSON Schema validator; `json!` builds the schema.                                                                                                                                                  |
| `toml`                       | `crew.toml`, with line and column in errors and the spans of keys and prompts.                                                                                                                                                                                                      |
| `jiff`                       | RFC 3339 with a required offset, and conversion to local time, which `std` lacks.                                                                                                                                                                                                  |

Deliberately left out:

- `clap`: five subcommands with at most two arguments are read from `std::env::args` in a few dozen lines;
- `anyhow`: an error `enum` with its message is enough;
- `tokio`: everything is sequential;
- a socket client: the CLI is the documented plugin API [vendor];
- `jsonschema`, neither at runtime nor in tests: it pulls a large dependency tree for a validation that `serde` and a dozen checks cover [E].

### 6.2 Pure core and adapter

The core runs no processes and reads neither the clock nor the disk except through its arguments:

- **`Config::parse(text, root) -> Result<Config, Vec<Error>>`.**
- **`plan(&Config, &HerdrState, &Env) -> Plan`.**
  - `Env` carries the test command, the binary's path, which worktrees exist and what runs in the foreground of the board pane.
  - `Plan` carries the verdicts and a list of `Step`: `CreateWorkspace`, `AdoptWorkspace`, `RenameTab`, `CreateWorktree`, `CreateTab`, `WritePrompt`, `StartAgent`, `RunInPane`, `SeedStatus`, `WriteSchema` and `Warn`. Focus is not a step (§4.2 step 10).
  - The workspace and pane ids in the steps are symbolic references to the result of an earlier step.
- **`board::load(bytes, &Config) -> Result<Status, Unavailable>` and `board::view(frame, Status | Unavailable, now, path)`.**

The adapter asks `pane process-info` about the board pane before calling `plan`. It then runs each `Step` in order, resolves the references with the real answers, and applies the retry policy of §4.2 step 8.

`--dry-run` prints:

- one line per role and one for the board, with its verdict (`create`, `occupied`, `no agent`, `repair`, `leave alone`);
- the warnings;
- the `Step`s, with unresolved references.

Without a server it does not start one: it says so in one line and plans against an empty herdr.

## 7. Tests

### 7.1 Unit tests (`cargo test`)

All fixtures are synthetic: a project `acme` with four roles in the main checkout, and a project `globex` with worktrees and extra instances.

- **Configuration:**
  - both example `crew.toml` are accepted;
  - one case per row of the §2.3 table, checking the message and, for placeholders, the line and column;
  - the default of `board.file`;
  - `kind` and `args` are unknown keys;
  - a prompt given as a file list is rejected;
  - basic strings are accepted too;
  - `start_message` at the top level and per role (an empty role value means none), with accents and «»; a newline, a tab, an escape or a DEL, more than 200 bytes of accented text and a leading `-` are errors with their line and column.
- **Prompts:** the order (role, then common), every placeholder, and unknown placeholders with their offsets.
- **Plan:**
  - an empty state creates workspace, rename, tabs, agents and board, in that order; a complete workspace has no steps (idempotency); a missing role tab gives only that tab and its agent;
  - a **project migrating from launcher scripts** is a fixture: four role tabs with `name: null` agents, a board tab still labelled "status", and a leftover tab "6" without an agent. It starts no agent. With the tab renamed to `board.tab`, it has no steps, and the `--dry-run` output is pinned;
  - a role tab occupied by an unnamed agent gives nothing;
  - a live agent in a foreign tab of the workspace gives no `StartAgent` and a `Warn` naming the tab;
  - a foreign tab without an agent is ignored;
  - with `globex-dev-2` alive and the base tab `globex-dev` missing, the base tab is created and its agent started, with no foreign-agent warning;
  - a role tab without an agent gives only a `Warn`, with `claude -r <value>` when the pane reports `agent_session` and `claude -r <role>` otherwise;
  - the verdict lines are the head of the `--dry-run` output, and `startup` prints them under a `herdr-crew: <label>` header;
  - a board tab with the shell in the foreground gives `RunInPane`; with another process, nothing;
  - an agent with the role's name alive in another workspace gives nothing;
  - another project's workspace with same-named tabs is ignored;
  - `CREW_AGENT_CMD` replaces `agent start`;
  - `add` numbers after `-2` and `-5` as `-6`;
  - `close` rejects a base role.
- **Startup (§4.4):**
  - herdr's fresh initial workspace is adopted (`AdoptWorkspace`, then `RenameTab` to the first role, the other tabs, agents and board), renamed when its directory is not called like the project and not renamed when it is;
  - each refusal separately: two workspaces (also when the other one carries the project's label), a renamed workspace or tab, a pane with `agent_session`, a pane outside the root, a busy shell, no context;
  - a restored workspace is only repaired: no `StartAgent`, no tab, the board typed in its bare shell; a restored workspace missing a role tab gets no `CreateTab` or `StartAgent` for it, only a warning, while an explicit `up` creates it; a project without its workspace is not recreated;
  - `startup` warnings never become notifications.
- **Board drawing:** `TestBackend` at 100×40, comparing the buffer's text (no colours) with snapshots in `tests/snapshots/`. Cases:
  - each project's example;
  - a busy board;
  - a missing file;
  - truncated JSON;
  - an unknown `state` (the message includes the value);
  - a date without an offset;
  - an `updatedBy` other than the writer;
  - the order with extra instances.

  The time is fixed by argument.
- **Board loading:** `board::load` accepts the example boards and rejects a repeated role, an unknown role, `<base role>-2` of a role without `extra`, and an unknown field.

### 7.2 Optional, against a real herdr isolated through XDG

A test marked `#[ignore]` that runs only with `CREW_REAL_HERDR=1`. It isolates herdr as follows:

- `XDG_CONFIG_HOME` and `XDG_STATE_HOME` point to a temporary directory, because herdr keeps `plugins.json` and the session sockets under the XDG directories [vendor];
- none of the caller's `HERDR_*` or `CLAUDE*` variables are passed;
- `herdr --session crewtest server` runs in the background;
- it aborts unless `herdr status` shows a socket under the temporary directory.

On a temporary git repository with its own test `crew.toml`:

1. `up --no-attach` with `CREW_AGENT_CMD=cat`. It checks the tabs with `tab list` and the board with `pane read`, then runs `up` again and checks that nothing was duplicated.
2. It quits the viewer with `pane send-keys <P> q` and runs `up` again: the viewer comes back (repair). With `cat` running in the board pane, `up` types nothing.
3. `add` and `close` of an extra instance. A later `add` gives the same number and reuses the worktree, and `close` of a base role fails.
4. `herdr plugin link` of the plugin directory (it writes the temporary `plugins.json`), then `herdr plugin action invoke up --plugin herdr-crew` from a test pane, then `plugin log list`.
5. **Startup (§4.4)**, in a second session `crewstart`: it links the plugin, starts the server with `HERDR_STARTUP_CWD` in the repository (like a client launched there) and checks that startup leaves one workspace, `crewtest`, with the role tabs and the board and no tab «1». Then it stops the session, starts the server again and checks that the restored workspace gets its board back while `ct-lead` stays a bare shell.
6. `herdr session stop` and `session delete` of each session, reaping the server process. It checks that `~/.config/herdr/plugins.json` did not change. `server stop` is never run without `--session`.

**Never a real `claude`, by construction.** Shadowing it in the `PATH` is not enough: a shell's startup files (`~/.zprofile`, `~/.profile`, `path_helper` through `/etc/profile`) can put it back first. Once a zsh whose startup files put `~/.local/bin` first launched the real `claude`, which stopped at its folder-trust dialog (`agent_not_ready`) until the tab was closed [verified]. So:

- every process the test starts gets a minimal `PATH`, not the caller's: the fake `claude`'s directory, a directory with links to the `herdr` and `git` that `which` finds at setup, and `/usr/bin:/bin`. The links, instead of their directories, keep out a directory that also holds the real `claude` (`~/.local/bin` holds both `herdr` and `claude`);
- `SHELL=/bin/sh` for the server and its panes;
- before the first `agent start`, a guard opens a tab and runs `command -v claude > <dir>/which-claude` in its shell, and aborts the test unless it names the fake. It checks the shell that will really launch the agent, whatever its startup files do.

The fake `claude` writes its arguments; one `add` without `CREW_AGENT_CMD` checks that `start_message` arrives as the last argument, whole. The plain `herdr` client in a terminal was checked by hand in the same isolation (a pty from a directory not called like the label, with zsh): startup adopted and renamed the workspace, and left the roles and the board [verified].

It is both the integration test of step 1 and the check of the [H] (§11). It passed on 2026-09-26 on macOS with herdr 0.9.1 [verified]:

- **XDG isolates on macOS.** `plugin link` wrote `plugins.json` under the temporary directory, the user's `~/.config/herdr/plugins.json` was unchanged, and `herdr plugin list` in the normal environment still said "No plugins installed."
- **The temporary directory needs a short name.** macOS limits a socket path to 104 bytes including the trailing NUL (`sun_path`). The longest path is the client socket, `<XDG_CONFIG_HOME>/herdr/sessions/crewtest/herdr-client.sock`. Measured:
  - a 153-byte path failed: the server did not start ("local socket name length exceeds capacity of sun_path");
  - at 107 bytes, the server opened the API socket (100 bytes) and then exited when it opened the client socket;
  - at 102 bytes, it worked.

  The test uses `<temp_dir>/crXXXX` without `canonicalize`, which prepends `/private` on macOS and pushed the path to 109 bytes. It aborts before starting anything if the path is longer than 103 bytes.

## 8. Build and distribution

- **When `[[build]]` runs.** Only on `herdr plugin install` from GitHub, never on `plugin link`, and it gets no execution context [vendor].
- **Artifact preparation.** `sh scripts/build.sh` builds both Rust binaries with the locked dependencies and copies the runtime to `bin/herdr-crew` via a temporary file and rename. The manifest points there. This runs before linking and after each development change; it installs nothing outside the checkout unless passed `--install-launcher`.
- **Public CLI.** GitHub installation passes `--install-launcher`. It copies `herdr-crew-launcher` into `CREW_BIN_DIR` or `~/.local/bin` as `herdr-crew`. For plugin installation, an explicitly set `CREW_BIN_DIR` must be absolute and nonempty; validation runs before compilation or artifact writes so checkout relocation cannot invalidate the destination. `--bin-dir DIR` overrides the environment when installing it directly. Installation replaces only a regular file carrying the launcher's ownership marker; unrelated commands and symlinks are preserved. The launcher reports a missing PATH entry without editing shell profiles.
- **Resolution.** Every ordinary public CLI invocation runs `herdr plugin list --plugin herdr-crew --json`, reads `plugin_root` and executes `bin/herdr-crew`, falling back to `target/release/herdr-crew` for older plugins. It preserves arguments, environment and working directory. Startup and actions invoke the runtime directly. `{{LAUNCHER}}` continues to use the canonical runtime path.
- **Offline use.** herdr 0.9.1 and 0.9.3 fall back to their local plugin registry when no server answers [vendor, inspected 2026-10-05]. Resolution does not start a server. The runtime's `up` keeps its own server-starting behaviour.
- **Removal.** `herdr-crew --uninstall-launcher` removes only an owned launcher, even without a registered plugin. Without an explicit directory or `CREW_BIN_DIR`, an installed launcher removes itself. `herdr plugin uninstall` removes the managed checkout separately.
- **Standalone use.** The runtime at `bin/herdr-crew` also works without plugin registration. `cargo build --release --locked` still produces the runtime in `target/release`, but does not prepare the manifest artifact or install the public launcher.

herdr builds in a temporary checkout, then moves it and registers the plugin [vendor, inspected in 0.9.1 and 0.9.3]. A public symlink into the build directory would break. Copying a resolver instead avoids embedding that path and follows reinstalls or a linked checkout. Installation spans two destinations: a host registration failure can leave the launcher behind. It resolves the previous registered plugin when available, or reports that installation is needed; it can remove itself independently.

`tests/launcher.rs` exercises installation, relocation, registry replacement, legacy binaries, forwarding, ownership and removal using temporary tools and prefixes. Its ignored real-herdr test validates offline linking and resolution under isolated XDG directories, without starting a server.

Without `cargo` it cannot be installed yet. Prebuilt binaries per platform are deferred until the first user without a Rust toolchain (§11). That work means publishing them in GitHub releases and having the build step download the binary matching the chosen ref and verify its checksum, keeping compilation as an alternative.

## 9. Migrating a project from launcher scripts

A project that runs role sessions with its own scripts migrates in one change:

1. **Write the config.** Create `.herdr/crew.toml`, moving each role's prompt text into `prompt` and the shared text into `common_prompt`, with the same placeholders. Give the board tab a project-prefixed label (for example `acme-status`); an unprefixed label such as `status` is discouraged by invariant 3.
2. **Rename live tabs** whose labels differ from the configuration: `herdr tab rename <tab> <label>`. Leftover tabs without an agent can stay; they are ignored.
3. **Dry-run.** With the plugin linked, `up --dry-run` must print only `occupied` or `leave alone` verdicts and no `Step`. For example, a project whose sessions were started by hand and whose old viewer still runs:

   ```text
   acme-lead      occupied (unnamed agent in w3:p1)
   acme-reviewer  occupied (unnamed agent in w3:p2)
   acme-designer  occupied (unnamed agent in w3:p3)
   acme-dev       occupied (unnamed agent in w3:p4)
   acme-status    leave alone (in the foreground: python3, uv)
   plan: no steps
   ```

   If a `StartAgent` or a foreign-tab warning appears, stop the migration.

4. **Swap the viewer.** Quit the old viewer in the board tab and run `up`. The pane is left with the shell in the foreground, and `up` repairs it with the new viewer over the real `status.json`, creating no tab.
5. **Clean up** in the same change: delete the old scripts and viewer, and add `/.herdr/status.schema.json` and `/.herdr/prompts/` to `.gitignore`. Existing worktrees under `[worktrees].dir` are reused.

## 10. Accepted risks and discarded alternatives

**Accepted risks:**

- **Global registry.** herdr's plugin registry is global to the user (`~/.config/herdr/plugins.json`) [vendor]. The global part holds no project data, and linking is the user's decision.
- **Permissions.** The plugin runs with all of the user's permissions and can type into sessions [vendor].
- **Windows unverified.** The implicit `.exe` on Windows is still [H] (§3), and so is the PowerShell quoting of `pane run`; Windows is not tested yet. The other step-1 hypotheses were verified on 2026-09-26 (§2.1, §3, §4.2, §7.2): relative `argv[0]`, the context of a CLI-invoked action, the foreground of `pane process-info`, the name reservation and XDG on macOS.
- **Concurrent `up` or `add`** (two invocations at once on the same project): there is no lock.
  - Live agent names are unique in herdr [vendor], so the second `agent start` with the same name fails, and so does the second `git worktree add` on the same path.
  - What is left is an orphan tab without an agent; the user closes it and the next `up` reports it.
  - herdr reserves the name as soon as it accepts `agent start`: a second `agent start` with the same name, 0.3 s later and with the first one still pending, failed with `agent_name_taken` [verified].
- **Unstable ratatui feature.** A box with wrapped text gets its height from `Paragraph::line_count`, which needs ratatui's unstable `unstable-rendered-line-info` feature. If that changes, the text is wrapped by hand [E].
- **Name clashes across projects.** A live agent with a role's name in another project blocks that role (invariant 3). Project-prefixed names avoid it; validation does not enforce them.
- **Schema drift.** The generated schema and the viewer's validator may diverge (§5.2); that is fixed when a real divergence affects the writer.
- **Restore before resume.** With `resume_agents_on_restore`, herdr relaunches a restored agent pane with `claude --resume <id>` about 0.7 s after startup begins. `pane list` already reports that pane's `agent_session` when startup runs, so adoption refuses it [verified with a fake `claude` and the session reported through `pane report-agent-session`, the call the Claude hook makes].
  - herdr also persists an agent started with `agent start` by its name (`agent_name` and `managed_agent_kind` in `session.json`) and lists it in `agent list` from the restore on, before its process runs again. So `startup` reads the role as `occupied` by its own name and says nothing, even when the resume then fails [verified in an isolated XDG: `agent list` at startup showed the restored `ex-designer` before herdr typed `claude --resume`]. The verdict lines in the log show it; a later `herdr-crew up --dry-run` gives the real state.
  - **A session without a transcript is not resumable.** Claude records a conversation only on its first request. A role started by `up` that nobody wrote to yet has a session id, reported by the hook, but no transcript, so herdr's `claude --resume <id>` after a restart fails with "No conversation found with session ID" and the tab is left as a bare shell [verified on 2026-09-26 on a real restore: a role started by `up` received no message before the server stopped]. There is nothing to resume: the name picker would only offer an **older** conversation of the same role. The remedy is to close the tab and run `herdr-crew up`, which creates it again with its prompt (and its `start_message`, which also makes this case rarer, §2.2).
  - What remains: a pane whose agent never reported a session, for example without herdr's Claude integration, carries nothing that tells it from a fresh shell. herdr does not resume such a pane either, so adopting it types into an idle shell, the same as a fresh one.
- **The role prompt after a resume.** herdr resumes with a plain `claude --resume <id>`, without `--append-system-prompt-file`. The role prompt survives because claude 2.1.x records the system prompt on the conversation's first request, appended text included, and resends that record on every later request and resume until the conversation is compacted (`--system-prompt-snapshot`, on by default) [vendor: `claude --help`, 2.1.283]. After a compaction the appended role prompt is lost. For the same reason, a prompt edited in `crew.toml` does not reach a resumed conversation until it compacts; a new conversation (`up` over a tab without an agent, or `/clear`) takes it.
- **Adopting a look-alike.** A restored session with one workspace, named like its directory, with a single tab «1» whose only pane is an idle shell in a project's root, is indistinguishable from a fresh one and is adopted. That is what the user would have got by typing `herdr-crew up` there.

**Discarded alternatives:**

- **Node**: it needs an installed runtime.
- **Keeping per-project scripts**: copies in several shells, plus viewers that diverge, pulling in tools like jq, uv and Python.
- **WASM**: herdr spawns processes and has no WASM runtime [vendor], so a host such as wasmtime would also be needed.
- **`herdr worktree create`**: it creates one workspace per worktree [verified: `herdr worktree` help], while here each worktree is a tab of the project's workspace.
- **`[[startup]]` only for plugin state, never to bring the project up**: discarded because it runs on every restore and would duplicate what herdr already resumes [vendor]. Reversed (§4.4): restore is safe because the role's tab exists with its label, so the `no agent` verdict never relaunches, and a project is only created by adopting herdr's fresh initial workspace.
- **`[[events]] on = "workspace.created"`**: it does not fire for herdr's initial workspace nor for restored ones, and it does fire for the workspaces `up` itself creates, which would run the hook again [verified]. Its event data has no `cwd`.
- **A bounded wait in a cold `up`** for startup to adopt: a headless server creates no initial workspace, so there is nothing to wait for (§4.4).
- **The board as a plugin pane**: see §3.
- **Socket instead of CLI**: it ties the binary to protocol 22; the CLI is the documented plugin API.
- **Counting only agent names to tell whether a role is alive**: sessions started by hand have no name (§4.2 step 3).
- **A fixed first message in the code** instead of `start_message`: its language and tone depend on the project (a project whose roles work in Spanish, a coordinating role with its own name), so it is configuration.
- **Prompts in separate files** (`prompt = ["file", …]`): one file per project keeps roles and prompts reviewable together. Placeholder errors point into `crew.toml` with line and column.

## 11. Construction

| Status   | What                                                                                                                                                                                                                                                                     | Trigger                                                              |
| -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------- |
| Built    | Step 1 (2026-09-26): the crate with its manifest, `up` (with `--dry-run`), `add`, `close`, `board` and `check`. The pure core (`config`, `prompt`, `plan`, `board`) is separate from the adapters (`herdr`, `git`, terminal loop). The §7.1 tests and the §7.2 real test pass. The step-1 [H] are verified except the Windows `.exe` (§3). | —                                                                    |
| Built    | The startup hook (2026-09-26): `[[startup]]` runs `herdr-crew startup`, which adopts herdr's fresh initial workspace and otherwise only repairs existing project workspaces (§4.4). Unit tests for adoption and each refusal, and the real test's startup session. | —                                                                    |
| Built    | `start_message` (2026-09-26): the first message of each new conversation, validated in §2.3 and sent only on `StartAgent` of a new tab (§2.2). | —                                                                    |
| Built    | Own repository with CI running `cargo fmt --check`, `clippy -D warnings` and `cargo test` on Linux and macOS; prompts inline in `crew.toml`                                                                                                                              | —                                                                    |
| Step 2   | Migrating the first project (§9)                                                                                                                                                                                                                                         | Step 1 green and linking approved by its user                        |
| Step 3   | A fake herdr in Rust (feature `fake-herdr`, persistent state, a call log) to test the §4.2 sequence in CI without a real herdr; Windows in CI                                                                                                                             | The first migration done                                             |
| Deferred | A picker pane for `add`/`close`                                                                                                                                                                                                                                          | A user wants to add or close instances from a shortcut               |
| Deferred | Prebuilt binaries (§8)                                                                                                                                                                                                                                                   | The first user without a Rust toolchain                              |
| Deferred | `kind` and `args` keys per role                                                                                                                                                                                                                                          | A role that is not Claude Code                                       |
| Deferred | `worktree_prompt`: text appended only to roles with `worktree = true`                                                                                                                                                                                                    | A project asks for it                                                |
| Deferred | Validating the board against the generated schema (`jsonschema` or an equivalence test)                                                                                                                                                                                 | A real divergence between schema and viewer that affects the writer  |
| Deferred | Scrolling in the board                                                                                                                                                                                                                                                   | A project's board does not fit its tab                               |
| Deferred | Configurable `owner` sections and board texts                                                                                                                                                                                                                            | A project with other sections                                        |
| Deferred | Relaunching a role automatically with `claude -r` and the session herdr knows. It would go through `herdr agent start <role> … -- --resume <id>`, never text typed into a shell, and it would change the step-3 rule that a tab without an agent only warns. The recorded id drifts after `/clear` or `/resume` inside the session, and a session without a first request has no transcript (§10). | Tabs without an agent after restarts become frequent                 |
| Deferred | `up` leaving the bring-up to startup when it has just started the server (§4.4, cold `up`)                                                                                                                                            | herdr's headless server creates an initial workspace                 |
| Deferred | A lock against concurrent `up`/`add`                                                                                                                                                                                                                                     | An orphan tab caused by concurrency in real use                      |

## 12. Open questions

None.
