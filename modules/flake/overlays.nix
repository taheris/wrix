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

      linuxPkgs = import nixpkgs {
        system = linuxSystem;
        config.allowUnfree = true;
      };

      hostOverlay =
        _final: prev:
        prev.lib.optionalAttrs prev.stdenv.hostPlatform.isDarwin {
          beads = prev.beads.overrideAttrs (old: {
            # Darwin's orphan-server test needs process discovery tools on PATH.
            nativeCheckInputs = (old.nativeCheckInputs or [ ]) ++ [
              prev.ps
              prev.lsof
            ];
          });
        };

    in
    {
      _module.args = {
        inherit linuxPkgs;
        pkgs = import nixpkgs {
          inherit system;
          overlays = [
            hostOverlay
          ];
          config.allowUnfree = true;
        };
      };
    };
}
