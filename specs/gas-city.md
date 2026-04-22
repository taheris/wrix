# Gas City Integration

Integrate [Gas City](https://github.com/gastownhall/gascity) into wrapix as the
multi-agent orchestration layer, with opinionated Nix defaults that let consumers
run autonomous ops loops with minimal configuration.

## Problem Statement

Wrapix provides secure sandboxed containers and Ralph drives single-agent
spec-to-implementation workflows. But production systems need continuous,
autonomous operation: monitoring for errors, fixing them, reviewing fixes for
quality, and deploying safely. This requires coordinating multiple agent roles
running concurrently — something Ralph was not designed for.

Gas City is an orchestration SDK that manages parallel AI agent sessions via a
Kubernetes-style reconciliation loop. By integrating Gas City as the orchestration
layer and wrapix as the execution environment, consumers get a complete autonomous
ops pipeline with good defaults and minimal configuration.

## Requirements

### Architecture

- Gas City (`gc`) binary bundled as a Nix dependency, pinned in the flake
- Wrapix generates `city.toml` from Nix expressions — consumers never write TOML
- Provider implemented as a shell script using Gas City's `exec:<script>` pattern
- Provider translates `gc` commands to `podman` operations
- One city per project; service containers are managed by the wrapix provider
  script via podman, not gc's native service abstraction (which handles
  workflow/proxy_process types, not OCI containers)
- `mkSandbox` remains the foundational primitive, unchanged
- `mkCity` uses `mkSandbox` internally for agent container images
- `mkCity` generates `city.toml`, the provider script, and container images at
  Nix build time — these are deterministic outputs in the Nix store
- All agents use a custom `scale_check` — a single `bd list` query instead
  of gc's default two-query check, which times out under dolt contention
  (gc hardcodes a 30s timeout for scale_check that cannot be configured)

**Components running on the host (per-city systemd unit):**

| Component | Role |
|-----------|------|
| `gc start --foreground` | Controller — reconciliation loop, convergence, orders, scheduling. Runs on the host, not in a container |
| Provider script | Translates gc commands to podman operations, manages worktrees, mounts bead context |
| Controller query API | Read-only session/status queries over the controller unix socket, accessible from containers |
| Controller mail handler | Receives intent-based mutation requests from agents via mail |
| Post-gate order | Event-gated order triggered by `convergence.terminated` — notifies judge, deploy bead creation |
| Gate condition script | Bridges convergence gate and judge session — nudges judge, waits for verdict |
| Entrypoint wrapper | Ensures `beads-dolt` is running and attached to the city network, prints pending reviews, runs recovery, kills stale containers on config drift, starts events watcher, stages gc home, runs `gc start --foreground` with trap-based cleanup (of city-owned resources only — `beads-dolt` persists) |

### Deployment Model

- gc runs **on the host** as a per-city systemd service invoking
  `gc start --foreground`. It is not itself containerized. Agent role
  containers (mayor, scout, judge, workers) are spawned as siblings by
  `provider.sh` via the local podman socket
- All agent/service containers share a per-city podman network
  (`wrapix-<city-name>`)
- Service containers are built from Nix packages via `dockerTools.streamLayeredImage`
- NixOS module generates the systemd unit, the per-city podman network, and
  invokes `mkCity` to produce `city.toml` and container images

**Dual-mode gc:**

- **Host-side gc**: direct provider access via podman (unchanged)
- **Container-side gc**: persistent role containers set
  `GC_SESSION=exec:/workspace/.gc/scripts/provider.sh`, which tells gc
  to use the exec provider directly. The provider's tmux methods work
  cross-container because tmux sockets live on the shared `.wrapix/tmux/`
  mount (see Shared Tmux Sockets). Read-only commands (status, session
  list) still go through the controller unix socket. No upstream gc
  changes required.
- Workers use `GC_SESSION=worker` (falls through to gc's built-in tmux
  provider) since they run their own local tmux and don't participate in
  cross-container communication

- Dolt is provided by `beads-dolt`, a separate per-workspace container
  managed by `lib/beads` and shared between host-side `bd` and the city.
  Its name and published port are derived from `sha256(workspace_path)`,
  so one workspace has exactly one dolt container regardless of how many
  tools use it
- The entrypoint ensures `beads-dolt` is running and runs
  `beads-dolt attach <city-network>` so role containers can reach it by
  container hostname. The city does not own the dolt lifecycle —
  `beads-dolt` persists across city restarts and host shell sessions
- The city must not corrupt the host's `.beads/` directory — gc discovers
  `.beads/` by walking up from its cwd, so the entrypoint stages an
  isolated `.gc/` scaffold and points gc's beads access at the shared
  `beads-dolt` container instead of letting gc auto-start its own dolt
- `gc stop` from the host shell must work — the controller socket must be
  reachable from the workspace directory (symlinked from the staged gc
  home into the workspace `.gc/`)

### Roles

Four agent roles in the ops loop (city government theme):

| Role | Job | Lifetime | Workspace Access | Host Access | gc Path |
|------|-----|----------|-----------------|-------------|---------|
| **Mayor** | Human's conversational interface — triage, status briefing, executes approved actions | Persistent | Read-only + `.beads/` rw | None | Controller socket + mail |
| **Scout** | Watches service containers, detects errors, creates beads, system housekeeping | Persistent | Read-only + `.beads/` rw | Read-only podman (logs, inspect) | Controller socket + mail |
| **Worker** | Picks up a bead, investigates, writes the fix | Ephemeral (per bead) | Read-write (own worktree) | None | Controller socket + mail |
| **Judge** | Reviews every worker's output, enforces style guidelines, owns merge | Persistent | Read-write (for merge) + `.beads/` rw | None | Controller socket + mail |

