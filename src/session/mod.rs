// Process / tmux session management.
//
// The "agent session" is an interactive Claude Code instance bound to a worktree
// with `GROVE_AGENT_DIR` exported. tmux is the default backend so the session
// survives detach, can be re-attached, and we can list/kill via tmux's named-
// session API.

pub mod container;
pub mod tmux;
