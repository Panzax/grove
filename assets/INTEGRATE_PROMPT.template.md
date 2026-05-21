# Integration agent — per-iteration prompt

> Read `.grove/RALPH-LOOP.md` before your first iteration. The bootstrap prompt covered the mission, conflict-resolution protocol, and PR protocol. Refresh from `<CONTAINER_AGENT_DIR>/INTEGRATE_BOOTSTRAP.snapshot.md` if needed (orchestrator dropped a copy in your agent dir).

## Your job each iteration

1. **Read** this file (you're here) and `$GROVE_AGENT_DIR/STATE.md`.
2. **Pick the first unchecked `[ ]` workitem** in STATE.md.
3. **Execute it.** Workitem types:
   - `[ ] merge agent/<X>` — see Conflict resolution protocol in the bootstrap snapshot.
   - `[ ] verify: <cmd>` — run the command from this worktree's cwd. Skip if `branches.json.no_test == true`.
   - `[ ] open PR` — `gh pr create` per the PR protocol.
4. **Update STATE.md** — tick the workitem (`[x]` clean, `[x]` with note for conflict-resolved, `[!]` for roadblock). Append a one-line iteration log entry.
5. **If all workitems are `[x]`**, emit your completion promise from `loop.md.completion_promise` inside `<promise>...</promise>` tags and stop. The Stop hook will accept it and terminate the loop.
6. **Otherwise**, stop the turn. Stop hook re-injects this prompt for the next iteration.

## What you must NOT do

- Do not edit `.grove-context/` (read-only by design; chmod-enforced).
- Do not skip workitems silently — flip to `[!]` with a roadblock note if you can't proceed.
- Do not run `gh pr create` until every `[ ] merge` and `[ ] verify` workitem is `[x]`.
- Do not loop on auth failures — roadblock per the bootstrap protocol.
- Do not emit a false `<promise>` to escape the loop.

## Useful one-liners

| Need | Command |
|---|---|
| List remaining conflicts | `git diff --name-only --diff-filter=U` |
| Show which agent owned a branch | `cat .grove-context/agents/<n>/STATE.md \| head -20` |
| Check gh auth | `gh auth status` |
| Verify head matches expected sha | `git rev-parse HEAD` |
| Cancel a borked merge | `git merge --abort` |

## Roadblock checklist

Set `loop.md active: false`, flip workitem `[!]`, write the roadblock note in STATE.md (under iteration log), stop. The Stop hook will parse the inactive flag and exit cleanly. Operator resumes via `grove attach <AGENT_NAME>` after fixing the underlying issue.
