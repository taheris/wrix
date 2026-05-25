# Project Overview

Wrapix provides sandboxed containers for AI-driven development. See
`docs/architecture.md` for system design.

## Authoring Conventions

- [`docs/spec-conventions.md`](spec-conventions.md) — what a spec is and isn't,
  trust tiers, standard section structure. Pinned by `loom plan` sessions.
- [`docs/style-rules.md`](style-rules.md) — code-style and test-quality rules
  organized by rule family (SH-, NX-, DOC-, GIT-, TST-, RS-, COM-, CLI-).
  Pinned by `loom run` and `loom gate review` sessions.

## Specs

Individual spec files live in [`../specs/`](../specs/). This table is the session-start
pin — keep it current when specs land or retire.

| Spec | Code | Beads | Purpose |
|------|------|-------|---------|
| [beads.md](../specs/beads.md) | [`.beads/`](../.beads/) | — | Issue tracking with dependency support |
| [image-builder.md](../specs/image-builder.md) | [`lib/sandbox/image.nix`](../lib/sandbox/image.nix) | — | Nix-based OCI image creation |
| [linux-builder.md](../specs/linux-builder.md) | [`lib/builder/default.nix`](../lib/builder/default.nix) | wx-ope | Remote Nix builds for macOS |
| [live-specs.md](../specs/live-specs.md) | [`lib/ralph/cmd/spec.sh`](../lib/ralph/cmd/spec.sh) | wx-a13n | Queryable, verifiable, observable specifications |
| [notifications.md](../specs/notifications.md) | [`lib/notify/`](../lib/notify/) | wx-q6x | Desktop notifications with focus suppression |
| [playwright-mcp.md](../specs/playwright-mcp.md) | [`lib/mcp/playwright/`](../lib/mcp/playwright/) | wx-9mvh | Browser automation for frontend development |
| [pre-commit.md](../specs/pre-commit.md) | [`.pre-commit-config.yaml`](../.pre-commit-config.yaml), [`lib/ralph/cmd/run.sh`](../lib/ralph/cmd/run.sh) | wx-t6rh | Git hooks and ralph run integration |
| [profiles.md](../specs/profiles.md) | [`lib/sandbox/profiles.nix`](../lib/sandbox/profiles.nix) | wx-1thzk | Pre-configured development environments |
| [ralph-harness.md](../specs/ralph-harness.md) | [`lib/ralph/`](../lib/ralph/) | wx-6gd5g | Ralph platform: state, templates, utilities, init |
| [ralph-loop.md](../specs/ralph-loop.md) | [`lib/ralph/cmd/{plan,todo,run}.sh`](../lib/ralph/cmd/) | wx-1ic8b | Ralph forward pipeline: plan → todo → run |
| [ralph-review.md](../specs/ralph-review.md) | [`lib/ralph/cmd/{check,msg}.sh`](../lib/ralph/cmd/) | wx-qvuhk | Ralph review gate: invariant clash, options format, push gate, clarify resolution |
| [ralph-tests.md](../specs/ralph-tests.md) | [`tests/ralph/`](../tests/ralph/) | wx-h0qqy | Integration tests for ralph workflow |
| [sandbox.md](../specs/sandbox.md) | [`lib/sandbox/default.nix`](../lib/sandbox/default.nix) | — | Platform-agnostic container isolation |
| [security-review.md](../specs/security-review.md) | — | wx-eok | Security tradeoffs and mitigations |
| [tmux-mcp.md](../specs/tmux-mcp.md) | [`lib/mcp/tmux/`](../lib/mcp/tmux/) | wx-4f3g | AI-assisted debugging via tmux panes |
| [loom-harness.md](../specs/loom-harness.md) | [`loom/`](../loom/) | wx-3hhwq | Loom platform: crate structure, workspace lints, process architecture, state store, command set |
| [loom-agent.md](../specs/loom-agent.md) | [`loom/crates/loom-agent/`](../loom/crates/loom-agent/) | wx-pkht8 | Agent backend abstraction: pi-mono RPC, Claude Code stream-json, and Direct (loom-llm + sandbox-aware tools via `loom-direct-runner`) |
| [loom-templates.md](../specs/loom-templates.md) | [`loom/crates/loom-templates/`](../loom/crates/loom-templates/) | wx-z28qe | Askama templates, partials inventory, per-phase pinning policy |
| [loom-llm.md](../specs/loom-llm.md) | [`loom/crates/loom-llm/`](../loom/crates/loom-llm/) | — | Public-contract LLM primitives: `LlmClient`, typed `CacheControl`, `Conversation` with built-in tool-use loop, agent-loop observers (doom-loop, duplicate-result) |
| [loom-gate.md](../specs/loom-gate.md) | [`loom/crates/loom-gate/`](../loom/crates/loom-gate/) | — | Quality gate: conformance + style + test-quality dimensions, plan/per-diff/standing stages, `loom gate verify` (deterministic) + `loom gate review` (LLM judge) |
| [loom-tests.md](../specs/loom-tests.md) | [`tests/loom/`](../tests/loom/) | wx-lfuuh | Test strategy: unit, integration, system tests for Loom |

## Terminology Index

| Term | Definition |
|------|------------|
| **bd** | CLI for the beads issue tracker |
| **Beads** | Persistent issue tracker used by Ralph and the `bd` CLI |
| **Deploy Key** | SSH key for git push operations from container |
| **Dolt** | SQL database backing beads; shared via `beads-dolt` container |
| **Focus-aware** | Notification suppression when terminal is focused |
| **Loom** | Rust agent driver replacing Ralph's bash scripts; supports pi-mono and Claude Code backends |
| **JSONL** | JSON Lines — one complete JSON object per `\n`-terminated line (same format as NDJSON; JSONL is the term Loom uses); protocol framing for both pi-mono RPC and Claude stream-json |
| **pasta** | Linux userspace networking for Podman containers |
| **playwright-mcp** | MCP server wrapping @playwright/mcp for browser automation in sandboxes |
| **prek** | Rust-based pre-commit framework (drop-in replacement for pre-commit) |
| **Profile** | Pre-configured set of packages and environment variables |
| **ralph:clarify** | Bead label for items awaiting human response via `ralph msg` |
| **ralph:scaffold** | Bead label for docs scaffolded by `ralph sync` |
| **Ralph** | Workflow orchestrator for spec-to-implementation |
| **Sandbox** | Isolated container environment for running Claude Code |
| **tmux-mcp** | MCP server for AI-assisted debugging via tmux panes |
| **virtio-fs** | Shared filesystem for macOS container VMs |
| **Worktree** | Per-bead git worktree under `.wrapix/worktree/<bead-id>` |
