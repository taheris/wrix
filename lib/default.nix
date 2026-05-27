{
  pkgs,
  system,
  linuxPkgs ? pkgs,
  crane ? null,
  fenix ? null,
  treefmt ? null,
}:

let
  sandbox = import ./sandbox {
    inherit
      pkgs
      system
      linuxPkgs
      crane
      fenix
      treefmt
      ;
  };
  beads = import ./beads { inherit pkgs linuxPkgs; };
  tmuxMcp = import ./mcp/tmux/mcp-server.nix {
    inherit pkgs crane fenix;
  };

in
{
  inherit (sandbox) profiles mkSandbox mkProfileImages;
  tmuxMcpPackage = tmuxMcp;
  inherit beads;

  prekHooks = pkgs.runCommand "wrapix-prek-hooks" { } ''
    install -Dm 555 ${./prek/hooks/pre-commit}         $out/pre-commit
    install -Dm 555 ${./prek/hooks/pre-push}           $out/pre-push
    install -Dm 555 ${./prek/hooks/prepare-commit-msg} $out/prepare-commit-msg
    install -Dm 555 ${./prek/hooks/post-checkout}      $out/post-checkout
    install -Dm 555 ${./prek/hooks/post-merge}         $out/post-merge
    install -Dm 444 ${./prek/lock.sh}                  $out/_lib/lock.sh
  '';

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

  # Top-level constructor for project-pinned rust profiles. `toolchain` is the
  # path to a rust-toolchain.toml; `sha256` is the fenix purity hash. Both are
  # required — Nix's destructuring errors when either is omitted, matching the
  # "no silent unpinned profile" invariant in specs/profiles.md.
  rustProfile =
    {
      toolchain,
      sha256,
      packages ? [ ],
      env ? { },
      mounts ? [ ],
      networkAllowlist ? [ ],
    }:
    let
      base = sandbox.rustProfileFromFile {
        file = toolchain;
        inherit sha256;
      };
    in
    base
    // {
      packages = base.packages ++ packages;
      env = base.env // env;
      mounts = base.mounts ++ mounts;
      networkAllowlist = base.networkAllowlist ++ networkAllowlist;
    };

  mkDevShell =
    {
      profile,
      packages ? [ ],
      env ? { },
      shellHook ? "",
    }:
    pkgs.mkShell {
      packages = profile.packages ++ packages;
      env = profile.env // env;
      shellHook = ''
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

        ${profile.shellHook or ""}
        ${shellHook}
      '';
    };
}
