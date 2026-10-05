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

## How this maps to herdr-crew

`up` starts every configured role, including reviewers who are initially waiting. Only the first role writes the board. Prompts define the work protocol; init configures no automatic routing, task graph or communication transport. Report to the coordinator through an available session channel or ask the user to relay the handoff.

Reviewed delivery uses developer and reviewer worktrees by default. Shared-checkout review is available with `--shared-checkout`: only the developer edits, and the reviewer does not switch or reset the shared checkout. New worktrees start detached from `worktrees.base`, not from local lead changes. Check the assigned base, create a task branch before committing and validate the delivered commit in the reviewer's worktree. Existing worktrees are reused.

The wizard discovers remote references locally; it does not fetch or verify a branch on the remote server. Before `up`, ensure the chosen base is available and dependencies are committed. Review prompts in the generated `crew.toml`; compare success, elapsed time, total agent cost and integration effort on your own tasks before expanding the crew.
