# MCP Server Registry
#
# Maps server names to their definitions. Each server exports:
#   - name: Server identifier (string)
#   - packages: List of runtime packages (server binary + dependencies)
#   - mkServerConfig: Function to generate server config from user options
#
# Usage:
#   mcpRegistry = import ./mcp { inherit pkgs; };
#   serverDef = mcpRegistry.tmux;
#   config = serverDef.mkServerConfig { audit = "/path/to/audit.log"; };
#
# This registry is used by mkSandbox to look up enabled MCP servers
# and merge their packages and configs.
#
# Spec: specs/tmux-mcp.md
{
  pkgs,
  rustProfile,
}:

{
  # tmux: MCP server for tmux pane management
  # Provides tools for AI-assisted debugging (create_pane, send_keys, capture_pane, etc.)
  tmux = import ./tmux { inherit pkgs rustProfile; };

  # playwright: MCP server for browser automation
  # Provides tools for AI-assisted frontend development (screenshots, navigation, interaction, etc.)
  playwright = import ./playwright { inherit pkgs; };
}
