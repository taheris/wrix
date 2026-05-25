{ inputs, ... }:

{
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
        inherit (inputs) crane fenix;
        treefmt = treefmtWrapper;
      };

    in
    {
      _module.args = {
        inherit wrapix;
      };

      legacyPackages.lib = {
        inherit (wrapix)
          deriveProfile
          mkDevShell
          mkProfileImages
          mkRalph
          mkSandbox
          profiles
          ;
      };
    };
}
