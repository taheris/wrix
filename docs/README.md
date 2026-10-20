# Project Overview

Wrix provides sandboxed containers for AI-driven development. See
`docs/architecture.md` for system design.

## Consumer Workflow

Provide one host `wrix` through Home Manager, nix-darwin, explicit
`nix profile add` or a temporary Nix environment; see
[Host CLI Setup](../specs/cli.md#host-cli-and-project-configuration). Projects
use `sandbox.devShell { }` for the
[automatic data handoff](../specs/profiles.md#project-launch-handoff), not a
second executable. Pure images/configuration remain available for programmatic
host `spawn`; configured sandbox runners are not compatibility entry points.

[Claude stdio clients](claude-stdio.md) describes the explicit
permission-protocol opt-in through `SpawnConfig.agent_args` and its
compatibility/security limits.

## Authoring Conventions

- [`docs/spec-conventions.md`](spec-conventions.md) — what a spec is and isn't,
  trust tiers, standard section structure.
- [`docs/style-rules.md`](style-rules.md) — code-style and test-quality rules
  organized by rule family (SH-, NX-, DOC-, GIT-, TST-, RS-, COM-, CLI-).

[Private Nix](private-nix.md) documents the intrinsic sandbox contract,
capability discovery, Loom integration and fresh-image verification boundaries.

[Verifier input discovery and measurements](verifier-inputs.md) describes
checked resource projections, conservative scope, and measured worker costs.

## Specs

Individual spec files live in `specs/`. This table is the session-start pin —
keep it current when specs land or retire.

| Spec                                         | Code                                                                                 | Beads    | Purpose                                                                  |
| -------------------------------------------- | ------------------------------------------------------------------------------------ | -------- | ------------------------------------------------------------------------ |
| [beads](../specs/beads.md)                   | [`.beads/`](../.beads/)                                                              | wx-v7m8n | Issue tracking with dependency support                                   |
| [cache](../specs/cache.md)                   | `crates/wrix-cache/`, Celler                                                         | —        | Host-published, sandbox-consumed Celler cache and age-based eviction     |
| [cli](../specs/cli.md)                       | [`crates/wrix-cli/`](../crates/wrix-cli/), `.#verify`                                | —        | Wrix command surface, repository initialization, and scoped verification |
| [image-builder](../specs/image-builder.md)   | [`lib/sandbox/image.nix`](../lib/sandbox/image.nix)                                  | wx-nf6eu | Nix-based OCI image source creation                                      |
| [linux-builder](../specs/linux-builder.md)   | [`lib/builder/default.nix`](../lib/builder/default.nix)                              | wx-ope   | Remote Nix builds for macOS                                              |
| [store](../specs/store.md)                   | `crates/wrix-service/`, `crates/wrix-sandbox/`, `lib/sandbox/`                       | wx-doaia | Approved service-owned installed Nix stores and read-only worker reuse   |
| [notifications](../specs/notifications.md)   | [`lib/notify/`](../lib/notify/)                                                      | wx-q6x   | Agent-neutral desktop attention notifications with focus suppression     |
| [playwright-mcp](../specs/playwright-mcp.md) | [`lib/mcp/playwright/`](../lib/mcp/playwright/)                                      | wx-9mvh  | Browser automation for frontend development                              |
| [pre-commit](../specs/pre-commit.md)         | [`.pre-commit-config.yaml`](../.pre-commit-config.yaml)                              | wx-t6rh  | Git hooks for treefmt, shellcheck, and integration tests                 |
| [profiles](../specs/profiles.md)             | [`lib/sandbox/profiles.nix`](../lib/sandbox/profiles.nix)                            | wx-1thzk | Host/sandbox environments and managed tool caches                        |
| [proxy](../specs/proxy.md)                   | `crates/wrix-service/`, `lib/services/`                                              | —        | Rama-backed HTTP/CONNECT admission for restricted egress                 |
| [sandbox](../specs/sandbox.md)               | [`lib/sandbox/default.nix`](../lib/sandbox/default.nix)                              | wx-fzop9 | Container isolation, launch preflight and explicit image GC              |
| [security](../specs/security.md)             | [`crates/wrix-sandbox/`](../crates/wrix-sandbox/), [`lib/sandbox/`](../lib/sandbox/) | wx-1dhkm | Explicit credential grants, network isolation, and execution evidence    |
| [services](../specs/services.md)             | `crates/wrix-service/`                                                               | wx-fvr1x | Host-user control, shared service roles and draining updates             |
| [tmux](../specs/tmux.md)                     | [`lib/sandbox/profiles.nix`](../lib/sandbox/profiles.nix)                            | wx-4f3g  | Native tmux CLI debugging and caller-owned session lifecycle             |

[Loom workspace safety troubleshooting](loom-troubleshooting.md) covers missing
host associations and retained-history paths without resetting safety evidence.

[Darwin attention setup and validation](darwin-attention.md) covers the
optional, consumer-provided OmniWM CLI integration and desktop shortcuts.

## Terminology Index

| Term                              | Definition                                                                                                                                                                        |
| --------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **bd**                            | CLI for the beads issue tracker                                                                                                                                                   |
| **beads**                         | Persistent issue tracker (used by the `bd` CLI)                                                                                                                                   |
| **canonical host build pipeline** | Shared coordination for Wrix-managed host Nix target realization; [cache](../specs/cache.md#canonical-host-build-pipeline) owns scoped publication and in-flight store protection |
| **deploy key**                    | Repo-scoped SSH key for Git operations from host and container contexts                                                                                                           |
| **dolt**                          | SQL database backing beads; workspace namespaces hosted in the managed host-user service deployment                                                                               |
| **execution**                     | One sandbox launch attempt, distinct from an agent conversation and its terminal focus target                                                                                     |
| **focus-aware**                   | Notification suppression when terminal is focused                                                                                                                                 |
| **focus target**                  | Opaque host terminal/tmux target used for notification routing, not an execution or conversation ID                                                                               |
| **Git grant**                     | Independent effective `deploy` or `sign` permission to deliver a Wrix-managed key to a sandbox                                                                                    |
| **image source**                  | Platform image input produced by Nix: Linux `nix-descriptor`, Darwin `docker-archive`                                                                                             |
| **loom**                          | External Rust workflow orchestrator that drives wrix sandboxes ([taheris/loom](https://github.com/taheris/loom))                                                                  |
| **Nix store pool**                | Service-owned installed store/build state for one approved canonical repository and guest Linux system; distinct from Celler views and tool-cache pools                           |
| **pasta**                         | Linux userspace networking for Podman containers                                                                                                                                  |
| **playwright-mcp**                | MCP server wrapping @playwright/mcp for browser automation in sandboxes                                                                                                           |
| **prek**                          | Rust-based pre-commit framework (drop-in replacement for pre-commit)                                                                                                              |
| **project Nix cache**             | Celler-backed authorized read set of canonical repository-private and approved common public-dependency views; archives may be physically shared and age-evicted                  |
| **profile**                       | Pre-configured set of packages and environment variables                                                                                                                          |
| **ProfileConfig**                 | Immutable Nix-generated JSON config consumed by the Rust `wrix` launcher                                                                                                          |
| **sandbox**                       | Isolated container environment for running Pi, Claude, or a consumer-supplied agent runner                                                                                        |
| **host controller**               | Host-user Wrix control service owning stable role routing, private management/issuance and generation lifecycle outside replaceable containers; not an agent supervisor           |
| **service container**             | Managed host-user service-generation container; generations can overlap during updates while workspace namespaces and per-launch permissions remain scoped                        |
| **tmux**                          | Upstream terminal multiplexer used directly through shell tools for persistent debugging processes                                                                                |
| **tool cache**                    | Wrix-managed sandbox-only Cargo registry/git, sccache or uv data; shared among workers or private to one launch, distinct from the project Nix cache                              |
| **verify target**                 | Logical verifier ID of the form `verify:<domain>.<check-id>`; `.#verify --list` is the authoritative ID inventory and runner config batches selected IDs through `.#verify`       |
| **virtio-fs**                     | Shared filesystem for macOS container VMs                                                                                                                                         |
| **wrix init**                     | Repo-local init namespace: bare verification, deploy-key provisioning, and explicit repo setup for Git/signing/hooks                                                              |
| **wrix.toml**                     | Optional repo-root Wrix policy override file; absent when defaults are sufficient                                                                                                 |
