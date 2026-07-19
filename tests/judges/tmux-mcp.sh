#!/usr/bin/env bash
set -euo pipefail

# Judge rubrics for tmux-mcp.md success criteria

test_context_isolation() {
  judge_files "lib/sandbox/default.nix" "lib/mcp/tmux/default.nix" "lib/mcp/tmux/mcp-server.nix"
  judge_criterion "MCP tools are isolated to a debug subagent rather than registered in the main session. The sandbox configuration shows MCP servers are added via an opt-in mcp parameter and their configs are placed in mcpServers within claudeSettings, meaning the main session has zero token overhead from debug panes and the debug subagent context is approximately 1.5k tokens (tool definitions only)."
}
