# Native Codex crew tree (experimental)

Date: 2026-10-19 (America/Santiago). Prototype only; no production startup changes.

This mode makes the coordinator responsible for creating and directing Codex
worker roles inside one native collaboration tree. It is available through the
experiment scripts. `herdr-crew up --agent codex` by itself still starts independent
sessions; it does not enable this mode or convert existing conversations.

The experiment converts a source crew into a **separate checkout's single-coordinator
crew**. Herdr starts and restores that coordinator through the existing trusted crew
hook and immutable snapshot. The coordinator creates the other roles with native
`collaboration.spawn_agent`, using `fork_turns = "none"` and each role's full prompt
and shared project instructions. It reuses them through `followup_task`.

```text
Herdr
├── tree-lead tab → Codex /root
│                  ├── /root/tree_reviewer
│                  └── /root/tree_coder
└── status tab
```

Workers have separate native thread UUIDs but share their parent's `session_id`.
They are not separate Herdr panes. Inspect their threads with Codex `/subagents`.
Giving each child a separate terminal attached to its existing thread has **not**
been validated. Starting another independent Codex process per role would undo the
tree architecture.

## Working with the coordinator

Talk to the coordinator's Herdr tab. It represents `board.writer` from the source
crew, such as `erp-main`, and has the native identity `/root`. Worker role names
are mapped to native task names: for example, `erp-reviewer` becomes
`/root/erp_reviewer` after the coordinator creates it. The coordinator retains the
returned identities and reuses the same workers for subsequent assignments.

| Direction | Behavior in the tested mode |
| --- | --- |
| Coordinator to a new worker | `spawn_agent` creates the role and supplies its full rendered instructions |
| Coordinator to an idle worker | `followup_task` starts another turn on that existing worker |
| Coordinator to a worker, information only | `send_message` queues information without starting an idle worker |
| Worker to coordinator | The worker's final response is delivered automatically as a native message |
| Worker to another worker | The coordinator receives the result and passes it into the other worker's task |

**Workers can communicate with main.** Their final responses can contain results,
questions or blockers. The coordinator receives them without the user copying text
between sessions. In Codex 0.163.0 the tested workers did not expose `send_message`,
so an intermediate message while continuing the same turn is not a supported
worker channel. A worker that needs input finishes its turn with the question;
the coordinator answers in another task or presents it to the user when needed.
Peer messages never grant approvals or expand permissions.

For example, ask the coordinator:

> Ask the existing reviewer to inspect this change and report findings without
> editing. Pass those findings to the existing coder, wait for its result, then
> run the tests and report back.

For subsequent work, ask it to reuse the existing role. The coordinator can wait
for native updates with `wait_agent` and inspect loaded agents with `list_agents`.
Communication through these native tools keeps the parent sandbox and approval
policy in effect; no `danger-full-access` setting is needed for the tested flow.

After a restart, keep the original checkout, crew snapshots, `CODEX_HOME`, profile
and session store (`sqlite_home`, when configured) available. The coordinator
resumes first, then `followup_task` can restore the saved workers lazily by their
known canonical paths. `list_agents` showing only `/root` immediately after a cold
restart is not a reason to create replacements. A failed lookup must be reported,
and the store and saved identities checked before attempting new role creation.

## Implementation and reproduction

`experiments/codex_tree.py` reads a source `crew.toml` and prepares the separate
destination. It keeps project prompts and ownership rules, replaces only obsolete
messaging guidance, maps hyphenated role names to native task names, and refuses
collisions, existing destination crews and symlinked `.herdr` directories. The
board writer becomes the coordinator. It does not edit the source project, install
global hooks, change user configuration, start sessions or migrate conversations.

Preparation requires Python 3.11+. For example, after creating an empty disposable
Git checkout:

```sh
python3 experiments/codex_tree.py \
  --source tests/fixtures/crew-codex-tree.toml \
  --root /absolute/path/to/disposable-checkout
```

Preparation writes the experiment's coordinator configuration; it does not start
the crew. All source roles must select Codex and share the checkout, and
`board.writer` must identify the coordinator. Worktrees and per-role Codex overrides
are rejected. Start with the fixture and provided test runners below to reproduce
the validated environment before adapting another crew.

The generated crew requires `features.multi_agent_v2 = true` in its Codex test
configuration. Codex 0.163.0 otherwise exposes `multi_agent_v1`, whose tools and
identities differ. The automated experiment configures this flag in a temporary
`CODEX_HOME`, installs and trusts only that home's exact test crew hook, and calls
ordinary `herdr-crew up --no-attach` there. Nothing is enabled globally.

Run the complete experiment from this repository:

```sh
python3 tests/support/test_codex_tree.py
CREW_REAL_HERDR=1 CREW_REAL_CODEX=1 \
  cargo test --locked --test codex_herdr native_codex_tree -- --ignored --nocapture
```

It needs real Herdr and Codex executables, Python 3.11+, and permission to bind
localhost HTTP and Unix sockets. It uses a unique named Herdr server, temporary
HOME/XDG/CODEX_HOME directories, a temporary Git repository, and a local scripted
Responses provider. It does not read credentials or call an external model. The
provider chooses tool calls; actual Codex executes them and persists their results.
This verifies runtime behavior, **not** a model's ability to follow the bootstrap
instructions autonomously.

