# Codex support implementation design

**Date:** 2026-10-06. **Status:** implemented; local offline tests, real Herdr restoration and real Codex request inspection pass. Remote CI/release validation remains pending.

Full Codex support means that a crew may use Claude Code, Codex, or both; every session receives its role instructions; extra instances inherit their role's settings; and restoring a conversation preserves those instructions and the crew's explicit launch settings. Existing Claude configurations keep their behavior.

Herdr already starts Codex. The substantial work is instruction delivery, session binding, and restoration. The implementation uses an additive Codex hook for role context and a small resume helper for saved options. It keeps large instructions out of terminal commands and leaves Herdr responsible for restoring agents. The lifecycle and request-inspection tests described below verify context delivery and configured restoration without external model calls.

## Verified baseline

The reviewed environment has Herdr 0.9.3 and Codex CLI 0.160.1, with the native Herdr Codex integration at version 8. The pre-change baseline passed 135 ordinary tests. New tests cover both agents, hooks and recovery; optional integration evidence appears below.

Before this change, the code fixed the launch kind to Claude in `src/herdr.rs::start_agent`, required Claude in `src/main.rs::check`, and recommended Claude resume commands in `src/plan.rs`. `Config` rejected `kind` and `args`, including a test specifically asserting those rejections. `Pane` retained only `agent_session.value`, discarding the agent and reference kind. `Agent` also discarded the reported agent kind.

The board, role ordering, extra-instance naming, root discovery, and Git worktree creation are substantially independent of the agent. Preserve their existing rules rather than introducing parallel implementations.

Herdr's exact [0.9.3 resume code](https://github.com/herdrdev/herdr/blob/v0.9.3/src/agent_resume.rs) builds `codex resume <id>` for native session references. Its [restore code](https://github.com/herdrdev/herdr/blob/v0.9.3/src/persist/restore.rs) prefers a valid agent-reported resume command when one exists. These are distinct persisted fields; an identity alone does not restore arbitrary crew CLI options.

## Instruction delivery

Codex `developer_instructions` adds session guidance. `model_instructions_file` replaces the built-in instructions, so using the rendered crew prompt as that file would change more than the role. Profiles live in the user's Codex configuration directory and are unsuitable as automatically generated project runtime files. These behaviors are documented in the [OpenAI configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference) and [advanced configuration](https://learn.chatgpt.com/docs/config-file/config-advanced).

A local transport probe used an isolated named Herdr server, temporary XDG directories, and a simulated `codex` executable. It verified the executable in the pane before launch, compared the entire received argument array, and included escaped newlines, Unicode, quotes, dollar signs, and backticks. No real Codex conversation or model request was started.

| Pane shell | Encoded instruction argument | Result |
| --- | --- | --- |
| `/bin/sh` | 268, 1,768, 4,068, 8,068, 16,068 bytes | Exact argument round trip |
| `/bin/sh` | 64,068 bytes | Executable did not receive the command |
| `/bin/zsh` with isolated startup configuration | 268, 1,768, 8,068, 16,068 bytes | Exact argument round trip |

The fake agent intentionally lacks a complete Codex UI, so `agent start` timing out is not a readiness result. The table establishes argument delivery on this macOS installation, not a portable size limit. Linux, other shells, and terminal readiness still require integration coverage. It updates the older 1.7 KB truncation observation in `docs/design.md` without making inline prompts an unrestricted transport.

| Approach | Decision |
| --- | --- |
| Entire prompt in `-c developer_instructions=...` | Useful probe; unsuitable as the only transport for unrestricted prompts |
| First message asks Codex to read a file | Possible limited implementation; does not establish the full restore contract |
| `model_instructions_file` points to the role prompt | Reject: replaces Codex's built-in guidance |
| Per-role edits to repository `AGENTS.md` or shared `.codex/config.toml` | Reject: several roles can share the main checkout and would overwrite one another |
| Separate per-role `CODEX_HOME` | Reject: fragments user configuration, authentication, hooks, and session discovery |
| Additive role hook and snapshot-based resume helper | Implemented |

