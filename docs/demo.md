# End-to-end demo: one task through a reviewed crew

This walkthrough follows one small feature through the `review` preset: you hand a task to the lead, a developer implements it in its own worktree, a reviewer requests a change, the lead integrates the accepted commit, and a stopped session is brought back.

The project is `tinylink`, a small URL shortener, and the task is issue #12: links that expire. The `herdr-crew` commands and their output are real (herdr-crew 0.2.0, home paths shortened). The handoffs, commits and board contents are an example of what the sessions write; each board is drawn by `herdr-crew board --once` from that example `status.json`. Your agents will word things differently.

## 1. Set up the crew

```sh
cd ~/src/tinylink
herdr-crew init --preset review --name tl --yes
herdr-crew check
```

With no herdr server running, `check` prints:

```text
herdr-crew 0.2.0 (<plugin_root>/bin/herdr-crew)
herdr-crew: valid configuration in ~/src/tinylink/.herdr/crew.toml
herdr-crew: git 2.50.1 at /usr/bin/git
herdr-crew: herdr 0.9.3 at ~/.local/bin/herdr
herdr-crew: claude 2.1.291 at ~/.local/bin/claude
herdr-crew: worktrees.base origin/main is known locally
herdr-crew: 0 existing role worktree(s) are valid
herdr-crew: not checked: network access to origin, agent sign-in and model access
```

`init` wrote `.herdr/crew.toml` with three roles: `tl-lead` coordinates and writes the board in the main checkout, `tl-dev` implements and `tl-reviewer` reviews, each in its own worktree under `.worktrees/`. Read the generated prompts before going on; they are the team's working agreement. `up --dry-run` shows what will happen:

```text
tl-lead        create
tl-dev         create
tl-reviewer    create
tl-status      create
plan: 16 steps
  1. create workspace "tl" in ~/src/tinylink
  2. rename the initial tab to "tl-lead"
  3. create the worktree of tl-dev in ~/src/tinylink/.worktrees/tl-dev from origin/main
  4. create tab "tl-dev" in <new workspace> (~/src/tinylink/.worktrees/tl-dev)
  ...
  14. seed ~/src/tinylink/.herdr/status.json
  15. create tab "tl-status" in <new workspace> (~/src/tinylink)
  16. type in <pane of "tl-status">: ... board --root '~/src/tinylink' ...
```

Start it with `cd ~/src/tinylink && herdr` when no herdr server is running, or with `herdr-crew up --no-attach` otherwise. Each role confirms its role in one line and waits; answer Claude's folder-trust question in each new tab.

## 2. Give the lead a task

You talk to `tl-lead` like to any agent:

```text
Implement #12: links can have an optional expiry, and expired links return 410.
Done means tests for both cases, reviewed by tl-reviewer, integrated in main.
```

## 3. The lead assigns it

