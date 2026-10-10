# Project Overview

Wrix provides sandboxed containers for AI-driven development. See
`docs/architecture.md` for system design.

## Authoring Conventions

- [`docs/spec-conventions.md`](spec-conventions.md) — what a spec is and isn't,
  trust tiers, standard section structure.
- [`docs/style-rules.md`](style-rules.md) — code-style and test-quality rules
  organized by rule family (SH-, NX-, DOC-, GIT-, TST-, RS-, COM-, CLI-).

[Verifier input discovery and measurements](verifier-inputs.md) describes
checked resource projections, conservative scope, and measured worker costs.

## Specs

Individual spec files live in `specs/`. This table is the session-start pin —
keep it current when specs land or retire.

| Spec                                         | Code                                                                                 | Beads    | Purpose                                                                  |
| -------------------------------------------- | ------------------------------------------------------------------------------------ | -------- | ------------------------------------------------------------------------ |
| [beads](../specs/beads.md)                   | [`.beads/`](../.beads/)                                                              | wx-v7m8n | Issue tracking with dependency support                                   |
| [cli](../specs/cli.md)                       | [`crates/wrix-cli/`](../crates/wrix-cli/), `.#verify`                                | —        | Wrix command surface, repository initialization, and shared verifier app |
| [image-builder](../specs/image-builder.md)   | [`lib/sandbox/image.nix`](../lib/sandbox/image.nix)                                  | wx-nf6eu | Nix-based OCI image source creation                                      |
| [linux-builder](../specs/linux-builder.md)   | [`lib/builder/default.nix`](../lib/builder/default.nix)                              | wx-ope   | Remote Nix builds for macOS                                              |
| [notifications](../specs/notifications.md)   | [`lib/notify/`](../lib/notify/)                                                      | wx-q6x   | Agent-neutral desktop attention notifications with focus suppression     |
| [playwright-mcp](../specs/playwright-mcp.md) | [`lib/mcp/playwright/`](../lib/mcp/playwright/)                                      | wx-9mvh  | Browser automation for frontend development                              |
| [pre-commit](../specs/pre-commit.md)         | [`.pre-commit-config.yaml`](../.pre-commit-config.yaml)                              | wx-t6rh  | Git hooks for treefmt, shellcheck, and integration tests                 |
| [profiles](../specs/profiles.md)             | [`lib/sandbox/profiles.nix`](../lib/sandbox/profiles.nix)                            | wx-1thzk | Pre-configured development environments                                  |
| [sandbox](../specs/sandbox.md)               | [`lib/sandbox/default.nix`](../lib/sandbox/default.nix)                              | wx-fzop9 | Platform-agnostic container isolation                                    |
| [security](../specs/security.md)             | [`crates/wrix-sandbox/`](../crates/wrix-sandbox/), [`lib/sandbox/`](../lib/sandbox/) | wx-1dhkm | Explicit credential grants, network isolation, and execution evidence    |
| [services](../specs/services.md)             | `crates/wrix-service/`, `crates/wrix-cache/`                                         | wx-fvr1x | Per-workspace service container and project Nix cache                    |
| [tmux](../specs/tmux.md)                     | [`lib/sandbox/profiles.nix`](../lib/sandbox/profiles.nix)                            | wx-4f3g  | Native tmux CLI debugging and caller-owned session lifecycle             |

## Terminology Index

| Term                  | Definition                                                                                                                                                                  |
| --------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **bd**                | CLI for the beads issue tracker                                                                                                                                             |
| **beads**             | Persistent issue tracker (used by the `bd` CLI)                                                                                                                             |
| **deploy key**        | Repo-scoped SSH key for Git operations from host and container contexts                                                                                                     |
| **dolt**              | SQL database backing beads; shared via the workspace service container                                                                                                      |
| **execution**         | One sandbox launch attempt, distinct from an agent conversation and its terminal focus target                                                                               |
| **focus-aware**       | Notification suppression when terminal is focused                                                                                                                           |
| **focus target**      | Opaque host terminal/tmux target used for notification routing, not an execution or conversation ID                                                                         |
| **Git grant**         | Independent effective `deploy` or `sign` permission to deliver a Wrix-managed key to a sandbox                                                                              |
| **image source**      | Platform image input produced by Nix: Linux `nix-descriptor`, Darwin `docker-archive`                                                                                       |
| **loom**              | External Rust workflow orchestrator that drives wrix sandboxes ([taheris/loom](https://github.com/taheris/loom))                                                            |
| **pasta**             | Linux userspace networking for Podman containers                                                                                                                            |
| **playwright-mcp**    | MCP server wrapping @playwright/mcp for browser automation in sandboxes                                                                                                     |
| **prek**              | Rust-based pre-commit framework (drop-in replacement for pre-commit)                                                                                                        |
| **project Nix cache** | Per-workspace local binary cache for Nix derivations scoped to a repository                                                                                                 |
| **profile**           | Pre-configured set of packages and environment variables                                                                                                                    |
| **ProfileConfig**     | Immutable Nix-generated JSON config consumed by the Rust `wrix` launcher                                                                                                    |
| **sandbox**           | Isolated container environment for running Pi, Claude, or a consumer-supplied agent runner                                                                                  |
| **service container** | Per-workspace `<repo>-service` container hosting shared local services                                                                                                      |
| **tmux**              | Upstream terminal multiplexer used directly through shell tools for persistent debugging processes                                                                          |
| **verify target**     | Logical verifier ID of the form `verify:<domain>.<check-id>`; `.#verify --list` is the authoritative ID inventory and runner config batches selected IDs through `.#verify` |
| **virtio-fs**         | Shared filesystem for macOS container VMs                                                                                                                                   |
| **wrix init**         | Repo-local bootstrap command that configures and verifies Wrix-managed Git transport, signing, and hooks                                                                    |
| **wrix.toml**         | Optional repo-root Wrix policy override file; absent when defaults are sufficient                                                                                           |