The human interacts primarily through the Mayor via `gc session attach mayor`.
Direct CLI access (`gc`, `bd`, `ralph`) is always available as a bypass.

### Ops Loop

```
Scout (watching) --> creates bead --> Worker (fixes) --> Judge (reviews + merges)
   ^                                                          |
   |                                              +-----------+
   |                                              v           v
   |                                           merge       reject --> Worker retry
   |                                              |           |
   |                                              v           v (loop > 1)
   +------------------------------------------ deploy     Mayor notified
                                                              |
                                                              v
                                                     Human (via attach)
```

- Scout is a persistent session. gc orders poke it on a polling interval
  (configurable, default 5 minutes). gc's `session_sleep` auto-suspends
  idle scouts; the next order or gc mail restarts it fresh. This keeps
  context clean while maintaining addressability for gc mail.
- Hybrid event trigger: the entrypoint wrapper starts a background process
  watching `podman events` for service container lifecycle events (die, oom,
  restart) and wakes the scout immediately via `gc nudge scout --message "..."`.
  Nudge is push-based (directly sends to the session). Service containers are
  not gc sessions, so gc hooks don't cover them.
- In addition to event-driven detection, the scout scans `podman logs` for
  error patterns using regex matching.
  Patterns are defined in `docs/orchestration.md` under `## Scout Rules`:
  - **Immediate** patterns (e.g., `FATAL|PANIC|panic:`) create a P0 bead
  - **Batched** patterns (e.g., `ERROR|Exception`) are collected over one poll
    cycle, then one bead per unique pattern
  - **Ignore** patterns suppress known noise
  - Defaults if the section doesn't exist: `FATAL|PANIC|panic:` immediate,
    `ERROR|Exception` batched
- Scout deduplicates — if a bead exists for the same error pattern, it appends
  rather than creating a new one
- Queue overflow protection: scout collapses related errors into a single bead
  across poll cycles, and stops creating new beads after a cap (configurable
  via `scout.maxBeads`, default: 10 open beads). Mayor is notified when
  the cap is reached.
- Workers run in isolated git worktrees with clean state per bead (see
  Session Lifecycle for details)
- **gc convergence owns the worker→judge loop end-to-end.** Worker
  executes the fix, judge is the gate, max iterations = 2. gc manages
  session lifecycle, handoff between worker and judge, and escalation.
  After 2 failed iterations, convergence escalates to the mayor.
  An event-gated order (`on: convergence.terminated`) triggers
  the post-gate logic (notify judge to merge, deploy bead) when
  convergence approves.
- Judge enforces `docs/style-guidelines.md` mechanically; flags anything
  outside documented rules for human review via `bd label add <id> human`
- After judge approval, deploy is gated by risk tier (see Deploy section)

### Mayor

The mayor is a persistent agent that serves as the human's conversational
interface to the city. The human interacts primarily via
`gc session attach mayor` (primary) or `gc mail send --to mayor` (async).

**Proactive briefing on attach:** When the human attaches, the mayor
immediately presents:
- Items pending human review (`bd human` queue) with context and
  suggested actions (approve, reject, investigate further)
- Changes that landed since last attach (merges, deploys)
- Escalations or failures (convergence failures, judge flags)
- Scout alerts (bead cap reached, container health issues)

**Spec decomposition:** On startup and on attach, the mayor checks for spec
changes via git diff against the last decomposition commit. If changes are
found, it proposes beads broken down into implementable units. By default,
the mayor waits for human approval before creating beads. Set
`mayor.autoDecompose = true` to auto-create without approval.

**Action execution:** The mayor executes approved actions on the human's
behalf — dismissing beads, approving deploys, filing investigations,
creating P0s. The human discusses and decides; the mayor runs the commands.

