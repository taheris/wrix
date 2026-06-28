# tmux-mcp: MCP server for tmux pane management
#
# Provides tools for AI-assisted debugging within wrix sandboxes:
# - tmux_create_pane: Spawn panes running commands
# - tmux_send_keys: Send keystrokes to panes
# - tmux_capture_pane: Capture pane output
# - tmux_kill_pane: Terminate panes
# - tmux_list_panes: List active panes
#
# Usage: nix build .#tmux-mcp
{ pkgs, rustProfile }:

rustProfile.buildPackage {
  src = ../../..;
  cargoLock = ../../../Cargo.lock;
  cargoExtraArgs = "-p tmux-mcp";

  # tmux must be on PATH at runtime; propagate so consumers pick it up.
  buildInputs = [ pkgs.tmux ];
  propagatedBuildInputs = [ pkgs.tmux ];

  meta = {
    description = "MCP server providing tmux pane management for AI-assisted debugging";
    mainProgram = "tmux-mcp";
  };
}
