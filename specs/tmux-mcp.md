# tmux-mcp

MCP server providing tmux pane management for AI-assisted debugging within wrix
sandboxes.

## Problem Statement

AI agents lack the ability to debug applications the way humans do — running a
server with debug logging in one terminal while sending test requests from
another, watching logs scroll, and iterating. The single-command Bash tool does
not support parallel observation. tmux-mcp exposes pane lifecycle and capture
primitives so an agent can spawn a server, drive it, and read its output across
turns.

## Architecture

A Rust binary implementing the MCP protocol (JSON-RPC over stdio) drives a tmux
session named `debug-{pid}`. The server runs inside the wrix container —
`sandbox.md` is the security boundary; this spec adds no further isolation.
Container construction, MCP opt-in plumbing, runtime MCP image bundles, and the
trust model belong to `sandbox.md` and `profiles.md`. This spec owns the wire
protocol, pane lifecycle, and tmux-specific diagnostic emission format.

Load-bearing decisions:

- MCP server runs inside the wrix container — the sandbox IS the trust boundary,
  not the server
- Open command policy — pane processes inherit sandbox constraints, no extra
  command filtering
- `remain-on-exit on` so panes survive their process for post-mortem capture
- tmux-specific JSON Lines diagnostics under the component policy owned by
  `security.md`

## MCP Tools

| Tool                | Parameters                          | Description                                                     |
| ------------------- | ----------------------------------- | --------------------------------------------------------------- |
| `tmux_create_pane`  | `command: string`, `name?: string`  | Create a new pane running the given command. Returns pane ID.   |
| `tmux_send_keys`    | `pane_id: string`, `keys: string`   | Send keystrokes to a pane (interactive input or commands).      |
| `tmux_capture_pane` | `pane_id: string`, `lines?: number` | Capture recent output from a pane. Default 100 lines, max 1000. |
| `tmux_kill_pane`    | `pane_id: string`                   | Terminate a pane and its process.                               |
| `tmux_list_panes`   | —                                   | List all panes with IDs, names, status, and running commands.   |

### Pane Lifecycle

Panes remain visible after their process exits so post-mortem capture works:

- tmux is configured with `remain-on-exit on`
- `tmux_list_panes` reports `status: "running"` or `status: "exited"`
- `tmux_capture_pane` on an exited pane returns final output (crash logs, stack
  traces)
- `tmux_kill_pane` removes the pane from both tmux and the server's internal
  state

### Error Format

Tool errors use MCP's standard `isError: true` response with plain-text
messages:

```json
{
  "content": [{"type": "text", "text": "Pane 'debug-1' not found. Use tmux_list_panes to see active panes."}],
  "isError": true
}
```

Messages are descriptive and may include recovery hints. There are no structured
error codes — the AI consumer reads natural language.

## Component Diagnostic Format

The component-diagnostic enablement posture, relationship to Wrix's security
audit surface, and artifact-lifecycle ownership are defined by `security.md`.
tmux-mcp defines two inputs and their emission behavior:

- `mcp.tmux.audit = "<path>"` maps to `TMUX_DEBUG_AUDIT` and writes JSON Lines,
  one event per line.
- When base diagnostics are configured, `mcp.tmux.auditFull = "<dir>"` maps to
  `TMUX_DEBUG_AUDIT_FULL` and writes each full capture to a numbered file.

The JSON Lines records have these shapes:

```json
{"ts": "2026-01-30T10:15:32Z", "tool": "create_pane", "pane_id": "debug-1", "command": "RUST_LOG=debug cargo run", "name": "server"}
{"ts": "2026-01-30T10:15:45Z", "tool": "send_keys", "pane_id": "debug-2", "keys": "curl -X POST localhost:3000/api/users"}
{"ts": "2026-01-30T10:15:46Z", "tool": "capture_pane", "pane_id": "debug-1", "lines": 200, "output_bytes": 4523}
{"ts": "2026-01-30T10:16:02Z", "tool": "kill_pane", "pane_id": "debug-1"}
```

JSON Lines records include pane commands and sent keystrokes without secret
classification or redaction; either field may contain credentials or other
secrets. Capture events record byte counts only. Full-capture files contain the
unredacted pane output and may also contain credentials or other secrets.

## Success Criteria

- The tmux-mcp integration suite passes: pane lifecycle (create/list/kill),
  `send_keys` + `capture_pane` round-trip, exited-pane status reporting,
  error-handling envelopes, component-diagnostic JSON-Lines format, and session
  cleanup on server exit [system](verify:tmux-mcp.integration)
- `mcp.tmux` composes with the rust profile via an explicit
  `mkSandbox { mcp.tmux = { }; }` instantiation: the image build succeeds, tmux
  and tmux-mcp resolve on PATH inside the container, and the MCP server responds
  to a JSON-RPC `initialize` request [system](verify:tmux-mcp.e2e-sandbox)
- Tool error responses construct `isError: true` envelopes via the MCP standard
  path [test](mcp::tests::tool_handler_validation_errors_use_success_envelope)
- No custom error-code field is present in the error envelope (the consumer
  reads plain text)
  [test](mcp::tests::tool_handler_error_content_has_no_custom_code_field)

## Requirements

### Functional

1. **MCP tool surface** — the five tools above are registered and respond to the
   documented parameters; capture defaults to 100 lines and caps at 1000.
2. **Pane lifecycle visibility** — exited panes remain inspectable until
   explicitly killed; the server tracks `running` vs `exited` state.
3. **Component diagnostics** — `audit` emits the documented JSONL record shape,
   and `auditFull` adds numbered files containing unredacted capture bodies when
   base diagnostics are configured.
4. **Single managed tmux session** — the server owns one `debug-{pid}` session
   and tears it down on exit.

### Non-Functional

1. **Sandbox-only trust boundary** — no command filtering beyond what the wrix
   container enforces; pane processes inherit container constraints.
2. **No privilege escalation** — server runs as the same unprivileged user as
   the selected agent; tmux session is user-local.
3. **Plain-text errors** — MCP error responses carry natural-language messages,
   not structured codes.

## Out of Scope

- **GUI / TUI for viewing panes** — agents read via capture; no visual surface
- **Pane layout management** — single-window-per-pane; no splits or tiling
- **Cross-container debugging** — single-sandbox scope
- **Debugger integration (gdb / lldb)** — this is terminal-level debugging
- **Persistent sessions** — tmux session is ephemeral, tied to MCP server
  lifetime
- **Component-diagnostic security policy and artifact lifecycle** — owned by
  `security.md`
