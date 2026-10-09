# Native tmux Debugging

Persistent terminal workflows inside Wrix sandboxes through the upstream tmux
CLI.

## Problem Statement

Agents need to run development servers, send input, and inspect output across
separate tool calls. Native tmux already supplies these capabilities through
Bash. Wrix provides the packaged tool and concise usage guidance rather than
owning an additional MCP protocol, pane registry, or process-management wrapper.

## Architecture

The base profile's native tmux package is owned by `profiles.md`. Agents invoke
it through their ordinary shell tool; Pi may also compose shell calls through
native codemode as described in `sandbox.md`. Container isolation and runtime
permissions remain owned by `sandbox.md` and `security.md`.

The supported workflow uses an explicitly selected, workflow-local tmux socket
and targets returned by tmux itself. It does not address an unrelated default
server or maintain Wrix-specific pane identifiers. `remain-on-exit` is enabled
before the workload starts so even immediately exiting commands remain
inspectable. Native pane status and exit-code queries distinguish a running
process from an exited one; capture reads available scrollback. Detachment here
means a tmux session continues between shell calls inside the running sandbox,
not that the Wrix launcher detaches; `sandbox.md` owns launch semantics.

The caller explicitly removes its sessions when finished. Shell-tool completion
or agent conversation completion does not implicitly destroy them; stopping the
container is the final process-cleanup boundary. No persistence across container
replacement or automatic process recovery is promised.

## Success Criteria

- A packaged sandbox supports a detached development server across independent
  shell calls, a request from another process, and capture of the server output
  through native tmux without any MCP server
  [system?](test-ci:test-tmux-cli-workflow)
- The documented workflow preserves an immediately exited command's output and
  exit status for later inspection, while distinguishing running panes
  [system?](test-ci:test-tmux-cli-exited-process)
- Explicit native cleanup removes only the caller's selected session, leaving an
  unrelated tmux session intact
  [system?](test-ci:test-tmux-cli-targeted-cleanup)
- Stopping a sandbox terminates its surviving tmux processes without affecting
  an unrelated sandbox's processes
  [system?](test-ci:test-tmux-cli-container-cleanup)
- Wrix exposes no tmux MCP package or registry entry, rejects `mcp.tmux` at Nix
  evaluation, and provides no tmux MCP diagnostic options, environment
  forwarding, or replacement process wrapper
  [check](verify:tmux.native-only-surface)
- Runtime selection of the unregistered `tmux` MCP server fails startup rather
  than silently selecting a replacement
  [system](verify:tmux.retired-selection-rejected)
- Shipped agent guidance concisely covers workflow-local socket selection,
  create/list, literal text versus special keys, capture, exited-process
  inspection, and targeted cleanup, while distinguishing finite scrollback from
  durable application logs
  [judge](../tests/judges/tmux.sh#test_native_workflow_guidance)

## Requirements

### Functional

1. **Native workflow** — create, inspect, send input, capture, and terminate
   using upstream tmux commands and identifiers, without an additional
   Wrix-owned or MCP server. Upstream tmux still uses its native server process.
2. **Post-mortem output** — the documented remain-on-exit workflow retains
   exited panes until explicit cleanup or container termination.
3. **Caller-owned lifecycle** — the agent owns its selected tmux sessions;
   container termination provides the final boundary, not a harness-session
   shutdown hook.

## Out of Scope

- Wrix-owned tmux MCP servers, protocol schemas, tool aliases, diagnostic logs,
  and pane registries; upstream tmux is the interface
- A replacement process framework, custom shell wrapper, or artifact service;
  application logs and exports remain caller-managed files
- Automatic per-conversation cleanup, cross-container debugging, persistent
  sessions across container replacement, and crash recovery
- Wrix-specific layout or debugger integration; callers may use upstream tmux or
  debugger capabilities directly
