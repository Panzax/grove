
## Bootstrap protocol

Your task: "<TASK>"

You are NOT yet looping — `loop.md` has `active: false`. Bootstrap yourself:

1. Read `.grove/RALPH-LOOP.md` and `.grove/SHARED.md` so you understand the loop contract and the project's conventions.
2. **Invoke the plan workflow as a reasoning skill** — the same structured thinking your training uses when you would otherwise enter plan mode (decompose intent → identify code paths → enumerate concrete steps with verifiable success criteria). Use it inline here. **Do NOT actually call `ExitPlanMode` and do NOT wait for user approval — there is no user to approve.** The plan workflow is the *technique*, not a tool call. If your CLI has a `/plan` slash command, treat it as off-limits in this loop; running it would block on user input and freeze you forever.
3. Flatten the resulting plan into 3–10 small, verifiable workitems. Each workitem should be a sentence whose truth can be checked by reading files or running tests. Write them as `- [ ] <item>` lines under the `## Workitems` section of `$GROVE_AGENT_DIR/STATE.md` (replace the placeholder line that's there). STATE.md is the live, mutable form of your plan — you re-plan when scope changes by editing this file.
4. Edit `$GROVE_AGENT_DIR/PROMPT.md`:
   - Replace the `Agent-specific context` section with anything specific to your task (file paths, test commands, peer agents).
   - Tune the per-iteration step list if your work needs a different rhythm.
5. Edit `$GROVE_AGENT_DIR/loop.md` frontmatter:
   - Set `completion_promise` to a sentence that is TRUE only when every workitem in STATE.md is `[x]`. Be specific. Bad: "Done". Good: "All STATE.md workitems are [x] and `cargo test --all` passes."
   - Set `active: true`.
6. STOP THIS TURN. The Stop hook will fire, see `active: true`, and re-inject your PROMPT.md as the next turn. The loop begins from there.

Do not start the work itself in this bootstrap turn. Plan, write the files, stop.

**Autonomy rule:** every step in this loop must complete without waiting on a human. If a task seems to require user judgment, write down the decision you would have asked for, make the most defensible call yourself, log the decision + reasoning in STATE.md's iteration log, and proceed. Roadblock (`[!]`) only for true unsafe-without-input situations (credentials, irreversible destructive ops outside your worktree).
