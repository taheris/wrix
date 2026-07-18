{ self, inputs, ... }:

{
  perSystem =
    {
      pkgs,
      system,
      linuxPkgs,
      treefmtWrapper,
      wrix,
      ...
    }:
    let
      test = import ../../tests {
        inherit
          pkgs
          system
          linuxPkgs
          wrix
          ;
        treefmt = treefmtWrapper;
        src = self;
        inherit (inputs) crane fenix;
      };

    in
    {
      _module.args.test = test;

      inherit (test) checks;
      legacyPackages.ciChecks = test.ciChecks;
      legacyPackages.ciApps = test.ciAppDerivations;
      legacyPackages.systemTests = test.systemTests;
      legacyPackages.testApps = test.testAppDerivations;
    };
}
