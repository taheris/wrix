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
      wrix = import ../../lib {
        inherit pkgs system linuxPkgs;
        inherit (inputs) crane fenix;
        treefmt = treefmtWrapper;
      };

    in
    {
      _module.args = {
        inherit wrix;
      };

      legacyPackages.lib = {
        inherit (wrix)
          deriveProfile
          mkDevShell
          mkProfileImages
          mkSandbox
          prekHooks
          profiles
          rustProfile
          ;
      };
    };
}
