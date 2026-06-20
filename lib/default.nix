{
  pkgs,
  system,
  linuxPkgs ? pkgs,
  crane,
  fenix,
  treefmt,
}:

let
  beads = import ./beads {
    inherit pkgs;
    inherit (rustCli) wrix;
  };

  devshell = import ./devshell {
    inherit
      pkgs
      rustCli
      beads
      ;
  };

  prek = import ./prek { inherit pkgs; };

  profile = import ./profile { inherit sandbox; };

  rustCli = import ./services/rust.nix {
    inherit pkgs;
    rustProfile = sandbox.profiles.rust;
  };

  sandbox = import ./sandbox {
    inherit
      pkgs
      system
      linuxPkgs
      crane
      fenix
      treefmt
      ;
    serviceCli = rustCli.wrix;
  };

  tmuxMcp = import ./mcp/tmux/mcp-server.nix {
    inherit pkgs;
    rustProfile = sandbox.profiles.rust;
  };

in
{
  inherit (devshell) mkDevShell;
  inherit (prek) prekHooks prePushChecks skipIfMissing;
  inherit (profile) deriveProfile rustProfile;
  inherit (sandbox) profiles mkSandbox mkProfileImages;
  inherit beads;

  rustPackage = rustCli;
  tmuxMcpPackage = tmuxMcp;
}
