# Project Overview

Wrix provides sandboxed containers for AI-driven development. See
`docs/architecture.md` for system design.

## Authoring Conventions

- [`docs/spec-conventions.md`](spec-conventions.md) — what a spec is and isn't,
  trust tiers, standard section structure.
- [`docs/style-rules.md`](style-rules.md) — code-style and test-quality rules
  organized by rule family (SH-, NX-, DOC-, GIT-, TST-, RS-, COM-, CLI-).

## Specs

Individual spec files live in [`../specs/`](../specs/). This table is the session-start
pin — keep it current when specs land or retire.

| Spec | Code | Beads | Purpose |
|------|------|-------|---------|
| [beads.md](../specs/beads.md) | [`.beads/`](../.beads/) | wx-v7m8n | Issue tracking with dependency support |
| [image-builder.md](../specs/image-builder.md) | [`lib/sandbox/image.nix`](../lib/sandbox/image.nix) | wx-nf6eu | Nix-based OCI image source creation |
| [linux-builder.md](../specs/linux-builder.md) | [`lib/builder/default.nix`](../lib/builder/default.nix) | wx-ope | Remote Nix builds for macOS |
| [notifications.md](../specs/notifications.md) | [`lib/notify/`](../lib/notify/) | wx-q6x | Desktop notifications with focus suppression |
| [playwright-mcp.md](../specs/playwright-mcp.md) | [`lib/mcp/playwright/`](../lib/mcp/playwright/) | wx-9mvh | Browser automation for frontend development |
| [pre-commit.md](../specs/pre-commit.md) | [`.pre-commit-config.yaml`](../.pre-commit-config.yaml) | wx-t6rh | Git hooks for treefmt, shellcheck, and integration tests |
| [profiles.md](../specs/profiles.md) | [`lib/sandbox/profiles.nix`](../lib/sandbox/profiles.nix) | wx-1thzk | Pre-configured development environments |
| [sandbox.md](../specs/sandbox.md) | [`lib/sandbox/default.nix`](../lib/sandbox/default.nix) | wx-fzop9 | Platform-agnostic container isolation |
| [security.md](../specs/security.md) | [`lib/sandbox/{linux,darwin}/default.nix`](../lib/sandbox/) | wx-1dhkm | Cross-cutting credential, network, and audit-trail invariants |
| [services.md](../specs/services.md) | `crates/wrix-service/`, `crates/wrix-cache/` | wx-fvr1x | Per-workspace service container and project Nix cache |
| [tmux-mcp.md](../specs/tmux-mcp.md) | [`lib/mcp/tmux/`](../lib/mcp/tmux/) | wx-4f3g | AI-assisted debugging via tmux panes |

## Terminology Index

| Term | Definition |
|------|------------|
| **bd** | CLI for the beads issue tracker |
| **beads** | Persistent issue tracker (used by the `bd` CLI) |
| **deploy key** | SSH key for git push operations from container |
| **dolt** | SQL database backing beads; shared via the workspace service container |
| **focus-aware** | Notification suppression when terminal is focused |
| **image source** | Platform image input produced by Nix: Linux `nix-descriptor`, Darwin `docker-archive` |
| **loom** | External Rust workflow orchestrator that drives wrix sandboxes ([taheris/loom](https://github.com/taheris/loom)) |
| **pasta** | Linux userspace networking for Podman containers |
| **playwright-mcp** | MCP server wrapping @playwright/mcp for browser automation in sandboxes |
| **prek** | Rust-based pre-commit framework (drop-in replacement for pre-commit) |
| **project Nix cache** | Per-workspace local binary cache for Nix derivations scoped to a repository |
| **profile** | Pre-configured set of packages and environment variables |
| **ProfileConfig** | Immutable Nix-generated JSON config consumed by the Rust `wrix` launcher |
| **sandbox** | Isolated container environment for running Claude Code |
| **service container** | Per-workspace `<repo>-service` container hosting shared local services |
| **tmux-mcp** | MCP server for AI-assisted debugging via tmux panes |
| **virtio-fs** | Shared filesystem for macOS container VMs |
