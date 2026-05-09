# tmux MCP server definition
#
# Server providing tmux pane management for AI-assisted debugging
# within wrapix sandboxes.
#
# Exports:
#   - name: Server identifier ("tmux")
#   - packages: Runtime packages (MCP server binary + tmux)
#   - mkServerConfig: Function to generate server config from user options
#
# Config options:
#   - audit: Path to audit log file (optional)
#   - auditFull: Path to directory for full capture logging (optional)
#
# Spec: specs/tmux-mcp.md
{
  pkgs,
  crane,
  fenix,
}:

{
  name = "tmux";

  # tmux must be listed explicitly: buildEnv doesn't follow propagatedBuildInputs
  packages = [
    (import ./mcp-server.nix { inherit pkgs crane fenix; }).bin
    pkgs.tmux
  ];

  mkServerConfig =
    {
      audit ? null,
      auditFull ? null,
    }:
    {
      command = "tmux-mcp";
      env =
        { }
        // (if audit != null then { TMUX_DEBUG_AUDIT = audit; } else { })
        // (if auditFull != null then { TMUX_DEBUG_AUDIT_FULL = auditFull; } else { });
    };
}
