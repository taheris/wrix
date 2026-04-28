{ inputs, ... }:

{
  flake.nixosModules = {
    city = ../nixos/city.nix;
  };

  perSystem =
    {
      pkgs,
      system,
      linuxPkgs,
      treefmtWrapper,
      ...
    }:
    let
      wrapix = import ../../lib {
        inherit pkgs system linuxPkgs;
        inherit (inputs) fenix;
        treefmt = treefmtWrapper;
      };

    in
    {
      _module.args = {
        inherit wrapix;
        city = wrapix.mkCity { name = "wx"; };
      };

      legacyPackages.lib = {
        inherit (wrapix)
          deriveProfile
          mkCity
          mkDevShell
          mkRalph
          mkSandbox
          profiles
          ;
      };
    };
}