The lead turns the task into a bounded assignment and sends it to the developer, through you or directly (see [the channels](workflows.md#channels-between-sessions)). From a Claude lead, for example:

```sh
herdr agent prompt tl-dev "Assignment: #12 expiring links. Base: origin/main 3f2a1c9. \
Accept when: links accept an optional expiry; expired links return 410; tests cover both. \
You own: src/links.rs, tests/links.rs. Stop and ask if the storage format must change." \
  --wait --timeout 600000
```

It records the assignment on the board, including a decision it needs from you:

```text
Project tinylink
╭ tl-lead  ● waiting ──────────────────────────────────────────────────────────────────────────────╮
│ Now  #12 expiring links: waiting for tl-dev's delivery                                           │
│ Next 1. send the delivery to tl-reviewer                                                         │
│      2. integrate #12                                                                            │
╰────────────────────────────────────────────────────────────────────────────────────── <1 min ago ╯
╭ tl-dev  ● working ───────────────────────────────────────────────────────────────────────────────╮
│ Now  #12 feat/12-expiry from origin/main 3f2a1c9                                                 │
╰────────────────────────────────────────────────────────────────────────────────────── <1 min ago ╯
╭ tl-reviewer  ● idle ─────────────────────────────────────────────────────────────────────────────╮
│ Now  idle                                                                                        │
│ Next 1. review #12 once delivered                                                                │
╰──────────────────────────────────────────────────────────────────────────────────────────────────╯
╭ Waiting on you ──────────────────────────────────────────────────────────────────────────────────╮
│ Before main • decide the default expiry (7 or 30 days)                                           │
╰──────────────────────────────────────────────────────────────────────────────────────────────────╯
╭ Recent ──────────────────────────────────────────────────────────────────────────────────────────╮
│ 10-06 10:00  #12 assigned to tl-dev (base 3f2a1c9)                                               │
╰──────────────────────────────────────────────────────────────────────────────────────────────────╯
Updated 2026-10-06 10:01 by tl-lead (<1 min ago)
~/src/tinylink/.herdr/status.json  ·  checked 10:01:05
```

## 4. The developer delivers a commit

In its worktree, `tl-dev` creates `feat/12-expiry` from the assigned base, implements, runs the tests and delivers the exact commit to the lead:

```text
Delivery: #12 on feat/12-expiry, commit 8d41e07 (base 3f2a1c9).
cargo test: 48 passed. New tests: expired_link_returns_410, link_without_expiry_never_expires.
Limitation: the expiry is checked at read time; no cleanup job.
```

Worktrees share the repository's refs, so the commit is already visible to the other sessions without a push: `git log -1 8d41e07` works in the reviewer's worktree and in the main checkout.

## 5. Review, a decision, and a correction

The lead asks `tl-reviewer` to review 8d41e07 against the acceptance criteria:

```text
Project tinylink
╭ tl-lead  ● waiting ──────────────────────────────────────────────────────────────────────────────╮
│ Now  #12: waiting for tl-reviewer on 8d41e07                                                     │
│ Next 1. integrate #12                                                                            │
╰────────────────────────────────────────────────────────────────────────────────────── <1 min ago ╯
╭ tl-dev  ● waiting ───────────────────────────────────────────────────────────────────────────────╮
│ Now  #12 delivered as 8d41e07; waiting for review                                                │
╰─────────────────────────────────────────────────────────────────────────────────────── 2 min ago ╯
╭ tl-reviewer  ● working ──────────────────────────────────────────────────────────────────────────╮
│ Now  #12 reviewing 8d41e07 against the acceptance criteria                                       │
╰────────────────────────────────────────────────────────────────────────────────────── <1 min ago ╯
╭ Waiting on you ──────────────────────────────────────────────────────────────────────────────────╮
│ Before main • decide the default expiry (7 or 30 days)                                           │
╰──────────────────────────────────────────────────────────────────────────────────────────────────╯
╭ Recent ──────────────────────────────────────────────────────────────────────────────────────────╮
│ 10-06 10:24  tl-reviewer started on 8d41e07                                                      │
│ 10-06 10:22  tl-dev delivered 8d41e07: cargo test 48 passed                                      │
│ 10-06 10:00  #12 assigned to tl-dev (base 3f2a1c9)                                               │
╰──────────────────────────────────────────────────────────────────────────────────────────────────╯
Updated 2026-10-06 10:24 by tl-lead (<1 min ago)
~/src/tinylink/.herdr/status.json  ·  checked 10:24:05
```

"Waiting on you" is where the board collects what only you can decide. You answer the lead: 30 days. The reviewer requests a change:

```text
Changes requested on 8d41e07: src/links.rs:88 compares the expiry with the local clock
instead of UTC, so a link created in UTC-3 expires three hours late.
Reproduce: TZ=America/Santiago cargo test expired_link.
```

The lead forwards the finding and the 30-day default to `tl-dev`, which delivers 2b7c9e0; `tl-reviewer` validates that exact commit in its worktree and accepts it.

## 6. The lead integrates

The lead merges the accepted commit in the main checkout, runs the combined checks and reports to you:

```sh
git merge --ff-only feat/12-expiry
cargo test
```

```text
Project tinylink
╭ tl-lead  ● idle ─────────────────────────────────────────────────────────────────────────────────╮
│ Now  idle                                                                                        │
│ Note #12 integrated in main as 2b7c9e0                                                           │
╰────────────────────────────────────────────────────────────────────────────────────── <1 min ago ╯
╭ tl-dev  ● idle ──────────────────────────────────────────────────────────────────────────────────╮
│ Now  idle                                                                                        │
╰─────────────────────────────────────────────────────────────────────────────────────── 6 min ago ╯
╭ tl-reviewer  ● idle ─────────────────────────────────────────────────────────────────────────────╮
│ Now  idle                                                                                        │
╰─────────────────────────────────────────────────────────────────────────────────────── 4 min ago ╯
╭ Waiting on you ──────────────────────────────────────────────────────────────────────────────────╮
│ Optional    • push main when you are ready                                                       │
╰──────────────────────────────────────────────────────────────────────────────────────────────────╯
╭ Recent ──────────────────────────────────────────────────────────────────────────────────────────╮
│ 10-06 10:48  #12 integrated in main (2b7c9e0); combined cargo test passed                        │
│ 10-06 10:44  tl-reviewer accepted 2b7c9e0                                                        │
│ 10-06 10:42  tl-dev fixed the UTC comparison in 2b7c9e0                                          │
│ 10-06 10:33  tl-reviewer requested changes on 8d41e07                                            │
│ 10-06 10:30  user chose 30 days as the default expiry                                            │
╰──────────────────────────────────────────────────────────────────────────────────────────────────╯
Updated 2026-10-06 10:48 by tl-lead (<1 min ago)
~/src/tinylink/.herdr/status.json  ·  checked 10:48:05
```

Pushing `main` stays your call.

## 7. Recover a stopped session

Sessions stop: an agent exits, a tab gets closed, the machine restarts. `up --dry-run` tells you what crew sees, and `up` repairs only what is missing.

- **The agent exited but the tab is still there.** `up` does not start a new conversation over the old one; it tells you how to resume it in that tab:

  ```text
  tl-dev         no agent
  warning: tab tl-dev has no agent; restart it there with `claude -r <session-id> -n tl-dev`
  ```

  Run that command in the `tl-dev` tab. For a Codex role, use `herdr-crew codex-list` and then `herdr-crew codex-resume <binding>` in its tab.
- **The tab was closed.** `herdr-crew up` creates the tab again, reuses the existing worktree with its branch and changes, and starts a fresh session with the role prompt. Tell it where it left off; the board and the delivered commits are the record.
- **herdr restarted.** herdr restores the workspace and resumes the agents; the startup hook only repairs the board tab. If you started herdr while a server was already running, run `herdr-crew up --no-attach` from the project.
- **Two commands at once.** `up`, `add` and `close` on the same project wait for each other, so a repair cannot race a bring-up.

## 8. More hands, then fewer

For an independent task, the lead can ask you to approve another developer, then run `herdr-crew add tl-dev` to start `tl-dev-2` in its own worktree. `herdr-crew close tl-dev-2` closes its tab when the work is integrated and keeps its worktree; `git worktree remove .worktrees/tl-dev-2` deletes it.

## What this demo does not show

- herdr-crew does not route these messages: the lead, the developer and the reviewer hand work over through you, Claude Code messaging or `herdr agent` commands, as their prompts say.
- Agent output above is illustrative. Results depend on your agents, prompts and repository.
- Every running session costs tokens. Start with the smallest crew that can deliver a verifiable result; see [docs/workflows.md](workflows.md).
