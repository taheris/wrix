{
  pkgs,
  system,
  linuxPkgs ? pkgs,
}:

let
  sandbox = import ./sandbox { inherit pkgs system linuxPkgs; };
  ralph = import ./ralph {
    inherit pkgs;
    inherit (sandbox) mkSandbox;
  };
  beads = import ./beads { inherit pkgs linuxPkgs; };
  city = import ./city {
    inherit pkgs linuxPkgs;
    inherit (sandbox) mkSandbox profiles baseClaudeSettings;
    inherit (ralph) mkRalph;
  };

in
{
  inherit (sandbox) profiles mkSandbox;
  inherit (city) mkCity;
  inherit (ralph) mkRalph scripts;
  inherit beads;

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
