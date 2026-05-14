# Architecture

Wrapix is a secure sandbox for running [Claude Code](https://claude.ai/code) in isolated containers. It provides container isolation on Linux (Podman) and macOS (Apple container CLI), with tooling for notifications, remote builds, AI-driven workflows (Ralph), and multi-agent orchestration (Gas City).

## Design Principles

1. **Container isolation is the security boundary** — Filesystem and process isolation protect the host
2. **Least privilege** — Containers run without elevated capabilities
3. **User namespace mapping** — Files created in `/workspace` have correct host ownership
4. **Open network** — Full internet access for web research, git, package managers
5. **Nix all the way down** — Config, images, and orchestration are deterministic Nix outputs

## Platform Support

| Platform | Container Technology | Networking |
|----------|---------------------|------------|
| Linux | Podman rootless | pasta (userspace TCP/IP) |
| macOS | Apple container CLI + Virtualization.framework | vmnet bridge |

Both platforms provide full network connectivity without elevated privileges. The `/workspace` directory is mounted read-write with correct file ownership.

## Source Layout

```
lib/
├── default.nix          # Top-level API: mkSandbox, mkCity, mkRalph, profiles
├── sandbox/             # Container isolation
│   ├── default.nix      # Platform dispatcher, MCP integration, mkProfileImages
│   ├── profiles.nix     # Built-in profiles (base, rust, python)
│   ├── image.nix        # OCI image builder
│   ├── manifest.nix     # Profile→image JSON manifest (LOOM_PROFILES_MANIFEST)
│   ├── linux/           # Podman implementation + krun microVM support
│   └── darwin/          # Apple container implementation
├── beads/               # Per-workspace beads-dolt container management
├── city/                # Gas City orchestration
│   ├── default.nix      # mkCity — generates city.toml, provider, images
│   ├── provider.sh      # exec:<script> provider — gc commands → podman ops
│   ├── agent.sh         # wrapix-agent wrapper (claude abstraction)
│   ├── scout.sh         # Scout helpers: parse-rules, scan
│   ├── gate.sh          # Convergence gate: nudge Judge, poll verdict
│   ├── scripts/         # Shell scripts (provider, entrypoint, gate, etc.)
│   │   ├── provider.sh      # exec:provider — podman lifecycle (start/stop/nudge)
│   │   ├── entrypoint.sh    # beads-dolt, recovery, events watcher, gc start
│   │   ├── gate.sh          # Convergence gate: nudge judge, wait for verdict
│   │   ├── post-gate.sh     # Post-convergence: merge, cleanup, deploy bead
│   │   ├── dispatch.sh      # Cooldown-aware Worker scale_check for gc
│   │   ├── stage-home.sh    # Isolate gc from host .beads/
│   │   ├── prime-hook.sh    # SessionStart/PreCompact role prompt loader
│   │   ├── recovery.sh      # Crash recovery: reconcile orphaned containers
│   │   └── ...              # worker-setup, worker-collect, judge-merge, etc.
│   ├── prompts/         # Per-role system prompts (mayor, scout, judge, worker)
│   ├── formulas/        # Default role formulas (mayor, scout, worker, judge)
│   └── orders/          # gc orders (post-gate)
├── mcp/                 # MCP server registry
│   ├── default.nix      # Server registry: { tmux, playwright }
│   ├── tmux/            # tmux MCP server
│   └── playwright/      # Playwright MCP server
├── ralph/               # Single-agent workflow orchestration
├── loom/                # Rust loom orchestrator package wrapper
├── pi-mono/             # Pi agent runtime layer (Node.js + pi binary)
├── prek/                # Pre-commit hook shims (flock-wrapped)
├── builder/             # Linux builder for macOS
├── notify/              # Desktop notifications
└── util/                # Shared utilities

loom/                    # Rust workspace: loom orchestrator (crates/loom*, profile manifest, spawn)

modules/
└── city.nix             # NixOS module: services.wrapix.cities.<name>

docs/
├── README.md            # Project overview, terminology (always pinned)
├── architecture.md      # This file (on demand)
├── orchestration.md     # Ops config, Scout rules, deploy commands (on demand)
└── style-rules.md  # Code standards the Judge enforces (on demand)
```

## Component Overview

| Component | Purpose | Entry Point |
|-----------|---------|-------------|
| Sandbox | Container creation and lifecycle (`wrapix run`/`spawn`) | `mkSandbox` |
| Gas City | Multi-agent orchestration | `mkCity` |
| Profiles | Pre-configured dev environments | `profiles.{base,rust,python}` |
| MCP Servers | Optional capabilities (tmux, playwright) | `mcp.tmux`, `mcp.playwright` |
| Image Builder | OCI image generation via Nix | `lib/sandbox/image.nix` |
| Notifications | Desktop alerts when Claude waits | `wrapix-notify`, `wrapix-notifyd` |
| Linux Builder | Remote Nix builds on macOS | `wrapix-builder` |
| Ralph | Spec-driven single-agent workflow | `ralph {start,plan,ready,step,loop}` |
| Loom | Per-bead workflow runner; dispatches profiles via manifest | `loom {plan,run}` |

## Sandbox Launcher

The launcher and the OCI image are separate Nix outputs, composed at the consumer's discretion:

| Output | Role |
|--------|------|
| `packages.wrapix` | Profile-agnostic launcher binary; reads image ref/source at runtime |
| `packages.image-<profile>` | Per-profile OCI artifact (claude + pi runtimes both installed) |
| `packages.sandbox-<profile>[-pi]` | `makeWrapper` of the launcher with image ref/source + `WRAPIX_AGENT` baked in — the user-facing `nix run .#sandbox-rust` target |
| `packages.profile-images` | JSON manifest mapping profile → `{ref, source}` consumed by Loom (`LOOM_PROFILES_MANIFEST`) |

The launcher exposes two subcommands sharing the same container construction (mounts, env passthrough, deploy key):

- `wrapix run [DIR] [CMD…]` — interactive (TTY); reads `WRAPIX_DEFAULT_IMAGE_REF`/`WRAPIX_DEFAULT_IMAGE_SOURCE` from the env. The `sandbox-<profile>` wrappers set both; Loom's `plan` phase exports them programmatically from the profile-image manifest.
- `wrapix spawn --spawn-config <file> [--stdio]` — programmatic dispatch (Loom). Reads image ref/source plus workspace, env allowlist, and agent args from a JSON `SpawnConfig`. The agent runtime (`claude` vs `pi`) is selected at container start via `WRAPIX_AGENT`, not baked per-image.

See [specs/sandbox.md](../specs/sandbox.md) and [specs/profiles.md](../specs/profiles.md) for the full launcher and manifest contracts.

## Security Model

**Protected**: Filesystem (only `/workspace` accessible), processes (isolated), user namespace (correct UID), capabilities (none elevated)

**Not protected**: Network traffic is unrestricted by design for autonomous work.

### MicroVM Boundary (Linux)

On Linux with KVM, containers can optionally run inside a [libkrun](https://github.com/containers/libkrun) microVM (`podman --runtime krun`) for hardware-level isolation. Set `WRAPIX_MICROVM=1` to opt in.

## Gas City

Gas City adds multi-agent orchestration on top of the sandbox. It runs four roles in an autonomous ops loop:

```
Scout (watching) → creates bead → Worker (fixes) → Judge (reviews)
                                                         |
                                               merge or reject → retry
                                                         |
                                                  escalation → Mayor
```

| Role | Job | Lifetime |
|------|-----|----------|
| Mayor | Human's conversational interface, triage, approved actions | Persistent |
| Scout | Watches services, detects errors, creates beads, housekeeping | Persistent |
| Worker | Picks up a bead, writes the fix in a git worktree | Ephemeral |
| Judge | Reviews diffs against `docs/style-rules.md`, owns merge | Persistent |

**Key design decisions:**

- gc runs on the host as a per-city systemd service; agent role containers are spawned as siblings by the provider script via the local podman socket
- Workers get isolated git worktrees at `.wrapix/worktree/<bead-id>`
- The provider script (`lib/city/scripts/provider.sh`) translates gc commands to podman operations
- Convergence manages the Worker→Judge loop (max 2 iterations before escalation to the Mayor)
- Merge is fast-forward only; rebase + prek on divergence
- `ralph sync` scaffolds the docs/ context hierarchy; scaffolded files are flagged for human review via `bd label add <id> human` and presented by the Mayor on attach

### gc Primitives

- **Convergence** — bounded Worker-Judge retry loop with a gate condition script; terminates on approve or max iterations (default 2), then escalates to the Mayor.
- **Orders** — event- or time-triggered workflow dispatchers defined in TOML under `lib/city/orders/`; fire formulas on conditions like `convergence.terminated`.
- **Formulas** — role-behavior definitions under `lib/city/formulas/`; each role's formula is a sequence of steps executed per session iteration.
- **Sling** — gc's routing primitive; sets `gc.routed_to=<role>` metadata on a bead so the target role's scale_check picks it up.

### Context Hierarchy

| File | Pinned | Purpose |
|------|--------|---------|
| `docs/README.md` | Always | Project overview, terminology |
| `docs/architecture.md` | On demand | System design |
| `docs/orchestration.md` | On demand | Ops config, Scout rules, deploy commands |
| `docs/style-rules.md` | On demand | Code standards the Judge enforces |
| `.wrapix/orchestration.md` | On demand | Dynamic/temporal overrides (local, tool-managed) |

## MCP Integration

MCP servers extend sandbox capabilities. The `mcp` parameter in `mkSandbox` accepts a set of server names:

```nix
mkSandbox {
  profile = profiles.rust;
  mcp.tmux = { };
  mcp.playwright = { };
}
```

See [tmux-mcp.md](../specs/tmux-mcp.md) and [playwright-mcp.md](../specs/playwright-mcp.md) for details.
