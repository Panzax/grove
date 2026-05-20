
## Bootstrap protocol

Your task: "<TASK>"

You are NOT yet looping — `loop.md` has `active: false`. Bootstrap yourself:

1. Read `.grove/RALPH-LOOP.md` and `.grove/SHARED.md` so you understand the loop contract and the project's conventions.
2. Use the plan workflow on the task. Decompose it into 3–10 small, verifiable workitems. Each workitem should be a sentence whose truth can be checked by reading files or running tests.
3. Write the workitems as `- [ ] <item>` lines under the `## Workitems` section of `$GROVE_AGENT_DIR/STATE.md` (replace the placeholder line that's there).
4. Edit `$GROVE_AGENT_DIR/PROMPT.md`:
   - Replace the `Agent-specific context` section with anything specific to your task (file paths, test commands, peer agents).
   - Tune the per-iteration step list if your work needs a different rhythm.
5. Edit `$GROVE_AGENT_DIR/loop.md` frontmatter:
   - Set `completion_promise` to a sentence that is TRUE only when every workitem in STATE.md is `[x]`. Be specific. Bad: "Done". Good: "All STATE.md workitems are [x] and `cargo test --all` passes."
   - Set `active: true`.
6. STOP THIS TURN. The Stop hook will fire, see `active: true`, and re-inject your PROMPT.md as the next turn. The loop begins from there.

Do not start the work itself in this bootstrap turn. Plan, write the files, stop.