`CREW_TREE_REPORT=/absolute/path/report.json` writes a compact evidence report.
`CREW_TREE_KEEP=1` retains successful run artifacts; failed runs retain them by
default. Herdr is stopped and the named test session is deleted in either case.

## Live-model acceptance

`experiments/codex_tree_live.py` exercises the same generated crew with the user's
existing native Codex login and configured model. There is no mock provider. It is
an explicit command, outside the normal test suite, and consumes real account usage:

```sh
cargo build --locked
python3 experiments/codex_tree_live.py --crew-bin target/debug/herdr-crew
```

The test creates a disposable checkout and named Herdr server. It uses the existing
trusted crew hook without installing or modifying hooks, copies no credentials,
and preserves user-wide model and permission settings. Codex's normal folder-trust
dialog is confirmed only for the files generated by this test; Codex saves that
per-folder trust decision. Native session records remain in the usual Codex home;
test database/logs and a compact report stay under the printed temporary directory.

The real coordinator must create both workers and receive their role confirmations,
ask the reviewer to find an off-by-one bug, pass the finding to the coder, and
verify the coder's fix with three real Python unit tests. After a cold Herdr restart,
it must reactivate the original workers and receive their recalled initial tokens
and work summaries, without spawning replacements. The harness checks actual
worker final messages delivered to root, filesystem results and unchanged UUIDs.
Real tool-message arguments are encrypted in native rollouts; a plaintext search
of those arguments cannot establish whether the coordinator gave a recall hint.

The invocation wrapper must pass temporary-store overrides **after** `resume`:
resume's own `-c` options can otherwise replace top-level overrides and send the
restored process to a different store. Hook inspection uses the usual configuration
without creating a thread or rebuilding the temporary session store.

**Live result:** the 2026-10-19 local acceptance run passed with `gpt-6.1-sol`
for all three threads, Codex 0.163.0 and Herdr 0.9.5. Root received three native
replies from each worker: initialization, review/fix and post-restart recall. The
reviewer found the inclusive-upper-bound bug, the coder applied the recorded patch,
and root only executed the independent unittest verification. All three tests
passed. After the cold restart the same three UUIDs continued, and both workers
returned their original tokens and previous work summaries. The evidence report
and generated checkout were retained under `/private/tmp/clu_2giu6m/`.

An earlier live attempt verified messaging and code work but failed recovery
because the test wrapper put `sqlite_home` before `resume`, selecting another
store after restart. The passing run uses the corrected wrapper. This was a test
environment bug; the production launcher was not changed for it.

## Observed behavior

The local run used **Codex 0.163.0 and Herdr 0.9.5 on macOS**, with workspace-write
and on-request approvals. The checked scenario covers:

| Check | Evidence |
| --- | --- |
| Role creation | Exactly one root and two children; distinct native UUIDs, one session tree, correct parent IDs |
| Instructions | Each worker receives its full rendered role prompt as its native initial task |
| Replies | Both workers' final responses arrive at the root as native `FINAL_ANSWER` messages |
| Information only | Root `send_message` queues information without activating an idle worker; it appears when the worker is next activated |
| New work | `followup_task` starts new turns on the same two workers without creating duplicate sessions |
| Role handoff | Root passes the reviewer's result into the coder's next native task; the coder's final response returns to root |
| Sandbox | Each child writes an allowed checkout file; writing to the protected `.codex` directory is denied |
| Herdr restart | The real server stops and restarts; the trusted root hook runs again; both original children accept followups and return results with the same three UUIDs |
| Hook ownership | Only the coordinator owns a crew snapshot/binding; children never overwrite its Herdr identity |

Every requested tool call must have a matching native output. Unsupported calls,
missing outputs and failed collaboration operations fail the test; simulated final
text alone does not establish success.

Two important findings shape the design:

1. **Workers do not expose collaboration tools in the tested CLI.** Direct child
   `send_message` calls were rejected as unsupported. Increasing `agents.max_depth`
   and enabling `multi_agent_v2_dynamic_tools` did not expose them in separate
   probes. The working topology has the coordinator dispatch and forward work;
   workers return final results automatically. They need no Herdr socket access.
2. **Cold restoration is lazy.** Immediately after restart, root `list_agents`
   can show only `/root`. `followup_task` to a previously created canonical child
   path restores that child and its history. An absent entry in `list_agents`
   therefore must not cause the coordinator to spawn a replacement.

These are installed-version observations, not guarantees for every Codex release.
The official [subagent documentation](https://learn.chatgpt.com/docs/agent-configuration/subagents)
describes parent orchestration, child threads and inherited permissions. The
experiment supplies the additional local evidence about tools and cold recovery.

## Boundaries before production

The prototype supports a Codex-only crew in one shared checkout. Worktrees,
per-role Codex overrides, independently sandboxed workers, dynamic extra instances,
worker tabs, mixed-agent crews and a worker-aware board are outside this experiment.
Unsupported per-role settings are rejected rather than silently dropped. The
existing production commands and sessions remain unchanged.

The next production step is an explicit tree mode with persisted role-to-thread
ownership and board integration, extending the passing live-model acceptance case.
The experiment does not claim migration of existing independent crew
conversations, robust duplicate prevention by a host controller, recovery during
an unfinished child tool call, or correctness after clear/compaction. Those need
their own acceptance cases before making this the default startup architecture.
