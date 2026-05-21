
## No task specified

`loop.md` is `active: false`. You have no task yet.

To start: edit `$GROVE_AGENT_DIR/PROMPT.md` + `$GROVE_AGENT_DIR/STATE.md` to describe what you want to do, set `loop.md` `completion_promise` + `active: true`, then stop. The Stop hook will start the loop on the next turn.

If you don't want to do anything, exit the session and the user can `grove agents purge <AGENT_NAME>` to remove the state.
