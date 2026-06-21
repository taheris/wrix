# tmux-mcp

MCP server providing tmux pane management for AI-assisted debugging within wrix sandboxes.

## Problem Statement

AI agents lack the ability to debug applications the way humans do — running a server with debug logging in one terminal while sending test requests from another, watching logs scroll, and iterating. The single-command Bash tool does not support parallel observation. tmux-mcp exposes pane lifecycle and capture primitives so an agent can spawn a server, drive it, and read its output across turns.

## Architecture

A Rust binary implementing the MCP protocol (JSON-RPC over stdio) that drives a tmux session named `debug-{pid}`. The server runs inside the wrix container — `sandbox.md` is the security boundary; this spec adds no further isolation. Container construction, MCP opt-in plumbing (`mcp.tmux = { … }`), and trust model belong to `sandbox.md`. This spec owns the wire protocol, pane lifecycle, and audit format.

Load-bearing decisions:

- MCP server runs inside the wrix container — the sandbox IS the trust boundary, not the server
- Open command policy — pane processes inherit sandbox constraints, no extra command filtering
- `remain-on-exit on` so panes survive their process for post-mortem capture
- Optional audit logging (JSON Lines) for review and debugging-the-debugger

## MCP Tools

| Tool | Parameters | Description |
|------|------------|-------------|
| `tmux_create_pane` | `command: string`, `name?: string` | Create a new pane running the given command. Returns pane ID. |
| `tmux_send_keys` | `pane_id: string`, `keys: string` | Send keystrokes to a pane (interactive input or commands). |
| `tmux_capture_pane` | `pane_id: string`, `lines?: number` | Capture recent output from a pane. Default 100 lines, max 1000. |
| `tmux_kill_pane` | `pane_id: string` | Terminate a pane and its process. |
| `tmux_list_panes` | — | List all panes with IDs, names, status, and running commands. |

### Pane Lifecycle

Panes remain visible after their process exits so post-mortem capture works:

- tmux is configured with `remain-on-exit on`
- `tmux_list_panes` reports `status: "running"` or `status: "exited"`
- `tmux_capture_pane` on an exited pane returns final output (crash logs, stack traces)
- `tmux_kill_pane` removes the pane from both tmux and the server's internal state

### Error Format

Tool errors use MCP's standard `isError: true` response with plain-text messages:

```json
{
  "content": [{"type": "text", "text": "Pane 'debug-1' not found. Use tmux_list_panes to see active panes."}],
  "isError": true
}
```

Messages are descriptive and may include recovery hints. There are no structured error codes — the AI consumer reads natural language.

## Audit Log Format

Auditing is opt-in via the `mcp.tmux.audit` parameter (path) or `TMUX_DEBUG_AUDIT` environment variable. Output is JSON Lines, one event per line:

```json
{"ts": "2026-01-30T10:15:32Z", "tool": "create_pane", "pane_id": "debug-1", "command": "RUST_LOG=debug cargo run", "name": "server"}
{"ts": "2026-01-30T10:15:45Z", "tool": "send_keys", "pane_id": "debug-2", "keys": "curl -X POST localhost:3000/api/users"}
{"ts": "2026-01-30T10:15:46Z", "tool": "capture_pane", "pane_id": "debug-1", "lines": 200, "output_bytes": 4523}
{"ts": "2026-01-30T10:16:02Z", "tool": "kill_pane", "pane_id": "debug-1"}
```

Capture events log byte counts only to avoid log bloat. `mcp.tmux.auditFull = "<dir>"` (or `TMUX_DEBUG_AUDIT_FULL`) additionally writes full capture contents to numbered files in the directory.

## Configuration

Enabled per sandbox via the `mcp` parameter on `mkSandbox` (see `sandbox.md`):

```nix
mkSandbox {
  profile = profiles.rust;
  mcp.tmux = {
    audit = "/workspace/.debug-audit.log";       # optional JSONL audit
    auditFull = "/workspace/.debug-audit/";      # optional full capture dir
  };
}
```

Profiles do not need a `-debug` variant — MCP servers compose orthogonally with the base profile.

## Success Criteria

- The tmux-mcp integration suite passes: pane lifecycle (create/list/kill), `send_keys` + `capture_pane` round-trip, exited-pane status reporting, error-handling envelopes, audit-log JSON-Lines format, and session cleanup on server exit
  [system](bash tests/mcp/tmux/integration.sh)
- `mcp.tmux` composes with the rust-debug profile image via `mkSandbox`'s `mcp` parameter: the image build succeeds, tmux and tmux-mcp resolve on PATH inside the container, and the MCP server responds to a JSON-RPC `initialize` request
  [system](bash tests/mcp/tmux/e2e-sandbox.sh)
- Tool error responses construct `isError: true` envelopes via the MCP standard path
  [system](bash tests/mcp/tmux/tool-error-envelope.sh test_tool_handler_error_response_uses_mcp_success_envelope)
- No custom error-code field is present in the error envelope (the consumer reads plain text)
  [system](bash tests/mcp/tmux/tool-error-envelope.sh test_tool_handler_error_has_no_custom_error_code_field)

## Requirements

### Functional

1. **MCP tool surface** — the five tools above are registered and respond to the documented parameters; capture defaults to 100 lines and caps at 1000.
2. **Pane lifecycle visibility** — exited panes remain inspectable until explicitly killed; the server tracks `running` vs `exited` state.
3. **Audit logging** — opt-in JSONL log with the documented record shape; capture bodies optionally land in a per-capture file under `auditFull`.
4. **MCP opt-in via sandbox** — the server is enabled per-sandbox via `mcp.tmux = { … }`; it does not proliferate profile variants.
5. **Single managed tmux session** — the server owns one `debug-{pid}` session and tears it down on exit.

### Non-Functional

1. **Sandbox-only trust boundary** — no command filtering beyond what the wrix container enforces; pane processes inherit container constraints.
2. **No privilege escalation** — server runs as the same unprivileged user as Claude Code; tmux session is user-local.
3. **Plain-text errors** — MCP error responses carry natural-language messages, not structured codes.

## Out of Scope

- **GUI / TUI for viewing panes** — agents read via capture; no visual surface
- **Pane layout management** — single-window-per-pane; no splits or tiling
- **Cross-container debugging** — single-sandbox scope
- **Debugger integration (gdb / lldb)** — this is terminal-level debugging
- **Persistent sessions** — tmux session is ephemeral, tied to MCP server lifetime
