# MCP Server Registry
#
# Maps server names to their definitions. Each server exports:
#   - name: Server identifier (string)
#   - packages: List of runtime packages (server binary + dependencies)
#   - mkServerConfig: Function to generate server config from user options
#
# Usage:
#   mcpRegistry = import ./mcp { inherit pkgs; };
#   serverDef = mcpRegistry.playwright;
#   config = serverDef.mkServerConfig { };
#
# This registry is used by mkSandbox to look up enabled MCP servers
# and merge their packages and configs.
#
# Spec: specs/playwright-mcp.md
{ pkgs }:

{
  # playwright: MCP server for browser automation
  # Provides tools for AI-assisted frontend development (screenshots, navigation, interaction, etc.)
  playwright = import ./playwright { inherit pkgs; };
}
