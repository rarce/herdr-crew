# Software development workflows for crew setup

The `herdr-crew init` presets are starting points for this plugin, based on the primary sources below. They are not benchmarked configurations of herdr-crew. Choose the smallest crew that can produce a verifiable delivery; add roles when a task creates independent work or a distinct review question.

## What the evidence supports

Anthropic describes orchestration for dynamically decomposed coding tasks and evaluation loops when acceptance criteria are clear. Its guidance starts with simple systems and adds complexity when measured outcomes justify it. These are engineering patterns, not an experimentally established team size. [Building effective agents](https://www.anthropic.com/engineering/building-effective-agents).

Claude Code's team guide identifies independent modules, research, code review and competing debugging hypotheses as useful cases. It warns that sequential dependencies and same-file edits reduce the value of teams. It currently suggests starting with 3–5 teammates for native teams, but that advice does not establish a default for persistent crew sessions. Those sessions have a different lifecycle and communication setup. [Agent teams](https://code.claude.com/docs/en/agent-teams).

The scaling study's v3 comparison finds no general multi-agent advantage on its SWE-bench Verified subset. Its software results use only 20 issues per configuration, so they should motivate local comparisons rather than a rule against collaboration. This is the reason the wizard offers a one-session default. [Towards a Science of Scaling Agent Systems](https://arxiv.org/html/2512.08296v3).

Agyn separates management, research, engineering and review, with isolated environments and explicit acceptance or requests for changes. CodeR separates reproduction, localization, editing and verification using task graphs. Both support distinguishing responsibilities and handoffs; neither proves that every repository needs a permanent session for every stage. [Agyn](https://arxiv.org/html/2602.01465), [CodeR](https://arxiv.org/html/2406.01304v3).

Agentless uses localization, repair and patch validation, including reproduction and regression checks. It illustrates that validation needs explicit resources even in a relatively simple workflow. Its model calls and candidate selection do not correspond to one persistent crew session. [Agentless](https://arxiv.org/html/2407.01489v2).

## Presets and role responsibilities

These four mappings are our implementation choices:

| Preset | Sessions and responsibilities | Work sequence |
| --- | --- | --- |
| `solo` | `dev`: understand, implement, validate and report; also writes the board | Requirement → implementation → checks → delivery |
| `review` | `lead`: acceptance and integration; `dev`: implementation and tests; `reviewer`: findings and acceptance | Assignment → implementation → review → corrections → integration |
| `parallel` | `lead`, `dev-a`, `dev-b`, `reviewer`; developers own independent components | Common base and interfaces → components → review → ordered integration → combined checks |
| `research` | `lead`, `research-a`, `research-b`; researchers gather evidence without editing source | Common reproduction → hypotheses → evidence → one decision → chosen implementation |

Start with `solo` for a localized fix or a sequence whose next step depends on immediate test feedback. Choose `review` when a separate review can examine compatibility, edge cases or acceptance. `parallel` requires work that can be divided by files or components and always uses worktrees. `research` is useful while the cause or design is uncertain; its lead implements or assigns the selected fix after consolidation.

Architecture and product decisions initially belong to the lead. Testing belongs to developers and reviewers. Add a dedicated architect, security reviewer, designer or test specialist only when a concrete assignment requires expertise and an independent deliverable. We intentionally avoid initializing an entire organizational chart.

## Assignment, delivery and acceptance

An assignment specifies the objective, acceptance criteria, exact base commit, owned files, interfaces, dependencies and a stopping condition. Developers deliver an exact commit with reproduction and regression results, commands and remaining limitations. Reviewers inspect that commit, report actionable findings, and explicitly accept it or request changes. The lead integrates accepted commits and verifies the combined result.

For research, deliver observations, rejected hypotheses, source locations and uncertainty. Researchers need not produce patches. For parallel work, one person owns shared schemas, models and lockfiles; assign dependent tasks after their prerequisites.

## How sessions collaborate

herdr-crew sets up the team; it does not run it. It gives you persistent sessions with role prompts, optional worktrees, a shared status board and a versioned configuration. It has no message transport, task queue, automatic assignment or task graph of its own: sessions hand work to each other through the channels below, following the protocol in their prompts, and you stay in the loop.

What herdr-crew provides:

- **Roles that survive.** Each role is a named herdr agent in its own tab. The sessions keep their context across tasks, herdr restores them after a restart, and `up` repairs what is missing without starting duplicates.
- **Isolation where it matters.** Roles with `worktree = true` edit their own checkout, so a developer and a reviewer never step on each other's files.
- **One shared picture.** The coordinator writes `.herdr/status.json`; the board tab shows every session's state, what each waits on, and what is waiting on you.
- **A team definition you can review.** `crew.toml` is versioned with the code, so the whole team, prompts included, changes through normal review.

### Channels between sessions

| Channel | How | Notes |
| --- | --- | --- |
| **You relay** | Copy the handoff from one tab and paste it in another. | Always works, for any agent. Slowest, but you see and approve every handoff. |
| **Claude Code messaging** | In a Claude role, ask the session to message another by role name, for example "send the delivery to @tl-reviewer". | Claude Code 2.1.224+ messages your other local sessions directly ([cross-session messaging](https://code.claude.com/docs/en/cross-session-messaging)). herdr-crew starts each Claude role with `-n <role>`, so the role name is the session name. Only between Claude sessions; each session's own permissions and prompts still apply. |
| **herdr agent commands** | The coordinator runs `herdr agent prompt <role> "<text>" --wait --timeout 600000`, and `herdr agent read <role> --source recent-unwrapped --lines 120` to read a reply. | Works for Claude and Codex roles, because every role is a herdr agent named after it. The command needs shell access to the herdr socket, which a sandboxed Codex session may not have and which your agent may ask you to approve. herdr rejects a prompt to an agent waiting at an approval dialog. Not exercised by herdr-crew's tests. |
| **Git** | Developers commit on a task branch; reviewers and the coordinator read that exact commit. | Worktrees share the repository's refs, so a commit is visible to every session at once, without pushing. The commit, not a description of it, is what gets reviewed and integrated. |
| **The board** | The coordinator records state, queues and decisions for you. | Status, not messages: other roles report to the coordinator instead of editing it. |

Whatever the channel, a session never answers another session's approval prompt or grants it permissions: approvals stay with you in the tab that asks.

### A handoff, step by step

This is the `review` preset's protocol; the others follow the same shape.

1. **You give the coordinator a task.** Talk to the lead tab as you would to any agent, including what "done" means for you.
2. **The coordinator assigns it** to a developer with the objective, acceptance criteria, the exact base commit, the files it owns and a stopping condition, and updates the board.
3. **The developer delivers a commit**, with the commands it ran, their results and remaining limitations, to the coordinator.
4. **The coordinator asks the reviewer** to review that exact commit against the acceptance criteria.
5. **The reviewer accepts or requests changes** with findings that name files, consequences and reproduction steps. Corrections go back to the developer, then to review again.
6. **The coordinator integrates** accepted commits in dependency order, validates the combined result, updates the board and reports to you.

Example handoffs, short enough to relay by hand:

```text
Assignment for tl-dev: #12 expiring links. Base: origin/main 3f2a1c9.
Accept when: links accept an optional expiry; expired links return 410; tests cover both.
You own: src/links.rs, tests/links.rs. Stop and ask if the storage format must change.
```

```text
Delivery from tl-dev: #12 on feat/12-expiry, commit 8d41e07 (base 3f2a1c9).
cargo test: 48 passed. New tests: expired_link_returns_410, link_without_expiry_never_expires.
Limitation: the expiry is checked at read time; no cleanup job.
```

```text
Review of 8d41e07 by tl-reviewer: changes requested.
src/links.rs:88 compares the expiry with the local clock instead of UTC; a link created
in UTC-3 expires three hours late. Reproduce: TZ=America/Santiago cargo test expired_link.
```

See [the end-to-end demo](demo.md) for a full round, the board at each step, and recovering a stopped session.

### Compared with other ways to run several agents

| | Several manual tabs | herdr-crew | Claude Code agent teams | One agent session |
| --- | --- | --- | --- | --- |
| Setup | By hand, every time | `crew.toml` in the repository; `up` or herdr's startup | A request to the lead session; experimental flag | None |
| Agents | Any | Claude Code and Codex, mixed per role | Claude Code | Any |
| Sessions after a restart | Start them again | herdr restores the tabs and resumes the agents; `up` repairs what is missing | In-process teammates are not restored on resume | Resume the one session |
| Isolation | Up to you | Optional worktree per role | One checkout; split files by owner | One checkout |
| Coordination | You | Role prompts, the board, and the channels above | Shared task list and direct messages | Not needed |
| Cost | One session per tab | One session per role, all running | One session per teammate | Lowest |

herdr-crew fits when the same roles come back task after task on a project, when you want them visible as tabs that survive restarts, or when you mix Claude Code and Codex. For one-off parallel exploration within a single Claude Code session, [agent teams](https://code.claude.com/docs/en/agent-teams) or subagents are lighter. For a localized fix, one session is usually best.

### Worktrees and the base branch

Reviewed delivery uses developer and reviewer worktrees by default. Shared-checkout review is available with `--shared-checkout`: only the developer edits, and the reviewer does not switch or reset the shared checkout. New worktrees start detached from `worktrees.base`, not from local lead changes. Check the assigned base, create a task branch before committing and validate the delivered commit in the reviewer's worktree. Existing worktrees are reused.

The wizard discovers remote references locally; it does not fetch or verify a branch on the remote server. Before `up`, ensure the chosen base is available and dependencies are committed. Review prompts in the generated `crew.toml`; compare success, elapsed time, total agent cost and integration effort on your own tasks before expanding the crew.
