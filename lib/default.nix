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

  sandboxLib = import ./sandbox {
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

  sandbox = sandboxLib // {
    mkSandbox =
      args:
      let
        resolved = sandboxLib.mkSandbox args;
      in
      resolved
      // {
        devShell =
          devShellArgs:
          if devShellArgs ? sandbox || devShellArgs ? profile then
            throw "sandbox.devShell does not accept `sandbox` or `profile`; use mkDevShell directly for profile-only shells"
          else
            devshell.mkDevShell ({ sandbox = resolved; } // devShellArgs);
      };
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
