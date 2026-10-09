{ self, inputs, ... }:

{
  perSystem =
    {
      config,
      pkgs,
      system,
      linuxPkgs,
      treefmtWrapper,
      wrix,
      loomCli,
      linuxLoomCli,
      ...
    }:
    let
      loomTests = import ../../tests/loom.nix {
        inherit pkgs loomCli linuxLoomCli;
        inherit (config) packages;
        devShell = config.devShells.default;
        lock = builtins.fromJSON (builtins.readFile ../../flake.lock);
      };
      test = import ../../tests {
        inherit loomTests;
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
      _module.args = {
        inherit test;
      };

      inherit (test) checks;
      legacyPackages.ciChecks = test.ciChecks;
      legacyPackages.ciApps = test.ciAppDerivations;
      legacyPackages.systemTests = test.systemTests;
      legacyPackages.testApps = test.testAppDerivations;
      legacyPackages.testFixtures.execution = test.executionSandbox;
    };
}
