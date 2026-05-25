{ inputs, ... }:

{
  perSystem =
    { system, ... }:
    let
      inherit (inputs) nixpkgs;

      linuxSystem =
        if system == "aarch64-darwin" then
          "aarch64-linux"
        else if system == "x86_64-darwin" then
          "x86_64-linux"
        else
          system;

      beadsPkgs =
        hostPkgs_: linuxPkgs_:
        let
          m = import ../../lib/beads {
            pkgs = hostPkgs_;
            linuxPkgs = linuxPkgs_;
          };
        in
        {
          beads-dolt = m.dolt;
          beads-push = m.push;
        };

      # Protocol version pinning — see specs/loom-tests.md NFR #9.
      #
      # Loom speaks two upstream protocols whose wire formats change only
      # on deliberate version bumps. Both pins live here so a single grep
      # surfaces what versions Loom is compiled against:
      #
      #   pi-mono     — 0.72.1, pinned in lib/pi-mono/package.json
      #                 (RPC framing for the pi backend)
      #   claude-code — tracks nixos-unstable via flake.lock
      #                 (stream-json framing for the claude backend);
      #                 pinPolicy = "nixpkgs" — bumping nixpkgs may bump
      #                 claude-code's wire surface.
      #
      # Protocol-bump checklist (run on every pi-mono OR claude-code bump,
      # including nixpkgs bumps that move claude-code):
      #   1. `cargo nextest run -p loom-agent` — parser tests assert on
      #      every documented field, so renamed/dropped fields fail loudly.
      #   2. Scan upstream changelog (pi-mono / claude-code releases) for
      #      new event types or message variants.
      #   3. If new event types exist, ensure they map to typed variants
      #      OR are caught by `Unknown` (`#[serde(other)]`) — extend
      #      `Unknown`-coverage tests.
      #   4. If new event types reach pipe-level paths (probe, steer,
      #      compaction, set_model, agent_end), update the matching mode
      #      in tests/loom/mock-pi/pi.sh or tests/loom/mock-claude/claude.sh.
      #   5. No live wire tests — verification stays at the parser layer.
      linuxOverlay =
        final: _prev:
        {
          pi-mono = final.callPackage ../../lib/pi-mono { };
        }
        // beadsPkgs final final;

      linuxPkgs = import nixpkgs {
        system = linuxSystem;
        overlays = [ linuxOverlay ];
        config.allowUnfree = true;
      };

      hostOverlay =
        final: _prev:
        {
          inherit (linuxPkgs) pi-mono;
        }
        // beadsPkgs final linuxPkgs;

    in
    {
      _module.args.pkgs = import nixpkgs {
        inherit system;
        overlays = [ hostOverlay ];
        config.allowUnfree = true;
      };

      _module.args.linuxPkgs = linuxPkgs;
    };
}
