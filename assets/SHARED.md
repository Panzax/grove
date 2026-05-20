# SHARED.md — canonical project context

> This file is **read-only for spawned agents**. Edits should be deliberate,
> ideally on `agent/shared` or the base branch, so every agent's worktree
> sees the same canon. `grove spawn` chmod's this file to `0444` in each
> worktree as a per-worktree speedbump against accidental writes.

This file is the place to record:

1. Project goal, scope, and what's explicitly out of scope.
2. Branch model (e.g. `develop`, `main`, integration conventions).
3. Locked user decisions: dependencies, libraries, version pins, deployment
   targets.
4. Safety rails: things never to commit (real secrets, generated data), things
   never to do without human approval (`force-push`, schema migrations).
5. Verification: how an iteration knows it succeeded (test command,
   linter command, type-check command). These should match
   `.grove/config.toml [verify]` so agents and `grove integrate` agree.
6. Conventions: code style, commit-message format, file naming.

Update this file via PR onto `agent/shared` (or the base branch) — never
edit it from a per-agent worktree.

---

## Project

(fill in: one-paragraph description of what this repo does and why)

## Stack

(fill in: detected stack, runtime version, key libraries the agent should
prefer / avoid)

## Branch model

- Base branch: `main` (update if different)
- Agent branches: `agent/<name>` (created by `grove spawn`)
- Code-sharing hub: `agent/shared`
- Integration: `integration/<timestamp>` (created by `grove integrate`)

## Verification

```
test:      (e.g. cargo test --all)
lint:      (e.g. cargo clippy -- -D warnings)
format:    (e.g. cargo fmt --check)
typecheck: (e.g. cargo check --all)
```

These should match `.grove/config.toml [verify]`.

## Safety rails

- (fill in: never commit X)
- (fill in: never run Y without confirmation)
- (fill in: always Z before pushing)

## Style / conventions

(fill in)
