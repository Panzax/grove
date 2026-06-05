# ADR 0001 — Running grove agents on a project that already ships a devcontainer

**Status:** Accepted (design)
**Date:** 2026-06-05
**Context source:** deep-research workshop (24 sources, 25 claims adversarially verified; key citations inline).

## Problem

Grove runs Claude Code agents in a per-project container (one container, N agents on git worktrees; egress via `git push`). Many target projects **already ship their own devcontainer** to build/run/test themselves — e.g. freqtrade, which uses a Docker devcontainer (freqtrade image, market-data mounts, `pip install -e`) and even runs *itself* in Docker for backtests. When grove initializes such a project, what container architecture should it use? Two intuitions:

1. **Merge** — agents run inside the project's own devcontainer so they get the exact runtime the project targets.
2. **Docker-in-Docker / sibling** — grove's agent container has Docker access and spins up the project's devcontainer as a nested/sibling container, keeping the agent env and project runtime separate.

## What the research settles

- **"Merge two devcontainers by nesting" is not a thing.** The devcontainer spec is *explicitly not a multi-container orchestrator* and provides **no way to nest one `devcontainer.json` inside another**; multi-container composition is delegated to Docker Compose. (3-0 — [containers.dev spec](https://containers.dev/implementors/json_reference/), [devcontainers/spec#10](https://github.com/devcontainers/spec/issues/10))
- **Spec-blessed ways to combine instead:**
  - **Extend the project's image + add tooling via `features`** — drop `image`, use `build`/`dockerfile` on the project's base, or layer Feature IDs (incl. `docker-in-docker`) as cacheable layers. (3-0 — [containers.dev/guide/dockerfile](https://containers.dev/guide/dockerfile))
  - **Compose composition** — each service gets a `devcontainer.json` pointing at a **shared `docker-compose.yml`** (`dockerComposeFile` + `service`); a devcontainer can **attach to a service in the project's *existing* compose**, inheriting its runtime + sibling services (db/redis) + mounts. (3-0 — [VS Code multi-container](https://code.visualstudio.com/remote/advancedcontainers/connect-multiple-containers))
  - A `baseImage` build property (base + features, no Dockerfile) is **proposed but not adopted**. (2-1 — [devcontainers/spec#74](https://github.com/devcontainers/spec/issues/74))
- **When agents must actually build/run the project's containers** (freqtrade backtests), the agent container needs a Docker daemon. Three options:

  | Approach | Mechanism | Tradeoffs |
  |---|---|---|
  | **DooD** (docker-outside-of-docker) | bind-mount host `/var/run/docker.sock` → **sibling** containers on host daemon | Low overhead, **reuses host build cache**; but **path-mismatch** (container ≠ host paths), **cannot mount container-only folders**, and **RW socket = host-root-equivalent** (3-0 — [VS Code](https://code.visualstudio.com/remote/advancedcontainers/use-docker-kubernetes), [OWASP](https://cheatsheetseries.owasp.org/), [quarkslab](https://blog.quarkslab.com/)) |
  | **DinD** (docker-in-docker) | nested daemon via the `docker-in-docker` feature; needs `--privileged` | Isolated cache; **can bind-mount any in-container path** (right pick when mounting agent files into nested containers). (3-0) |
  | **microVM sandbox** | each agent gets its **own daemon in a microVM, no host socket, no host privilege** (Docker Sandboxes) | Modern precedent, **purpose-built for "AI agent that builds/runs a containerized project."** (3-0 — docker.com) |

## Decision

**Default to extend/compose ("merge"); add Docker-in-the-agent only when the project's workflow truly requires building/running containers; and when it does, prefer an isolated daemon over the host socket.**

Mapped to grove's backend + preset system:

1. **No project devcontainer** → grove's own preset (current behavior). Unchanged.
2. **Project ships a devcontainer, agents only edit + run its commands** → grove **detects and extends it** (see sketch below). Agents get the project's runtime (deps, mounts) natively; grove layers its own tooling on top. This is the common case and the cleanest realization of the "merge" intuition.
3. **Agents must spin up the project's *own* containers** → grant a daemon via the **`docker-in-docker` feature (privileged)**, or — preferably — grove's **sandbox-backend / microVM-isolated daemon**. **Do not mount the host RW Docker socket into agent containers** (host-root + path-mismatch).

So: "merge" wins for the common case; "docker-in-docker" is the *fallback*, not the default — and its security-correct form is an isolated daemon, not the host socket. Grove's deferred **sandbox-mode** plan is already aligned with the best-practice endpoint.

## Proposed grove implementation — "detect-and-extend an existing project devcontainer" preset

A new preset path in the setup wizard (`src/agent/setup.rs` / `src/devcontainer/`), selected automatically when `detect_project_context` finds a project `.devcontainer/devcontainer.json`:

- **Detect:** during init, if `.devcontainer/devcontainer.json` already exists, offer **"Extend the project's devcontainer"** as the pre-selected preset (alongside the curated presets).
- **Extend, don't overwrite:** instead of writing grove's own `image`/preset, generate a grove devcontainer that **builds FROM the project's image** (or references the project's compose `service`), and **layers grove's required tooling via `features` + a small Dockerfile fragment**:
  - `ghcr.io/devcontainers/features/common-utils` (or an explicit apt step) to guarantee **tmux** — grove runs every agent in tmux, so a project devcontainer that lacks tmux (e.g. agent-studio) currently breaks grove. **Grove must inject tmux when extending** (this is a known onboarding bug, see below).
  - the Anthropic `claude-code` feature, `git`, `github-cli`.
  - grove's `agent.tmux.conf` (truecolor RGB — see the TUI work) and the `.grove` control-plane wiring.
- **Preserve the project's contract:** keep the project's `remoteUser`, mounts (e.g. freqtrade's `MARKET_DATA_DIR`), and lifecycle commands; grove *adds* to `postCreateCommand`/features rather than replacing them. The derived `remote_user` comes from the project image (no hardcoding — consistent with the Part C de-hardcoding work).
- **Compose case:** if the project uses `docker-compose.yml` with a dev service, grove generates its agent `devcontainer.json` with `dockerComposeFile` pointing at the project's compose + `service` set to the dev service, so freqtrade's db/redis siblings and network come for free.
- **Docker-needing projects:** if the project's test/build commands invoke Docker, the wizard offers to add the `docker-in-docker` feature (privileged) OR route through the sandbox-backend's isolated daemon — never the host socket.

### Known onboarding bug this fixes
**tmux missing in an extended/existing devcontainer breaks grove** (hit live on agent-studio). Grove assumes tmux; when reusing a project's devcontainer it must guarantee tmux is installed (feature or apt in the grove-added layer), and fail-fast with a clear message if it's absent rather than producing a cryptic tmux error.

## Consequences

- Grove's value proposition strengthens: agents inherit the *real* project runtime instead of a generic preset that can't actually build/test the project.
- The setup wizard gains a branch (detect → extend vs fresh preset); the de-hardcoded `remote_user` derivation already supports arbitrary project users.
- The sandbox-backend work is the right home for the "agent needs Docker, securely" case — this ADR is a forcing function to keep it isolated-daemon-first, not host-socket.

## References (verified)

- devcontainer spec / json reference — https://containers.dev/implementors/json_reference/
- Orchestrator interop (no native nesting) — https://github.com/devcontainers/spec/issues/10
- Extend via Dockerfile / features — https://containers.dev/guide/dockerfile
- Multi-container / attach to existing compose — https://code.visualstudio.com/remote/advancedcontainers/connect-multiple-containers
- Docker in dev containers (DinD vs DooD) — https://code.visualstudio.com/remote/advancedcontainers/use-docker-kubernetes
- `baseImage` proposal (unadopted) — https://github.com/devcontainers/spec/issues/74
- Docker socket = host root — OWASP Docker Security Cheat Sheet; https://blog.quarkslab.com/
- Docker Sandboxes (per-agent microVM daemon) — docker.com