## Configuration contract

Keep `version = 1` and introduce optional keys. Old files resolve to Claude; older binaries will reject the new keys rather than silently select the wrong agent. Document the minimum herdr-crew release supporting them.

```toml
version = 1
kind = "codex"                     # optional default; absent means claude

[codex]                           # optional defaults for Codex roles
sandbox = "workspace-write"
approval_policy = "on-request"

[workspace]
label = "payments"

[board]
tab = "payments-status"
writer = "payments-lead"

[worktrees]
dir = ".worktrees"
base = "origin/main"

[[roles]]
name = "payments-lead"
kind = "claude"                    # overrides the project default
prompt = '''Coordinate the delivery and maintain the board.'''

[[roles]]
name = "payments-dev"
prompt = '''Implement your assigned component in your worktree.'''
worktree = true
extra = true

[roles.codex]
profile = "development"            # existing user profile, never generated
```

Resolve each role's kind as role override, project default, then Claude. Resolve each Codex option independently as role override, project default, then the user's Codex configuration. Missing and empty values must not be conflated. An explicit empty additional-directory list clears the project default; extra instances use their base role's effective settings.

Support structured Codex options for model, profile, reasoning effort, sandbox, approval policy, search, and additional writable directories. Model IDs must remain user-selected strings; do not hard-code a model or silently migrate it. Validate Codex-only role options against the effective kind, use strict unknown-key handling, and preserve source positions in errors.

Do not introduce unrestricted `args` in this implementation. Subcommands, `--cd`, `--worktree`, prompt overrides, and bypass flags can conflict with the role and isolation contract. The adapter owns the working directory, initial message, instruction delivery, and resume selection. Future additional options can be added with explicit semantics.

Retain `start_message` inheritance, including an empty role override disabling it. Keep its existing one-line, 200-byte constraint for the terminal launch path. Remove Claude-specific wording from shared validation errors. It is sent only for a fresh role or extra instance, never during restore, board repair, or a repeated `up`.

## Codex integration and startup

The `codex-install` operation registers one stable, synchronous `SessionStart` hook for crew context. Installation may update user-level Codex hook configuration only when the user invokes that installation operation. Ordinary `init`, `up`, `add`, and `startup` must not install hooks or change global configuration.

Merge the crew entry without replacing the native Herdr integration or other hooks. Preserve unknown JSON fields and unrelated configuration. Uninstall removes only the crew-owned entry and artifacts; repeated install and uninstall are idempotent. Honor the user's configured Codex home without copying credentials or editing the native Herdr script.

