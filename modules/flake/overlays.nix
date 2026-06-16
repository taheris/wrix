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

      # Agent runtime packages exposed to consumers:
      #
      #   claude-code — tracks nixos-unstable via flake.lock
      #                 (stream-json framing for the claude backend);
      #                 bumping nixpkgs may bump claude-code's wire surface.
      linuxOverlay = _final: _prev: { };

      linuxPkgs = import nixpkgs {
        system = linuxSystem;
        overlays = [
          linuxOverlay
        ];
        config.allowUnfree = true;
      };

      hostOverlay = _final: _prev: { };

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
