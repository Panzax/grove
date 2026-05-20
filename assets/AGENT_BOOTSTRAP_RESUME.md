
## Resuming after a session restart

This is a fresh claude session attached to an existing agent. Your previous loop state is intact.

Read `$GROVE_AGENT_DIR/loop.md` and `$GROVE_AGENT_DIR/STATE.md` to see where you left off.

- If `loop.md` has `active: true`, the Stop hook will continue the loop on the next Stop — do NOT redo bootstrap. Sit at this prompt and stop the turn; the next iteration's prompt will arrive.
- If `loop.md` has `active: false`, the loop is parked. Review STATE.md, fix anything that needs fixing, then flip `active: true` and stop.

The previous session's `session_id` has been cleared, so the Stop hook will accept this new claude session.
