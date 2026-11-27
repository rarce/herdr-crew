# Models and effort per role

Investigation: 2026-11-20 (updated 2026-11-23 for `agy`). Sources: Anthropic's current model
and effort guidance (Claude Fable 5.2, Opus 5.5, Sonnet 5.5, Haiku 5.5), `claude --help`
2.1.329, `codex debug models` from Codex CLI 0.170.0, and `agy models` / `agy --help` from
Antigravity CLI 1.5.15. These are starting points for herdr-crew, not benchmarks of it;
measure on your own tasks before raising a default.

## Why roles need different models

A crew's sessions do different work. A lead plans, assigns and integrates; a reviewer judges a
delivery; a developer runs a long edit-and-test tool loop; a researcher mostly reads. Giving
every session the same model either overspends on reading and routine edits or underspends on
the decisions that are expensive to get wrong. Two findings from the vendor guidance shape the
profiles:

- **Judgment roles repay capability.** Use at least `high` effort for intelligence-sensitive
  work. A missed defect in review or a wrong decomposition by the lead costs more turns than
  the review or the plan itself, so these roles get the strongest tier first.
- **Cost is per completed task, not per request.** For agentic coding, the standard tier
  (Sonnet 5.5, GPT-6.2-Sol) at `high` effort is the usual best cost per finished task. Lower
  effort means fewer, more consolidated tool calls; useful for reading and simple tasks, not
  for a developer that must get tests passing.
- **Frontier models start at `high`.** Claude Fable 5.2's gains above `high` are uneven; use
  `xhigh` only where measured. For Opus-class models, `xhigh` is the usual best setting for
  coding and agentic work, and Opus 5.5 defaults to `medium` unless effort is set, so crew
  sets it explicitly.
- **Reading-heavy work fits a cheaper model.** Research sessions gather evidence and report;
  the standard or light tier keeps parallel hypotheses affordable.
- **Parallel developers multiply cost.** In `parallel`, two developers (and any `add`ed
  instances) inherit the developer setting, so it stays on the standard tier except in
  `quality`.

## Profiles

`herdr-crew init --models PROFILE` (or the wizard's *Models* step, including first-run `herdr-crew up`) writes each role's model and
effort into `[roles.<agent>]`. `balanced` is the default. After picking a profile in the setup
wizard, press `e` or `Tab` to customize any role's model (from the agent's catalog, `custom…`,
or `inherit`) and effort before saving; in line-by-line mode (`--no-tui`), answer `y` when asked
whether to customize roles.

| Duty | `economy` | `balanced` | `quality` |
| --- | --- | --- | --- |
| solo | standard · high | strong · high | frontier · high |
| lead | standard · high | strong · high | frontier · high |
| developer | standard · medium | standard · high | strong · xhigh |
| reviewer | strong · medium | strong · high | frontier · high |
| researcher | light · medium | standard · medium | strong · high |

`inherit` writes no model settings: every role uses your agent's own configuration, as before.

Tiers map to each agent as follows:

| Tier | Claude Code (`--model`) | Codex (`model`) | pi | agy (`--model`) |
| --- | --- | --- | --- | --- |
| frontier | `fable` | `gpt-6.2-astra` | your provider's model | `gemini-4-argon` |
| strong | `opus` | `gpt-6.2-astra` | your provider's model | `gemini-4-neon` |
| standard | `sonnet` | `gpt-6.2-sol` | your provider's model | `gemini-4-hydrogen` |
| light | `haiku` | `gpt-6-luna` | your provider's model | `gemini-3.8-flash` |

Claude Code aliases follow the latest model of each family, so the configuration does not age.
Codex and `agy` use IDs from their current catalogs (`codex debug models`, `agy models`) and
may need updating as models are retired. For `agy`, crew passes base model names alongside
`--effort` (rejecting combinations where a suffixed slug like `gemini-4-neon-high` or an
`orchestrator` model is paired with `effort`). pi's model depends on the provider you log in
to, so profiles set only pi's `thinking` level and leave `model` alone.

Effort names are shared: `low`, `medium`, `high` and `xhigh` exist for all four agents
(Claude `--effort`, Codex `model_reasoning_effort`, pi `--thinking`, agy `--effort`). Codex
also accepts `ultra` (maximum reasoning with automatic task delegation) and pi clamps a level
to what its model supports.

Duties of an existing crew are guessed from the role names `init` generates: the board writer
is the lead (or solo, when it is the only role), names containing `review` or `research` are
reviewers or researchers, and other roles are developers. `herdr-crew config` shows the guess.

## Changing models later

`herdr-crew config` opens a terminal editor for the defaults and each role:

- **Agent**: the project default (`kind`) or a role's own; `inherit` removes the override.
- **Model**: a model from the agent's catalog, `custom…` for any other name, or `inherit`.
- **Effort**: the levels the agent accepts, or `inherit`.
- **`p`** applies a profile to every role at once, by duty; **`Del`** returns a cell to its
  inherited value; **`s`** saves.

In a Codex tree crew (`codex_tree = true`) the native subagents from `[[codex_workers]]` are
rows too: their model and effort are what the coordinator passes to `spawn_agent` (a worker
that sets one gets the coordinator's value for the other; one that sets neither inherits the
coordinator's), and their agent is always the coordinator's. Profiles, in setup and with `p`,
set only the roles, since per-subagent options were verified with Codex 0.172.0 only. Tree
crews keep their single coordinator's agent; to use another agent, set the crew up again
with `init`.

Every change is validated before it is applied, and saving keeps the file's comments and
formatting. Inherited values are shown dimmed with `↳`, and the footer shows the flags the
selected role launches with.

## When a change takes effect

Model and effort are launch options. They apply when crew starts a new conversation (`up` or
`add` over a tab without an agent). A running session keeps its model; change it inside the
session (`/model`, `/effort`) or close the tab and run `up`.

- **Codex** saves the launch options with each role binding, so recovery through
  `codex-resume` keeps the model and effort the conversation started with.
- **pi** records provider, model and thinking level in the session and restores them on resume.
- **Claude Code** is resumed by herdr with a plain `claude --resume <id>`, without crew's
  flags. After a herdr restart, a resumed Claude role may run with Claude Code's own default
  model and effort until a new conversation starts (not yet verified on a live restore). Set a matching `model` in Claude Code's settings,
  or use `/model` in the restored tab, if that matters for a role.

- **agy** is resumed by herdr with a plain `agy --conversation <id>` (via `herdr integration
  install antigravity-cli`), without crew's launch flags. Verify on your installation whether
  a restored conversation keeps its original model/effort or falls back to `settings.json`
  defaults; use `/model` or `/effort` in the restored tab if needed.

`fallback_model` (Claude only) passes `--fallback-model`, a comma-separated list Claude Code
tries when the main model is busy or unavailable.
