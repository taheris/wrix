{ self, inputs, ... }:

{
  perSystem =
    {
      pkgs,
      system,
      linuxPkgs,
      treefmtWrapper,
      wrapix,
      ...
    }:
    let
      test = import ../../tests {
        inherit
          pkgs
          system
          linuxPkgs
          wrapix
          ;
        treefmt = treefmtWrapper;
        src = self;
        inherit (inputs) fenix;
      };

    in
    {
      _module.args.test = test;

      inherit (test) checks;
    };
}