**Informal grouping:** When asked about a topic ("what's happening with
auth?"), the mayor queries beads and groups by common patterns — same
module, same error, same area — without requiring a formal convoy/grouping
data model.

**Concierge, not analyst:** The mayor aggregates signals from the scout,
judge, and convergence. It does not duplicate their analysis — it presents
their findings conversationally with suggested actions.

### Scout Housekeeping

In addition to error detection, the scout performs system housekeeping
during each patrol cycle:
- **Stale beads:** identifies beads with no recent activity
- **Orphaned workers:** detects containers with no matching in-progress bead
- **Worktree cleanup:** removes stale worktrees in `.wrapix/worktree/`

Housekeeping findings are reported via bead notes or flagged for human review
if action is needed.

### Ad-hoc Requests

Two paths for the human to inject work (directly or via the Mayor):

- **Investigation**: `gc mail send --to scout -s "investigate" -m "..."` —
  the scout picks this up on its next order cycle, investigates, and creates
  a bead if it finds something actionable. Lightweight, no bead created
  upfront, no entry into the worker→judge loop unless warranted. Via the
  Mayor: "can you have the scout look into X?"
- **Urgent fix**: `bd create --priority=0` — creates a P0 bead that bypasses
  cooldown and enters the full ops loop (worker→judge→merge→deploy).
  Via the Mayor: "this is urgent, create a P0 for X"

### Merge

The judge owns both review and merge as one continuous flow. After approving
a worker's changes, the judge merges immediately — no handoff to a separate
script. This naturally serializes merges since the judge processes its queue
one at a time.

- Linear history only: `git merge --ff-only <bead-id>`
- If fast-forward fails (main advanced): rebase the branch onto main, run
  `prek` (pre-commit stage), then fast-forward merge. If tests fail after
  rebase, reject back to a new worker with the failure details.
- If rebase has conflicts: reject back to a new worker with conflict details
  as context — no automatic conflict resolution
- Judge does not run tests — it reviews code quality against
  `docs/style-guidelines.md` only. `prek` runs tests during rebase.
- After merge: `rm -rf .wrapix/worktree/<bead-id>` + `git worktree prune`
  and `git branch -d <bead-id>`. `git worktree remove` cannot be used
  because the provider rewrites the worktree's `.git` file with a
  container-internal path. On rejection, old branch is also deleted — the
  new worker creates a fresh one.
- After a successful merge the judge pushes to the github mirror with
  `git push` from inside its container. Authentication uses
  `secrets.deployKey` — see Secrets. If no deploy key is configured the
  judge falls back to leaving the commit local; the human lands it from
  the host.

The post-gate order still fires on `convergence.terminated` events but is
lighter — it notifies the judge to merge (for approved convergences),
handles deploy bead creation, and sends notifications. The judge does the
actual git operations.

#### Judge Gate

The worker→judge handoff is managed by gc convergence with
`gate_mode=condition`. The gate condition script:

1. Reads `commit_range` from bead metadata (set by the provider script
   after worker commits via `bd update <bead-id> --set-metadata "commit_range=<range>"`)
2. Nudges the judge session with the commit range via `gc nudge judge`
3. Polls bead metadata for `review_verdict` (approve/reject), set by the
   judge via `bd update <bead-id> --set-metadata "review_verdict=approve"`
4. Returns exit 0 on approve, exit 1 on reject — gc convergence uses this
   to decide whether to iterate or terminate

On rejection, the judge provides specific feedback referencing documented
rules and line numbers. gc convergence starts a new worker iteration with
the judge's notes included in the task file. After 2 failed iterations,
convergence escalates to the mayor, who presents the situation to the human
conversationally.

The judge reads the bead from `.beads/`, diffs the commits, and reviews
against `docs/style-guidelines.md`. Bead ID and role type are injected via
the formula's `env` configuration, not `runtime.Config` directly.

### Notifications

Two notification paths:

**Mayor-mediated (primary):** The mayor aggregates signals from other roles
and presents them to the human conversationally on attach. Events flow to the
mayor, not directly to the human:
- `bd label add <id> human` flags (judge flagged something outside
  documented rules)
- Convergence escalation (max iterations reached — worker→judge loop
  failed twice)
- Deploy approval needed
- Spec decomposition proposals ready for review

**`wrapix-notify` (fallback):** For events that need attention when the human
is not attached to the mayor. The post-gate order and entrypoint wrapper call
`wrapix-notify` (the notification client). `wrapix-notifyd` is the daemon and
must not be called directly (it blocks).
- Periodic digest (hot spots, fix counts, rejection rates) — generated
  by a cooldown-gated digest order on a configurable interval
- gc (on the host) and role containers reach `wrapix-notify` via the
  notify socket — role containers get it mounted in by the provider
- Future: migrate to beads hooks when available, so standalone `ralph run`
  users also get notifications

### Deploy

- After judge approval and merge, the post-gate order creates a deploy bead
  summarizing the change and its risk classification
- Default: deploy beads are flagged for human approval via
  `bd label add <id> human`. The mayor presents deploy beads to the human
  on attach with suggested action. The human runs `nix build` on the host,
  then restarts the affected containers manually
  (`podman stop && podman rm && podman run`).
- Consumer can opt into auto-deploy for low-risk changes by defining an
  `## Auto-deploy` section in `docs/orchestration.md`. When the judge
  classifies a change as low-risk per these rules, the post-gate order
  skips the human label. A cooldown-gated deploy order polls for
  unflagged deploy beads and restarts containers automatically (still
  requires the human or CI to have pre-built the images).
- The city does not run Nix builds (gc and role containers alike).
  Rolling restarts and database migrations are out of scope for v1.

### Build-to-Ops Transition

- No automatic escalation from ops to build mode, ever
- The system surfaces patterns for the human to interpret (via the mayor):
  - Worker observations: "this is a patch, the real fix needs restructuring"
    (flagged via `bd label add <id> human`)
  - Judge observations: "third patch to this module this week"
    (flagged via `bd label add <id> human`)
  - Periodic digest: hot spots, fix counts, rejection rates (see
    Notifications section)
- Human decides when to initiate `ralph plan` for a spec-driven redesign
- During a build: creating a spec bead implicitly holds the affected area.
  The spec bead's description should name the modules/files under redesign.
  The judge checks for active spec beads and rejects worker fixes that
  touch held areas. When the spec bead closes, the hold lifts automatically.
- Build complete to ops: automatic — scout watches new code, no explicit handoff

### Context Hierarchy

| File | Shared | Pinned | Purpose |
|------|--------|--------|---------|
| `docs/README.md` | git | Always (baked into formulas) | Project overview, terminology |
| `docs/architecture.md` | git | On demand (referenced by formulas when needed) | System design |
| `docs/orchestration.md` | git | On demand (loaded by ops formulas at session start) | Ops config, deploy commands, role rules |
| `docs/style-guidelines.md` | git | On demand (loaded by judge formula at session start) | Code standards the judge enforces |
| `.wrapix/orchestration.md` | local | On demand (loaded by ops formulas at session start) | Dynamic/temporal overrides |

- `ralph sync` always scaffolds missing docs files for any project:
  `docs/README.md` (project overview), `docs/architecture.md` (system design),
  `docs/style-guidelines.md` (code standards). These are useful with or
  without Gas City.
- When `ralph sync` detects `mkCity` in the flake, it additionally scaffolds
  `docs/orchestration.md` from the built city config with placeholder sections
  for deploy commands, scout rules, and auto-deploy criteria.
- All scaffolded files are created as beads flagged for human review via
  `bd label add <id> human`
- The entrypoint wrapper prints an informational summary of pending reviews
  on startup (including scaffolding beads) but does not block. This is
  expected operation — the mayor presents these items to the human on attach.
- `.wrapix/orchestration.md` is tool-managed — updated by gc commands, not
  manually edited

### Anti-Slop

- Judge enforces `docs/style-guidelines.md` mechanically
- Changes outside documented rules are flagged for human (via mayor), not auto-decided
- Human decisions feed back into `docs/style-guidelines.md`, growing the rules
  organically
- Judge also sweeps `.wrapix/orchestration.md` for stale dynamic context
  (expired dated entries, undated entries older than 7 days)

### Nix API

Flake API — minimal:

```nix
wrapix.mkCity {
  services.api.package = myApp;
}
```

Flake API — full options:

```nix
wrapix.mkCity {
  # workspace defaults to flake root
  profile = wrapix.profiles.rust;

  services = {
    api = {
      package = myApp;
      # Service container options follow NixOS oci-containers schema
      # (ports, environment, volumes, cmd, etc.)
    };
    db = {
      package = pkgs.postgresql_16;
    };
  };

  # Agent configuration
  agent = "claude";           # default, only option for now

  # Scaling and pacing
  workers = 1;                # max concurrent workers (default: 1)
  cooldown = "2h";            # time between task dispatches (default: "0")
  scout.interval = "5m";      # polling interval (default: "5m")
  scout.maxBeads = 10;         # bead cap before scout pauses (default: 10)

  # Mayor configuration
  mayor.autoDecompose = false; # auto-create beads from spec changes (default: false)

  # Resource limits per role (optional, default: no limits)
  resources = {
    worker = { cpus = 2; memory = "4g"; };
    scout = { cpus = 1; memory = "2g"; };
    judge = { cpus = 1; memory = "2g"; };
    mayor = { cpus = 1; memory = "2g"; };
  };

  # Secrets — string = env var name, absolute path = file
  secrets.claude = "ANTHROPIC_API_KEY";                     # reads host env var
  secrets.deployKey = config.sops.secrets.deploy-key.path;  # reads file (absolute path)
}
```

NixOS module (the module receives `wrapix` via the flake's NixOS module imports):

```nix
services.wrapix.cities.myapp = {
  workspace = "/var/lib/myapp";    # required on NixOS (no flake root)
  profile = "rust";                # string shorthand resolved by the module
  services = {
    api.package = myApp;
    db.package = pkgs.postgresql_16;
  };
  secrets.claude = config.sops.secrets.claude-api-key.path;
};
```

### Provider Interface

Shell script implementing Gas City's `exec:<script>` provider pattern.
The script receives a command and arguments, translates to podman operations.
Both host-side and container-side gc call the provider — container-side gc
sets `GC_SESSION=exec:<script>` so gc resolves to the exec provider, and
shared tmux sockets allow the provider's tmux methods to work cross-container
without podman (see Shared Tmux Sockets).

**Container-side gc command routing:**

| gc command | Container path | Mechanism |
|------------|---------------|-----------|
| `gc status` | Controller query API | Unix socket (read-only) |
| `gc session list` | Controller query API | Unix socket (read-only) |
| `gc session nudge` | Exec provider | Shared tmux socket (direct) |
| `gc session peek` | Exec provider | Shared tmux socket (direct) |
| `gc session send-keys` | Exec provider | Shared tmux socket (direct) |
| `gc agent start/stop` | Mail to controller | Intent-based, controller executes with provider |
| `gc mail send/check` | Direct (beads-based) | Already works, no provider dependency |
| `gc sling` | Direct (beads-based) | Already works, no provider dependency |
| `bd *` | Direct (beads-based) | Already works, no provider dependency |

**Persistent roles (mayor, scout, judge)** — full tmux-based interaction:

| gc method | Provider action |
|-----------|----------------|
| `Start` | `podman run -d` with tmux as PID 1, socket at `.wrapix/tmux/<role>.sock` |
| `Stop` | Remove shared socket + `podman stop && podman rm` |
| `Interrupt` | `tmux -S <socket> send-keys C-c` (falls back to `podman exec` if no socket) |
| `IsRunning` | Check shared socket liveness, fall back to `podman inspect` |
| `Attach` | `tmux -S <socket> attach` (falls back to `podman exec -it tmux attach`) |
| `Peek` | `tmux -S <socket> capture-pane` |
| `SendKeys` | `tmux -S <socket> send-keys` |
| `Nudge` | Wait for idle + `tmux -S <socket> send-keys` |
| `GetLastActivity` | `tmux -S <socket> display -p '#{pane_last_activity}'` |
| `ClearScrollback` | `tmux -S <socket> clear-history` |
| `IsAttached` | Return false (not tracked in v1) |
| `RunLive` | No-op (unsupported by exec provider — returns nil without calling script) |

**Ephemeral workers** — no tmux, container exit signals completion:

| gc method | Provider action |
|-----------|----------------|
| `Start` | `podman run -d` with task command as entrypoint |
| `Stop` | `podman stop && podman rm` |
| `IsRunning` | `podman inspect --format '{{.State.Running}}'` |
| `Peek` | `podman logs --tail` |
| `Interrupt` / `SendKeys` / `Nudge` | No-op (worker runs to completion or is stopped) |
| `IsAttached` / `Attach` / `GetLastActivity` / `ClearScrollback` / `RunLive` | No-op |

**Shared across both modes:**

| gc method | Provider action |
|-----------|----------------|
| `ListRunning` | `podman ps --filter label=gc-city=<name>` |
| `SetMeta/GetMeta/RemoveMeta` | Container-internal files at `/tmp/gc-meta/<key>` via `podman exec` (gc session metadata — separate from bead metadata managed by `bd update --set-metadata`) |
| `CopyTo` | `podman cp` |
| `ProcessAlive` | Persistent: `podman exec pgrep`. Ephemeral: delegates to `IsRunning` |
| `CheckImage` | `podman image exists <image>` |
| `Capabilities` | Returns empty (exec provider hardcodes all false) |

Container labeling convention:
- `gc-city=<city-name>`
- `gc-role=mayor|scout|worker|judge`
- `gc-bead=<bead-id>` (workers only)

### Shared Tmux Sockets

Persistent role containers (mayor, scout, judge) place their tmux server
sockets on the shared `.wrapix/` mount at `.wrapix/tmux/<role>.sock`.
This enables cross-container communication without podman:

- **Why**: `podman exec` is not available inside containers. Without
  shared sockets, container-side `gc session nudge` cannot reach other
  roles' tmux sessions.
- **How**: `persistent_start` launches tmux with
  `tmux -S /workspace/.wrapix/tmux/${GC_AGENT}.sock`. The `persistent_exec`
  helper checks for the socket first (`tmux -S <socket> ...`), falling
  back to `podman exec` for pre-migration containers.
- **gc resolution**: Persistent containers set
  `GC_SESSION=exec:/workspace/.gc/scripts/provider.sh` so gc uses the
  exec provider (which calls `provider.sh`) rather than its built-in tmux
  provider. Without this, gc's internal tmux state cache tries
  `tmux list-panes -a` on a non-existent default socket and concludes
  sessions are not running.
- **Workers**: Keep `GC_SESSION=worker` (gc's built-in tmux provider).
  Workers run their own local tmux for process lifecycle
  (`tmux wait-for worker-exit`) and don't participate in cross-container
  communication.
- **Permissions**: All containers use `--userns=keep-id`, so socket
  file permissions work across containers sharing the `.wrapix/` mount.
- **Cleanup**: `recovery.sh` removes stale sockets for roles whose
  containers are no longer running. The `stop` method removes the socket
  before stopping the container.

### Session Lifecycle

**Persistent roles (mayor, scout, judge):**
- Started with the city, stopped with the city
- `podman run -d` with tmux server as PID 1, socket on shared
  `.wrapix/tmux/<role>.sock` mount
- Both host-side and container-side gc interact via shared tmux sockets
  (`tmux -S .wrapix/tmux/<role>.sock`). The provider falls back to
  `podman exec` if no socket exists (pre-migration containers).
- Container-side agents use `GC_SESSION=exec:<script>` so gc resolves
  the exec provider and calls `provider.sh nudge/peek/send-keys` which
  talks to the target's tmux socket directly
- Human attaches to the mayor via `gc session attach mayor`

**Ephemeral workers:**
- One container per bead, clean state every time
- Workers discover beads via gc's pull model: `bd ready --metadata-field
  gc.routed_to=worker --unassigned` (gc routes beads via its
  `EffectiveSlingQuery` which sets `gc.routed_to=<agent_template>`)
- The provider script's `Start` handler creates the git worktree:
  `git worktree add .wrapix/worktree/<bead-id> -b <bead-id>`
- The provider rewrites the worktree's `.git` file to
  `gitdir: /mnt/git/worktrees/<bead-id>` and mounts the main `.git`
  at `/mnt/git:rw` so git operations work inside the container
- Worker container mounts the worktree as its workspace (`/workspace:rw`),
  `.beads/` as read-only, and receives a `.task` file built from the bead
  description, acceptance criteria, and any judge notes from prior attempts
- Worker commits to the branch, then exits. A background monitor sets
  `commit_range` and `branch_name` on the bead metadata after exit.
- gc convergence detects worker completion and hands off to the judge gate
- After convergence completes (approved or escalated), the judge handles
  merge and worktree cleanup:
  `rm -rf .wrapix/worktree/<bead-id>` + `git worktree prune`

**Crash recovery:**
- gc runs on the host as a systemd service with `Restart=always`; agent
  role containers are spawned by the provider as siblings
- On restart: scan `podman ps --filter label=gc-city=<name>` for running containers
- Reconcile against beads state (desired vs actual)
- Orphaned workers (no matching in-progress bead): stop and remove
- Workers that finished (commits on branch, bead still open): re-enter convergence
- Stale worktrees in `.wrapix/worktree/`: clean up orphans
- Stale tmux sockets in `.wrapix/tmux/*.sock`: remove sockets for roles
  whose containers are no longer running

### Beads Sync

Two modes depending on execution path:

| Mode | Beads access | Sync mechanism |
|------|-------------|----------------|
| `ralph run` (standalone) | Container has bd + dolt, does its own pull/push | Existing behavior, unchanged |
| `gc start` (orchestrated) | Host-side gc, mayor, scout, and judge have bd pointed at the shared `beads-dolt` container | Ephemeral workers have no bd/dolt access |

In gc mode, gc itself (on the host: provider script, gate script, post-gate
order), mayor, scout, and judge all have bd access — all of them talk to the
same shared `beads-dolt` container attached to the city network. Only
ephemeral workers are isolated — they receive their task
via environment variables and a mounted task file, with no direct bd access.

The provider script's `Start` handler passes the bead ID, task
description, and relevant context to the worker via environment variables and
a mounted task file. The task file contains the bead description, acceptance
criteria, and any judge notes from prior attempts. The worker reads these,
executes the task using `wrapix-agent`, commits results to its worktree
branch, and exits. A background monitor in the provider script sets
`commit_range` and `branch_name` on the bead metadata via
`bd update --set-metadata` after the worker exits, so the gate condition
script can read this context when bridging to the judge.
The post-gate order reads bead state and updates it based on the container
exit code and branch contents. No dolt sync between containers.

### Ralph Integration

Ralph stays as the standalone workflow tool. Gas City is additive:

| Phase | Ralph (standalone) | Gas City (orchestrated) |
|-------|-------------------|------------------------|
| Spec authoring | `ralph plan` | `ralph plan` (unchanged) |
| Work decomposition | `ralph todo` | Mayor decomposes specs into beads |
| Execution | `ralph run` (single agent, one container) | `gc start` (multi-agent, parallel workers) |

In city mode, the mayor handles spec decomposition — the same job as
`ralph todo` but as a formula step. On startup and on human attach, the
mayor checks for spec changes (git diff against last decomposition commit).
If changes are found, it proposes beads and waits for human approval before
creating them (default). Set `mayor.autoDecompose = true` to skip approval.

`ralph todo` still works standalone for `ralph run` users — beads are the
interface regardless of which tool creates them.

### Agent Abstraction

The agent tool (Claude, Codex, Gemini) is a configuration option, not baked into
the container image:

```nix
wrapix.mkCity {
  agent = "claude";    # default, only option for now
}
```

The provider script calls a `wrapix-agent` wrapper that translates to the
configured agent's CLI. For claude, this invokes `claude` in both modes —
ephemeral workers receive their task via a mounted prompt file and `docs/`
context, persistent roles run as interactive sessions. The wrapper handles
prompt construction and output capture. One place to swap, all roles benefit.
Future agent providers require only a new entry in the
agent registry and the corresponding package in the Nix closure.

Role behavior is defined as gc formulas. `mkCity` generates default formulas
for mayor, scout, worker, and judge roles. Consumers can override formulas to
customize role behavior without modifying the provider script or container
images.

### Secrets

- Secrets are never baked into images — always injected at runtime
- `secrets.claude` is required — city fails to start if not set
- String starting with `/` = file path (works with sops-nix, agenix, or plain files)
- Any other string = host environment variable name
- gc runs on the host and inherits secrets from the systemd unit
  environment; the provider script passes them to agent containers at start
- Two secret names are **well-known**: `deployKey` and `signingKey`. When
  set as file paths, they are mounted at `/run/secrets/<name>:ro` and
  additionally exported into role containers as `WRAPIX_DEPLOY_KEY` and
  `WRAPIX_SIGNING_KEY`. The shared `git-ssh-setup.sh` fragment sourced by
  the persistent-role startup translates them into `GIT_SSH_COMMAND` and
  commit-signing config — so the judge can `git push` to the github
  mirror after merge without host SSH forwarding (see Merge)

```nix
# Option A: env var (no leading /)
secrets.claude = "ANTHROPIC_API_KEY";

# Option B: file path (works with sops-nix, agenix, etc.)
secrets.claude = config.sops.secrets.claude-api-key.path;  # resolves to /run/secrets/...

# Additional secrets (optional)
secrets.deployKey = config.sops.secrets.deploy-key.path;
secrets.signingKey = config.sops.secrets.signing-key.path;
```

### Resource Limits and Pacing

**Compute:** Podman-native resource limits per role:

```nix
resources = {
  worker = { cpus = 2; memory = "4g"; };
  scout = { cpus = 1; memory = "2g"; };
  judge = { cpus = 1; memory = "2g"; };
  mayor = { cpus = 1; memory = "2g"; };
};
```

Default: no limits.

**Pacing:** Two controls plus automatic backpressure:

- `workers` — max concurrent workers (default: 1)
- `cooldown` — time between task dispatches (default: `"0"`)
- P0 beads bypass cooldown — dispatched immediately regardless of pacing.
  The human can create P0 beads directly (`bd create --priority=0`) or
  ask the mayor to inject urgent work into the ops loop.
- Reactive backpressure (automatic): when any agent hits a rate limit,
  gc pauses dispatching until the window resets

```nix
workers = 1;
cooldown = "2h";     # supports "30m", "1h", "2h30m", etc.
```

### Platform Support

| Platform | `ralph plan/todo/run` | `mkCity` (ops) |
|----------|----------------------|----------------|
| Linux | Yes | Yes (production) |
| macOS | Yes (existing wrapix support) | Not supported |

### Testing

Layered testing via `nix flake check` and pre-commit hooks:

| Layer | What | Hook | Automated |
|-------|------|------|-----------|
| 1. Nix evaluation | `mkCity` evaluates, `city.toml` valid, TOML generation | `nix-flake-check` (pre-push) | On push |
| 2. Unit tests | Shell syntax, gate exit codes, provider commands, scout parsing, config validation, formula step commands | `nix-flake-check` (pre-push) | On push |
| 3. Integration | Full ops loop: gc → provider.sh → podman → container → mock claude | `city-integration` (pre-push) | On push (requires podman, skips gracefully if missing) |

Unit tests live in `tests/city/unit.nix` and run inside the Nix sandbox (no
podman needed). The integration test lives in `tests/city/integration.nix`
and exercises the real stack end-to-end:

- Phase 1 (happy path): gc starts mayor + scout + judge via podman, scout
  creates bead, worker commits in worktree, judge approves and merges,
  post-gate creates deploy bead
- Phase 2 (merge conflict): conflicting change on main, post-gate detects
  rebase conflict, rejects back to worker with failure context
- Phase 3 (escalation): convergence ends with non-approved reason, post-gate
  cleans up worktree and branch

```yaml
# .pre-commit-config.yaml
- id: nix-flake-check
  stages: [pre-push]
  entry: nix flake check

- id: city-integration
  stages: [pre-push]
  entry: nix run .#test-city
  files: ^(lib/city/scripts/|tests/city|specs/gas-city)
```

### Upgrades

- Human rebuilds the flake on the host (`nix build`), then restarts the
  city (`gc stop && gc start --foreground`)
- gc does not detect or apply upgrades automatically
- In-flight workers are stopped; their beads remain open and are picked up
  after restart
- Graceful drain (wait for active workers) is out of scope for v1

### Migration

No breaking changes. Gas City is additive:

1. **No change** — existing `mkSandbox` + `ralph` users are unaffected
2. **Try gc** — add `mkCity` alongside existing setup, use `gc start` instead
   of `ralph run`
3. **Full ops** — define services in `mkCity`, get the autonomous ops loop

### CLI Surface

No new commands. Existing tools compose:

| Tool | Domain | Used for |
|------|--------|----------|
| `gc` | Orchestration | `gc start --foreground`, `gc stop`, `gc status`, `gc session attach mayor` |
| `bd` | Work tracking | `bd ready`, `bd human`, `bd close` (direct bypass) |
| `ralph` | Spec workflow + setup | `ralph plan`, `ralph todo` (standalone), `ralph run` (fallback), `ralph sync` (city setup) |

**Container-side gc availability:**

Persistent role containers set `GC_SESSION=exec:<provider>` so gc uses
the exec provider directly. Shared tmux sockets enable cross-container
nudge/peek/send-keys without podman. On the host (no `GC_SESSION`), gc
discovers the provider from `city.toml`.

| Command | Container support | Mechanism |
|---------|-------------------|-----------|
| `gc status` | Yes | Controller query API |
| `gc session list` | Yes | Controller query API |
| `gc session nudge` | Yes | Exec provider → shared tmux socket |
| `gc session peek` | Yes | Exec provider → shared tmux socket |
| `gc mail send/check` | Yes | Direct (beads) |
| `gc sling` | Yes | Direct (beads) |
| `gc session attach` | No | Host only (requires TTY) |
| `gc restart` | Yes | Mail to controller |

## Affected Files/Modules

| Area | Files | Change |
|------|-------|--------|
| Nix API | `lib/city/default.nix` | `mkCity` function |
| Shared dolt | `lib/beads/default.nix` | Per-workspace `beads-dolt` container + CLI, shared between host `bd` and the city |
| Provider | `lib/city/scripts/provider.sh` | Shell script for `exec:<script>` |
| NixOS module | `modules/city.nix` | `services.wrapix.cities` |
| Agent wrapper | `lib/city/scripts/agent.sh` | `wrapix-agent` CLI abstraction |
| Formulas | `lib/city/formulas/` | Default mayor, scout, worker, judge formulas |
| Post-gate order | `lib/city/scripts/post-gate.sh` | Event-gated order: notify judge to merge, deploy bead creation |
| Orders | `lib/city/orders/post-gate/order.toml` | gc order definition for post-gate event trigger |
| Gate condition | `lib/city/scripts/gate.sh` | Convergence gate: nudge judge, wait for verdict |
| Recovery | `lib/city/scripts/recovery.sh` | Crash recovery: reconcile containers vs bead state |
| Dispatch | `lib/city/scripts/dispatch.sh` | Cooldown-aware worker scale_check for gc |
| gc home staging | `lib/city/scripts/stage-home.sh` | Isolate gc from host `.beads/` |
| Entrypoint | `lib/city/scripts/entrypoint.sh` | `beads-dolt` start+attach, pending reviews, recovery, events watcher, gc home staging, gc start with trap cleanup |
| Unit tests | `tests/city/unit.nix` | Nix-sandbox tests for all components |
| Integration tests | `tests/city/integration.nix` | Full ops loop via podman |
| Flake | `flake.nix` | Add gc dependency, expose mkCity |
| Sandbox | `lib/sandbox/default.nix` | No changes (mkCity uses mkSandbox) |
| Ralph | `lib/ralph/cmd/sync.sh` | Extend `ralph sync` to detect mkCity and scaffold |
| Docs convention | `docs/` | Established by scaffolding on first run |

## Success Criteria

- [x] `mkCity` evaluates with minimal config (`services.api.package = myApp`)
  [verify](tests/city/unit.nix::city-mkcity-eval)
- [x] Generated `city.toml` is valid and references the wrapix provider script
  [verify](tests/city/unit.nix::city-city-toml), [verify](tests/city/unit.nix::city-config-validate)
- [x] Provider script handles all gc provider methods
  [verify](tests/city/unit.nix::city-shell-syntax), [verify](tests/city/unit.nix::city-provider-worker)
- [x] Ephemeral workers use git worktrees at `.wrapix/worktree/<bead-id>`
  [verify](tests/city/integration.nix::Wait for worker worktree)
- [x] Persistent roles (mayor, scout, judge) start with tmux as PID 1
  [verify](tests/city/integration.nix::Verify tmux session alive in mayor container)
- [x] gc convergence detects worker completion and triggers judge gate
  [verify](tests/city/integration.nix::Wait for judge approval (via monitor gate pipeline))
- [x] Secrets are injected at runtime, never baked into images
  [verify](tests/city/unit.nix::city-secrets)
- [ ] `ralph run` still works standalone without gc
- [x] NixOS module generates systemd units and podman network
  [verify](tests/city/unit.nix::city-nixos-module)
- [ ] Crash recovery: gc systemd service restarts, reconciles orphaned containers
- [ ] `ralph sync` scaffolds missing docs files and creates review beads
- [x] Service packages are built into OCI images via `dockerTools.streamLayeredImage`
  [verify](tests/city/unit.nix::city-service-images)
- [ ] Cooldown pacing delays task dispatch by configured duration
- [ ] Judge enforces `docs/style-guidelines.md` rules
  [judge]
- [x] Provider script is clean, minimal shell with no Go dependencies
  [judge]
- [x] Agent abstraction allows future provider swaps without architectural changes
  [judge]
- [ ] P0 beads bypass cooldown and are dispatched immediately
- [x] Merge uses fast-forward only; rebase + prek on divergence
  [verify](tests/city/integration.nix::Verify merge landed on main), [verify](tests/city/integration.nix::Judge detects merge conflict and rejects)
- [x] Post-gate order sends notifications via `wrapix-notify`
  [verify](tests/city/integration.nix::Wait for post-gate pipeline (deploy bead created))
- [ ] Scout pauses bead creation when queue cap is reached
- [x] Scout detects errors via log pattern regex matching
  [verify](tests/city/unit.nix::city-scout-parse-rules), [verify](tests/city/unit.nix::city-scout-scan)
- [x] Judge gate reads commit range from bead metadata
  [verify](tests/city/integration.nix::Verify gate can find metadata after recovery)
- [ ] Scout polling uses gc orders for scheduling
- [ ] Worker→judge retry uses gc convergence with max 2 iterations
- [x] Role behavior defined as gc formulas, overridable by consumers
  [verify](tests/city/unit.nix::city-formulas)
- [x] Post-gate handles escalation path (non-approved convergence)
  [verify](tests/city/integration.nix::Post-gate handles escalation (non-approved))
- [x] Custom scale_check avoids gc's 30s timeout under dolt contention
  [verify](tests/city/unit.nix::city-config-validate)
- [ ] Mayor formula starts as persistent session and responds on attach
- [ ] Mayor presents proactive briefing (pending reviews, recent merges, escalations)
- [ ] Mayor decomposes spec changes into proposed beads on startup/attach
- [ ] Mayor executes approved actions on human's behalf (dismiss, approve, file)
- [ ] Mayor `autoDecompose` config option skips human approval
- [ ] Judge owns merge after approval (review + merge as one flow)
  [verify](tests/city/integration.nix::Verify merge landed on main)
- [ ] Judge rejection includes specific rule references and line numbers
- [ ] Escalation flows to mayor instead of raw notification
- [ ] Scout performs housekeeping (stale beads, orphaned workers, worktree cleanup)
- [ ] Entrypoint prints informational pending-review status without blocking
- [ ] Formula step commands are individually testable against known state
  [verify](tests/city/unit.nix::city-formula-steps)

## Out of Scope

- Web UI / dashboard — the human interacts via mayor and CLI (`gc`, `bd`, `ralph`)
- Voice interaction with mayor — future work
- Multi-machine cities — one city per host; architecture supports future
  multi-host via podman remote but not implemented in v1
- Non-Nix consumers — `mkCity` requires Nix
- Custom agent providers — claude only at launch; Codex/Gemini are future work
- The Wasteland — federated cities / trust networks
- macOS production cities — dev only, Linux for production
- Formal convoy/grouping data model — mayor handles informal grouping via queries
- Token tracking / budget enforcement — rely on backpressure + cooldown
- Service builds from source — services defined by Nix package
- Gas City upstream contributions — provider is `exec:` script, not a Go PR.
  Container-side gc works via `GC_SESSION=exec:<provider>` and shared tmux
  sockets — no upstream changes needed