Codex requires trust for non-managed hooks. The setup instructions must include reviewing the crew hook through `/hooks`. Hook sources are additive, and `SessionStart` can return `additionalContext` as developer context. Configure `additionalContextLimit = 0` only together with an enforced output bound, set to 64 KiB of rendered role context, so a large prompt cannot silently spill to a partial preview. [OpenAI hook documentation](https://learn.chatgpt.com/docs/hooks).

The installed Codex 0.160.1 JSON schema exposes `hooks/list`, including `enabled`, `trustStatus`, `currentHash`, source, and per-directory errors. Use a short-lived `codex app-server --stdio` connection for this read-only preflight, without starting a thread or a turn. Match the exact installed crew hook and relevant directory. Unknown, modified, untrusted, disabled, or policy-blocked hooks stop a planned Codex launch before workspace or Git mutations. Do not automatically bypass hook trust. The adapter must bound its I/O and reap the inspection process.

The real CLI profile probe found that `app-server` rejects both `--profile` and the retired `-c profile=...` selector. Crew validates the existing profile TOML separately and passes `--profile` only to interactive launch/resume. Profiles containing `[hooks]` are rejected with a migration message: hook configuration and trust must stay in the inspectable base layer. Other profile keys remain native Codex settings. This explicit compatibility bound prevents a trusted base hook from concealing a disabled or differently trusted profile hook.

Codex roles require Herdr 0.9.3 and Codex 0.160.1 or newer. Claude retains the plugin's Herdr 0.9.1 minimum. The latter is a tested compatibility target, not a claim that all older versions lack the necessary features. A Codex-specific server-version and CLI/protocol gate enforces this requirement. Include version and protocol checks in preflight rather than allowing partially configured sessions.

Fresh launch sequence:

1. Validate the configuration, project-owned paths, executable availability, required integration, and hook trust for the roles that will actually start. A board-only repair must still work when Codex is unavailable.
2. Render the existing role and common prompts. Write an immutable launch snapshot and prepare a binding for the returned pane and terminal IDs.
3. Start through native `herdr agent start --kind codex`, passing only bounded structured options and the optional initial message.
4. The hook validates the input and binding, supplies the exact role context, associates the native session ID with that snapshot, and reports the custom resume command to Herdr.

Use `--no-daemon` for crew-managed Codex launches in the initial implementation. It avoids assuming that a shared backend forwards each pane's Herdr context correctly. Reapply it on resume. A later change may enable daemon mode after a specific test demonstrates correct hook context in two concurrent crew panes; process-detection behavior must also be verified with the real Codex TUI.

Keep role instructions separate from authentication and approvals. The native Codex login remains the user's login. Preflight and setup must not request an API key or initiate login flows.

## Session binding and restoration

Runtime artifacts belong in the main checkout under a new ignored directory such as `.herdr/codex/`. Validate every generated path through `files.rs`, including artifacts for extra instances. Preserve its protections against symlinks, hardlinks, path traversal, and escaping into Git metadata.

A launch snapshot records the effective crew settings, rendered instructions, role and instance names, main checkout, intended working checkout, and a runtime format version. It contains no authentication data. Prepare it before launch and do not mutate it when `crew.toml` changes while the conversation remains active.

Bind a session using both Herdr context and verified Git identity. A directory alone is insufficient: several roles may share the main checkout. Pane IDs alone are also insufficient across different servers. Validate the workspace, tab label, pane, terminal, server context, main checkout, and registered worktree. Unrelated Codex sessions receive no crew context. Refuse to associate the same native session with a second role or project.

Handle `startup`, `resume`, `clear`, and `compact` explicitly. Startup associates the prepared snapshot; resume and compact keep its role instructions; clear binds the new session to the existing role activation. Do not treat a compaction event as a new crew role. When executing a resume helper, pass a private binding reference to the child process and use it to validate the new pane context.

Report the short resume command `herdr-crew codex-resume <binding>`, using Herdr's agent-reported resume facility. The helper reads and validates the immutable snapshot and constructs the Codex process arguments directly, so neither the prompt nor arbitrary filesystem paths need to be encoded into the terminal command. Require the public launcher for this recovery contract; verify that it resolves to the registered plugin and document the development-checkout setup.

Herdr 0.9.3 restricts reported resume commands to a plain executable name, at most 64 arguments and 8,192 argument bytes, with no control characters or apostrophes. Use an opaque, validated binding token rather than a root path in that command. Keep token resolution local to the verified checkout; the token must not authorize arbitrary command execution. [Herdr resume validation](https://github.com/herdrdev/herdr/blob/v0.9.3/src/agent_resume.rs).

Agent-reported commands have different deduplication keys from native IDs. The implementation must explicitly reject duplicate session ownership and test duplicate persisted snapshots. Confirm with an isolated restart that the native Herdr hook cannot erase or race the crew's reported command, and that the helper runs once for the saved session.

There is a concrete registration race in Herdr 0.9.3: without an existing matching hook authority, `can_record_reported_resume` requires an already detected matching agent process. A `SessionStart` report can therefore arrive too early. Another hook authority can also invalidate the stored command. The implementation reports/reads back identity using `herdr:codex`, then reports the recovery argv separately through `herdr-crew:codex`, without acquiring lifecycle authority. It supplies fresh sequences and retries `resume_not_accepted` for up to five seconds. The isolated Herdr test confirms that the command is persisted and restored even after a subsequent native report; no upstream change was needed. [Herdr terminal state](https://github.com/herdrdev/herdr/blob/v0.9.3/src/terminal/state.rs).

The helper reapplies the snapshot's explicit options, working directory, daemon choice, and integration. It never sends `start_message`. A missing or incompatible snapshot, mismatched checkout, or unavailable launcher leaves a recoverable error; it must not start a fresh conversation. `startup` continues to repair only the board on restore.

Extend the status model to retain the session's agent, reference kind, source, and value, plus the live agent kind. Recovery messages use the recorded session kind even if `crew.toml` now selects another agent. Changing configuration never terminates or replaces an occupied role.

## Permissions and environment

Inherit user permissions unless the crew explicitly configures them. Do not translate a Claude permission flag into a Codex bypass. Under Codex's workspace-write policy, `.git`, resolved Git pointer directories, `.codex`, and `.agents` remain protected. Git mutations can need approval even when source files are writable. [OpenAI sandbox documentation](https://learn.chatgpt.com/docs/agent-approvals-security#protected-paths-in-writable-roots).

Keep the existing worktree manager and never pass Codex `--worktree`. If the board writer works in a worktree, its runtime context must provide the absolute board and schema locations. Granting write access to the board's parent directory should be an explicit validated setting; do not automatically grant write access to the entire main checkout. Preserve the documented values of existing placeholders and add absolute runtime guidance separately.

Test socket access to Herdr, launching the plugin helper, Git operations, and network-requiring validation under the chosen Codex permissions. Hook execution and model-generated sandboxed commands are separate paths; one working does not prove the other is allowed. Respect managed policies and expose a concrete error when they prevent the configured workflow.

Audit inherited session markers when creating a cold Herdr server and child sessions. In particular, the native Codex integration ignores a reported session when inherited `CODEX_THREAD_ID` identifies a different session. Clear inappropriate inherited markers without dropping the user's Codex home, profile, authentication, or unrelated preferences.

## Implementation changes

| Component | Responsibility |
| --- | --- |
| `src/config.rs` | Kind enum, defaults, typed Codex options, inheritance, strict validation |
| New `src/agent.rs` | Effective launch settings, agent-specific arguments, recovery descriptions |
| New Codex integration module | Hook install/remove, bounded trust inspection, context protocol, snapshot binding, resume helper |
| `src/main.rs` | Setup options, integration and internal command dispatch, dependency and compatibility checks |
| `src/init.rs` | Agent selection, `--agent`, setup guidance, runtime ignore rule |
| `src/plan.rs` | Agent settings in launch steps, runtime artifacts, recorded session kinds, informative dry runs |
| `src/herdr.rs` | Execute the extended plan, prepare bindings with real IDs, retain retry and blocked-start handling |
| `src/files.rs` | Preflight and atomic writes for all new runtime artifacts |
| Tests and CI | Both agents, mixed crews, hook protocol, custom resume, native terminal integration |
| README, design, metadata | New configuration and setup contract, supported versions, removal of Claude-only scope statements |

Model options belong in the pure adapter rather than in board rendering. No new agent identity field is required in the board JSON schema to deliver this feature. A dependency may be justified for preserving TOML during explicit integration setup; do not add an SDK or an OpenAI API client for launching the installed CLI.

## Implementation order and acceptance

The instruction and recovery prototypes were completed first, then incorporated into the implementation. The following remain the acceptance requirements for releases.

| Stage | Required evidence |
| --- | --- |
| Hook prototype | Trusted hook contributes exact developer context; untrusted/disabled hooks are detected before launch; large output is not silently spilled |
| Recovery prototype | Native session hook plus crew hook preserve identity and a persisted custom resume despite process-detection timing; isolated restart reapplies settings and context exactly once |
| Configuration and adapter | Old fixtures remain Claude; project/role overrides and extra inheritance are correct; incompatible options fail before mutations |
| Production integration | Installer preserves unrelated hooks/configuration; public launcher relocation works; bounded errors and cleanup are verified |
| CLI and planning | `init`, `check`, `up`, `add`, adoption, dry run, occupied roles, and board-only restore obey the new contract |
| Release validation | Linux/macOS checks pass, isolated terminal tests pass, and an opt-in real Codex conversation verifies the complete lifecycle |

Focused tests must cover:

- Claude-only, Codex-only without Claude installed, and mixed crews; arbitrary live agent kinds continue to block occupied panes.
- Project and role inheritance, per-role model/profile/permissions, empty overrides, additional directories, and extras.
- Prompts with Unicode, multiline content, quotes, backticks, long paths, and at least 64 KiB; oversize input fails explicitly at the documented context bound.
- Shared checkout roles receive different instructions; worktree roles use the correct checkout; an unrelated or renamed pane cannot acquire another role's binding.
- Trust inspection is bounded, local, and performs no model request; modified hooks and managed-policy denial prevent launches without preventing board repair.
- Start blocked by project trust or login, busy-shell retries, transport failures after possible launch, and missing session metadata. Retrying must not duplicate a started agent.
- Resume, clear, compact, plugin update/relocation, deleted snapshots, duplicate native IDs, and configuration changes during active conversations.
- `start_message` reaches fresh Codex intact and is absent on every recovery path.
- Installation and removal preserve other hook entries, native integrations, authentication files, and user configuration.
- Marker isolation, daemon choice, socket access, Git approvals, and board writing from a configured worktree writer.

`tests/support/herdr.sh` now carries agent kinds and complete session references instead of assuming `--append-system-prompt-file`. Ordinary tests stay offline. The scheduled Linux/macOS integration pins Herdr 0.9.3 and Codex 0.160.1, runs guarded fake-agent restoration, and inspects real Codex requests through a local mock provider. External model conversations remain outside these tests.

Run `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked`, and the locked release build for implementation changes. Review any changed board snapshots; no board layout change is planned. Instruction delivery, configured startup and restored startup pass locally. Linux/macOS CI and release checks must pass on the final revision.

## Local implementation evidence

- The final macOS suite passes 153 ordinary tests, formatting, Clippy for all targets and the locked release build. The opt-in Claude/launcher Herdr tests pass as well. Linux/macOS CI is configured and awaits execution on the published revision.
- `tests/execution.rs` exercises native argument selection for mixed crews and extras; Codex-only dependency checks; hook trust/configuration errors before project changes; context-size and symlink bounds; installer preservation; distinct shared-checkout roles; clear/compact; duplicate ownership; renamed tabs; deleted snapshots; and saved options after configuration changes.
- `tests/codex_herdr.rs` runs Herdr 0.9.3 with guarded fake agents and temporary XDG directories. It verifies exact context, native-hook coexistence, a persisted custom resume command, and configured recovery after an actual server restart. The test session is stopped and deleted on exit.
- The opt-in `real_codex_sends_exact_crew_hook_context_as_additive_developer_guidance` test uses Codex 0.160.1 with its own temporary configuration and a local mock Responses provider. It reviews/trusts only its own installed hook and inspects the outgoing request, checking complete large-context delivery as developer guidance and separate built-in instructions. No real credentials or external model are used.
- `codex-list` exposes role, binding and native session ID for manual recovery. `codex-resume` operates on the saved activation rather than requiring the current `crew.toml`.

The CLI inspector caps stdout at 2 MiB, retains at most 8 KiB of error output and bounds each inspection to ten seconds. Runtime context is capped at 64 KiB and persisted artifacts/profile input at 512 KiB; shell-encoded launch options are capped at 8 KiB after resolving additional directories. Hook failures return `continue: false` as successful JSON output, because a nonzero command-hook exit would otherwise be advisory.
