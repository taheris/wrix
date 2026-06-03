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

      # Agent runtime packages exposed to consumers:
      #
      #   claude-code — tracks nixos-unstable via flake.lock
      #                 (stream-json framing for the claude backend);
      #                 bumping nixpkgs may bump claude-code's wire surface.
      linuxOverlay = final: _prev: beadsPkgs final final;

      linuxPkgs = import nixpkgs {
        system = linuxSystem;
        overlays = [
          linuxOverlay
        ];
        config.allowUnfree = true;
      };

      hostOverlay = final: _prev: beadsPkgs final linuxPkgs;

    in
    {
      _module.args.pkgs = import nixpkgs {
        inherit system;
        overlays = [
          hostOverlay
        ];
        config.allowUnfree = true;
      };

      _module.args.linuxPkgs = linuxPkgs;
    };
}
