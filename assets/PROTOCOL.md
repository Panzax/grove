# Inter-agent collaboration protocol

How parallel agents in worktrees talk to each other and share code. The
mechanism is a **blackboard / mailbox on the shared filesystem** — `.grove/bus/`
at the repo root, reached by every worktree because per-agent state lives
centrally (see `.grove/RALPH-LOOP.md`). The bus is **gitignored** and
runtime-created; the framework files describing it are tracked.

Decoupled agents reading/writing a shared structured space is the classic
blackboard architecture and the most reliable design for turn-based agents —
pipes and queues don't fit a model that acts in discrete turns, has no event
loop, and persists state in files anyway.

## Directory layout

```
.grove/bus/                       # all paths below are runtime-created
  log.d/<ISO8601>-<sender>.md     # broadcast feed — one file per event
  inbox/<recipient>/              # direct messages: one file per message
    <ISO8601>-from-<sender>.md
    archive/                      # recipient moves read mail here
  contracts/                      # negotiated interface agreements
    <feature-pair>.md             # e.g. auth__token__schema.md
```

Single uid (typically `vscode` in the devcontainer) means OS file permissions
don't enforce isolation between agents — the bus is **convention-backed**, not
sandboxed. Agents are assumed cooperative.

## Broadcasts — `bus/log.d/`

> Use for: status pings, anything you want all agents to see, "I just touched
> module X", end-of-iteration summaries.

**Posting:** write a *new file* `bus/log.d/<ISO8601>-<sender>.md` (or
`grove msg broadcast "<text>"`). Don't append to a shared log — concurrent
appends from multiple agents interleave and corrupt. One file per event is
atomic and race-free.

**Reading:** glob `bus/log.d/*` and consume entries newer than the timestamp
you stored at the end of your last iteration. Track the last-seen timestamp
in your own `STATE.md` (`bus_last_seen: ...`).

**Format (suggested):**

```markdown
---
ts: 2026-05-20T22:00:00Z
from: feat-a
kind: status | touched | note | summary
---

One short paragraph. Files touched. Anything peers should know.
```

## Direct messages — `bus/inbox/<recipient>/`

> Use for: a specific question or request to one peer. The recipient is
> obligated to read; the sender is obligated not to spam.

**Sending:** write `bus/inbox/<recipient>/<ISO8601>-from-<sender>.md` (or
`grove msg <recipient> "<text>"`). Don't overwrite — each message is its own
file.

**Reading (every iteration):** list `bus/inbox/<self>/` (ignoring `archive/`).
Process every message. Then `mv` each one into `archive/` so it's not
processed twice. (Don't `rm`; archived mail is the audit trail.)

If you can't act on a message yet, drop a brief reply into the sender's
inbox and *still* archive the original (it has been received).

## Contracts — `bus/contracts/`

> Use for: a stable interface agreement between two (or more) agents — a
> function signature, a JSON schema, a CLI flag set. Contract-first: agree on
> the interface before either side implements it, so neither has to re-do
> work after merging.

One file per contract: `bus/contracts/<feature-pair>.md`. Both sides edit it
by appending revisions, not overwriting. Once agreed, **commit a stub** of the
agreed shape onto the `agent/shared` branch (below) — that's the canonical
source-of-truth that both implementations target.

## Code sharing — the `agent/shared` hub branch

> The rule: **never merge peer-to-peer.** All cross-agent code sharing goes
> through one hub branch. This keeps the graph near-tree and the final
> `grove integrate` clean.

- When an agent produces something reusable (a contract stub, a shared
  fixture, a utility module), it commits that to `agent/shared`.
- Other agents merge `agent/shared` into their own `agent/<name>` branch to
  pick it up: `git merge agent/shared`.
- If a dependency is **foundational** (e.g. an interface both features build
  against from day one), commit it to the project's base branch *before*
  fanning out the agents. Don't try to back-fill the base.
- Sharing *information / intent / a contract* → the bus (cheap, fast).
  Sharing *actual code* → `agent/shared` (deliberate, reviewed).

## Visibility — peers' state

All agents share the host uid and `.grove/agents/` is one shared dir, so
**every agent can read every other agent's `STATE.md`** at
`.grove/agents/<peer>/STATE.md`. Use this — it's how you see what a peer is
working on without sending a message. *Don't* write to a peer's state; treat
it as read-only.

`grove agents list` and `grove loop` print every loop's status header +
iteration counter — useful for the human supervisor.

## `grove integrate` sees the bus

When `grove integrate` runs, the conflict-resolver agent is given a
read-only snapshot of `bus/log.d/` plus each merged branch's `STATE.md`
under `worktrees/.integration/.grove-context/`. So conflicts can be resolved
with *intent context*, not just syntactic merge logic. The integration
worktree itself does **not** run a loop.

## Naming & hygiene

- Agent names: kebab-case, letters/digits/`-`/`_`, no slashes.
- Timestamps: ISO 8601 UTC.
- Don't delete bus files — archive them. The log is the audit trail.
