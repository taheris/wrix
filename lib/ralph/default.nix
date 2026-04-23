# Ralph Wiggum Loop for AI-driven development.
#
# Provides unified ralph command with subcommands:
#   plan, logs, edit, tune, ready, step, loop, status, check, diff, sync
{
  pkgs,
  mkSandbox ? null, # only needed if using mkRalph
  beads ? null, # beads module (provides cli + shellHook); enables socket bootstrap
}:

let
  inherit (builtins) toJSON;
  inherit (pkgs) runCommand writeText;
  inherit (pkgs.lib) mapAttrs;

  templateDir = ./template;

  # Import template module for validation
  templateModule = import ./template/default.nix { inherit (pkgs) lib; };

  # Pre-compute metadata as JSON files for shell scripts
  # This avoids needing to call nix eval at runtime (which may fail in containers)
  variablesJson = writeText "ralph-variables.json" templateModule.variablesJson;

  # Template variables as JSON (maps template name -> list of variable names)
  templatesJson = writeText "ralph-templates.json" (
    toJSON (mapAttrs (_name: tmpl: tmpl.variables) templateModule.templates)
  );

  # All ralph scripts bundled in a single derivation
  scripts = runCommand "ralph-scripts" { nativeBuildInputs = [ pkgs.coreutils ]; } ''
    mkdir -p $out/bin $out/share/ralph
    for script in ${./cmd}/*.sh; do
      name=$(basename "$script" .sh)
      if [ "$name" = "util" ]; then
        # util.sh is sourced, not executed directly
        cp "$script" $out/bin/util.sh
      elif [ "$name" = "ralph" ]; then
        # main entry point has no prefix
        cp "$script" $out/bin/ralph
        chmod +x $out/bin/ralph
      else
        # subcommands get ralph- prefix
        cp "$script" $out/bin/ralph-$name
        chmod +x $out/bin/ralph-$name
      fi
    done

    # Compute source hash for staleness detection at runtime
    # Scripts and templates are hashed separately so each can warn independently
    cat ${./cmd}/*.sh | sha256sum | cut -d' ' -f1 > $out/share/ralph/scripts-hash
    find ${./template} -type f | sort | xargs cat | sha256sum | cut -d' ' -f1 > $out/share/ralph/templates-hash

    # Copy pre-computed metadata
    cp ${variablesJson} $out/share/ralph/variables.json
    cp ${templatesJson} $out/share/ralph/templates.json
  '';

in
{
  inherit scripts templateDir;

  # `shellHook` passthru is composed by lib/ralph/template/flake.nix.
  package = scripts.overrideAttrs (old: {
    passthru = (old.passthru or { }) // {
      shellHook = ''
        export PATH="${scripts}/bin:$PATH"
        export RALPH_TEMPLATE_DIR="''${RALPH_TEMPLATE_DIR:-${templateDir}}"
        export RALPH_METADATA_DIR="${scripts}/share/ralph"
      '';
    };
  });

  # `nix run #init` — wraps `ralph init` with template/metadata env and bd on PATH.
  initApp = {
    meta.description = "Bootstrap a wrapix project in cwd";
    type = "app";
    program = "${pkgs.writeShellScriptBin "ralph-init-runner" ''
      set -euo pipefail
      export PATH="${scripts}/bin:${pkgs.beads}/bin:$PATH"
      export RALPH_TEMPLATE_DIR="${templateDir}"
      export RALPH_METADATA_DIR="${scripts}/share/ralph"
      exec ralph init "$@"
    ''}/bin/ralph-init-runner";
  };

  # Template validation for flake checks
  # Usage: ralph.lib.mkTemplatesCheck pkgs
  mkTemplatesCheck = templateModule.mkTemplatesCheck pkgs;

  # Create ralph support for a given sandbox or profile
  # Returns: { packages, shellHook, app, sandbox }
  # - packages: list to add to devShell
  # - shellHook: shell setup for PATH and env vars
  # - app: nix app definition for `nix run`
  # - sandbox: the sandbox used (with package and profile)
  #
  # Usage:
  #   mkRalph { sandbox = mySandbox; }              # Use existing sandbox
  #   mkRalph { profile = profiles.rust; }          # Create sandbox from profile
  #   mkRalph { profile = profiles.rust; env = {}; } # Profile with extensions
  mkRalph =
    {
      sandbox ? null,
      profile ? null,
      packages ? [ ],
      mounts ? [ ],
      env ? { },
    }:
    let
      effectiveSandbox =
        if sandbox != null then
          sandbox
        else if profile != null then
          mkSandbox {
            inherit
              env
              mounts
              packages
              profile
              ;
          }
        else
          throw "mkRalph requires either 'sandbox' or 'profile' argument";

      wrapixBin = effectiveSandbox.package;

      # Host-side beads bootstrap: start the per-workspace beads-dolt
      # container and export connection info so `bd dolt pull` in
      # ralph/cmd/run.sh talks to the shared server (TCP on Darwin,
      # socket on Linux). Fails loudly if the server never appears — no
      # fallback to bd's embedded autostart. No-op when beads arg is
      # null (template-only consumers).
      beadsBootstrap =
        if beads != null then
          ''
            if [ -d "$PWD/.beads/dolt" ]; then
              if ! command -v podman >/dev/null 2>&1; then
                echo "ralph: .beads/dolt exists but podman is not on PATH" >&2
                return 1 2>/dev/null || exit 1
              fi
              ${beads.dolt}/bin/beads-dolt start "$PWD"
              ${beads.waitAndExport}
            fi
          ''
        else
          "";

      beadsPackages = if beads != null then [ beads.cli ] else [ ];
    in
    {
      inherit (effectiveSandbox) profile;
      sandbox = effectiveSandbox;

      # Packages to include in devShell
      packages = [
        scripts
        wrapixBin
      ]
      ++ beadsPackages;

      # Shell hook that symlinks ralph scripts from the source tree for
      # live editing (no direnv reload needed).  Metadata stays in the
      # store since it's computed at build time.
      shellHook = ''
        ${beadsBootstrap}
        ${effectiveSandbox.profile.hostShellHook or ""}
        export PATH="${scripts}/bin:${wrapixBin}/bin:$PATH"
        export RALPH_TEMPLATE_DIR="${templateDir}"
        export RALPH_METADATA_DIR="${scripts}/share/ralph"
        export WRAPIX_PROFILE="${effectiveSandbox.profile.name}"
      '';

      # Nix app definition for `nix run .#ralph`
      app = {
        meta.description = "Ralph Wiggum loop in a sandbox";
        type = "app";
        program = "${pkgs.writeShellScriptBin "ralph-runner" ''
          set -euo pipefail
          ${beadsBootstrap}
          export PATH="${scripts}/bin:${wrapixBin}/bin:$PATH"
          export WRAPIX_PROFILE="${effectiveSandbox.profile.name}"
          exec ralph "$@"
        ''}/bin/ralph-runner";
      };
    };
}
