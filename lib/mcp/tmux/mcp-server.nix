# tmux-mcp: MCP server for tmux pane management
#
# Provides tools for AI-assisted debugging within wrapix sandboxes:
# - tmux_create_pane: Spawn panes running commands
# - tmux_send_keys: Send keystrokes to panes
# - tmux_capture_pane: Capture pane output
# - tmux_kill_pane: Terminate panes
# - tmux_list_panes: List active panes
#
# Usage: nix build .#tmux-mcp
{
  pkgs,
  crane,
  fenix,
}:

let

  fenixPkgs = fenix.packages.${pkgs.stdenv.hostPlatform.system};
  toolchain = fenixPkgs.combine [
    fenixPkgs.stable.defaultToolchain
  ];
  craneLib = (crane.mkLib pkgs).overrideToolchain (_: toolchain);

  cleanedSrc = craneLib.cleanCargoSource ./tmux-mcp;

  commonArgs = {
    src = cleanedSrc;
    cargoLock = ./tmux-mcp/Cargo.lock;
    buildInputs = [ pkgs.tmux ];
    propagatedBuildInputs = [ pkgs.tmux ];
    meta = {
      description = "MCP server providing tmux pane management for AI-assisted debugging";
      mainProgram = "tmux-mcp";
    };
  };

  cargoArtifacts = craneLib.buildDepsOnly commonArgs;

  bin = craneLib.buildPackage (
    commonArgs
    // {
      inherit cargoArtifacts;
      doCheck = false;
    }
  );

  clippy = craneLib.cargoClippy (
    commonArgs
    // {
      inherit cargoArtifacts;
      cargoClippyExtraArgs = "--all-targets";
    }
  );

  nextest = craneLib.cargoNextest (
    commonArgs
    // {
      inherit cargoArtifacts;
    }
  );
in
{
  inherit
    bin
    clippy
    nextest
    cargoArtifacts
    ;
}
