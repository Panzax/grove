# Grove Ralph loop — authoring guide

**Read this before writing or initializing a per-agent loop.** Adapted from the
official `ralph-loop` plugin and proven out in our freqtrade harness. We use a
file-driven setup — there is no slash command and no
`.claude/ralph-loop.local.md`. State lives in `.grove/agents/<name>/`.

## What a Ralph loop is

An autonomous agent that re-reads a prompt + state file every iteration, does
the smallest next task, commits, and repeats. The point is **fresh context
each iteration**: progress lives in the codebase, git history, and the loop's
own `STATE.md` — not in the LLM's context window. The name comes from Ralph
Wiggum: stumble forward with naive confidence, but iterate until done.

## Mechanism

A `Stop` hook (`.grove/tools/loop-hook.sh`) intercepts session exit. If the
agent's `loop.md` says the loop is active and incomplete, the hook emits
`{"decision":"block","reason":"<prompt body>"}` — Claude treats this as a new
user turn, re-feeding the prompt for the next iteration. The hook is registered
into `~/.claude/settings.json` by `grove init` (idempotent jq-style patch).

State files per agent live in `.grove/agents/<name>/` (central, gitignored,
**survives `grove remove`**):

| File | Role |
|---|---|
| `PROMPT.md` | What the agent reads each iteration (seeded from `PROMPT.template.md`; edit per agent) |
| `STATE.md` | Workitem checklist + iteration log + roadblock markers — the agent **edits this** each iteration |
| `loop.md` | YAML frontmatter the hook reads + a short pointer body |

`grove spawn` seeds these on `grove spawn <name>` and exports
`GROVE_AGENT_DIR=<repo>/.grove/agents/<name>` so the hook (and the agent) find
them. When `GROVE_AGENT_DIR` is unset or `loop.md` is absent, the hook
**`exit 0`s** — so the main workspace, integration worktrees, and any non-
agentic Claude session are silently unaffected.

## `loop.md` frontmatter

```
---
active: true | false
iteration: <int>
max_iterations: <int>          # 0 means unlimited
completion_promise: "<text>"   # the agent emits <promise>text</promise> to stop
session_id: "<filled by grove spawn>"
---

Read $GROVE_AGENT_DIR/PROMPT.md and do the next smallest task.
```

- **`active: false`** — the hook `exit 0`s regardless. Use this to park a
  halted loop without auto-spinning.
- **`max_iterations`** — when `>0` and reached, the hook stops the loop.
  **Always set a sensible value.** The primary safety net against runaways.
- **`session_id`** — isolation guard. The hook compares this against the
  session's own ID and bails if they differ — so two sessions can't drive the
  same loop. `grove spawn start_session` refreshes this on launch; stale
  values will silently block the loop, so don't hand-edit it.
- **`completion_promise`** — set this to a sentence that is true only when the
  whole task is done (e.g. `"All workitems in STATE.md are [x]"`). The agent
  ends the loop by emitting `<promise>that-exact-sentence</promise>` in a
  reply.

### The `<promise>` completion contract — CRITICAL RULE

> If a completion promise is set, you may ONLY output it when the statement is
> completely and unequivocally TRUE. **Do not output false promises to escape
> the loop**, even if you think you're stuck or should exit for other reasons.
> The loop is designed to continue until genuine completion.

If you're stuck, write the roadblock into `STATE.md` and set `active: false` —
do not lie your way out. This rule is carried verbatim from the upstream Ralph
plugin.

## Per-iteration loop protocol

Every iteration the agent must, in order:

1. **Read `$GROVE_AGENT_DIR/PROMPT.md`** (you're reading it now if the hook
   re-injected) and **read `$GROVE_AGENT_DIR/STATE.md`**.
2. **Read the bus:** new files in `.grove/bus/inbox/<self>/` and any new
   broadcasts in `.grove/bus/log.d/`. See `.grove/PROTOCOL.md`.
3. **Pick the first unchecked `[ ]` task** in `STATE.md` whose dependencies are
   met. If none — emit the completion promise.
4. **Do the smallest correct change.** Reuse existing patterns. Add/extend
   unit tests. Tests must pass before commit.
5. **Self-review the diff** for bugs, wrong assumptions, missing edge cases.
6. **Commit** with a clear message. Push to `origin` only.
7. **Update `STATE.md`** — flip the box to `[x]` and append a one-line log
   entry. If a task's premise was wrong, flip to `[!]` and document.
8. **Post to the bus** (if relevant): drop messages in
   `.grove/bus/inbox/<other>/`, broadcast to `.grove/bus/log.d/`, write a
   `bus/contracts/` entry. Archive read inbox mail to
   `inbox/<self>/archive/`.
9. **Loop again** (the `Stop` hook will re-feed PROMPT.md).

## Roadblock — when to halt the loop

Set the task's checkbox to `[!]`, write a clear roadblock note in `STATE.md`,
and **emit the completion promise** to stop the loop (or set `active: false`).
A loop with a roadblock should not spin. Roadblock conditions:

- Needs a user decision unresolvable from the spec/codebase.
- Needs credentials, an account, or external setup the user must provide.
- A test failure that survives systematic debugging.
- Premise of the task was wrong — building further would compound the error.

A task that's "buildable test-first against the spec" is **not** a roadblock.

## Prompt-authoring best practices

1. **Set `max_iterations`.** Even when the work is genuinely open-ended. The
   counter is the safety net.
2. **Verifiable completion criteria.** The `completion_promise` should be a
   sentence any reader can check by looking at files/tests — not a vibe.
3. **Incremental goals.** A `STATE.md` checklist of small tasks beats one
   "build the feature" prompt. The loop's strength is many small correct steps.
4. **Self-correction loop.** Bake "run the tests / self-review the diff" into
   the per-iteration steps. The model lying-to-itself is the #1 failure mode.
5. **Escape hatches.** Tell the agent how to roadblock (write `[!]`, set
   `active: false`) — so it doesn't paint itself into a corner.
6. **Reference `SHARED.md`.** Don't repeat project-wide rules in every
   per-agent PROMPT.md — point at `.grove/SHARED.md` for branch model, safety
   rules, locked user decisions.

## When to use Ralph

✅ Well-defined work with verifiable success criteria (build a connector,
   migrate a module, harden a class with fault-injection tests). Greenfield
   incremental builds. Refactors with passing tests as the goalpost.

❌ Design decisions that need judgement, one-shot operations, work without
   clear pass/fail criteria, urgent production debugging where you need to see
   each step.

## How to initialize a new loop here

Don't author the files by hand — `grove spawn` seeds them:

```
grove spawn <name>                       # new worktree + branch + seeded loop
grove spawn <name> --branch <existing>   # base the worktree on an existing branch
grove spawn <name> --task "<desc>"       # seed STATE.md with one initial task
```

That creates `worktrees/<name>` (on `agent/<name>` or the supplied branch),
seeds `.grove/agents/<name>/PROMPT.md` from `.grove/PROMPT.template.md`,
initializes `STATE.md` and `loop.md`, sets `chmod 0444` on the worktree's
`SHARED.md`, and attaches a tmux session whose Claude instance has
`GROVE_AGENT_DIR` exported.

Then edit `.grove/agents/<name>/PROMPT.md` and `STATE.md` to describe the work
(don't touch `SHARED.md` — it's the shared layer). Set `loop.md`'s
`max_iterations` and `completion_promise`, flip `active: true`, and the next
`Stop` will start the loop.

See `.grove/PROTOCOL.md` for the inter-agent collaboration bus and the
`agent/shared` hub-branch convention.
