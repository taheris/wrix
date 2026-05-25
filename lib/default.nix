{
  pkgs,
  system,
  linuxPkgs ? pkgs,
  crane ? null,
  fenix ? null,
  treefmt ? null,
}:

let
  loom = import ./loom {
    inherit pkgs crane fenix;
  };
  loomLinux =
    if linuxPkgs == pkgs then
      loom
    else
      import ./loom {
        pkgs = linuxPkgs;
        inherit crane fenix;
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
    loomLinuxPackage = loomLinux;
  };
  beads = import ./beads { inherit pkgs linuxPkgs; };
  ralph = import ./ralph {
    inherit pkgs beads;
    inherit (sandbox) mkSandbox;
  };
  tmuxMcp = import ./mcp/tmux/mcp-server.nix {
    inherit pkgs crane fenix;
  };

  # Host-native Rust toolchain for the devShell (includes rust-analyzer + src
  # for IDE support).  Container images use the Linux toolchain from
  # sandbox.profiles.rust; this is its macOS/host counterpart.
  devToolchain =
    if fenix != null then
      let
        hostFenixPkgs = fenix.packages.${system};
      in
      hostFenixPkgs.combine [
        hostFenixPkgs.stable.defaultToolchain
        hostFenixPkgs.stable.rust-analyzer-preview
        hostFenixPkgs.stable.rust-src
      ]
    else
      null;

in
{
  inherit (sandbox) profiles mkSandbox mkProfileImages;
  inherit (ralph) mkRalph scripts;
  ralphPackage = ralph.package;
  ralphInitApp = ralph.initApp;
  loomPackage = loom;
  loomLinuxPackage = loomLinux;
  tmuxMcpPackage = tmuxMcp;
  inherit beads devToolchain;

  deriveProfile =
    baseProfile: extensions:
    baseProfile
    // extensions
    // {
      packages = (baseProfile.packages or [ ]) ++ (extensions.packages or [ ]);
      mounts = (baseProfile.mounts or [ ]) ++ (extensions.mounts or [ ]);
      env = (baseProfile.env or { }) // (extensions.env or { });
      networkAllowlist = (baseProfile.networkAllowlist or [ ]) ++ (extensions.networkAllowlist or [ ]);
    };

  mkDevShell =
    {
      packages ? [ ],
      shellHook ? "",
    }:
    pkgs.mkShell {
      packages = [
        pkgs.beads
        pkgs.beads-dolt
        pkgs.beads-push
        pkgs.dolt
        pkgs.prek
      ]
      ++ packages;
      shellHook = ''
        ${shellHook}
        # Configure Dolt origin remote for bd dolt pull/push (no-op if already set)
        if [ -d .beads/dolt/beads/.dolt ] && [ -d .git/beads-worktrees/beads/.beads/dolt-remote ]; then
          _dolt_remote="file://$PWD/.git/beads-worktrees/beads/.beads/dolt-remote"
          (cd .beads/dolt/beads && dolt remote add origin "$_dolt_remote" 2>/dev/null || true)
        fi

        # Start per-workspace dolt container and export env vars suppressing
        # bd's embedded autostart. See lib/beads/default.nix.
        ${beads.shellHook}

        # Ensure prek owns .git/hooks/ — bd hooks install can overwrite the shim
        if [ -d .git ] && [ -f .pre-commit-config.yaml ] && ! grep -q 'prek' .git/hooks/pre-commit 2>/dev/null; then
          echo "Installing prek hooks (bd shim detected or hooks missing)..."
          prek install -f
          chmod 555 .git/hooks/
        fi

        echo "Wrapix development shell"
      '';
    };
}
