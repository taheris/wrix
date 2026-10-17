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
        inherit
          pkgs
          linuxPkgs
          wrix
          loomCli
          linuxLoomCli
          ;
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

      checks =
        test.checks
        // (import ../../tests/notify/client.nix {
          inherit pkgs linuxPkgs;
          darwinPkgs = inputs.nixpkgs.legacyPackages.aarch64-darwin;
          darwinHostClient = self.packages.aarch64-darwin.wrix-notify-host;
        });
      legacyPackages.ciChecks = test.ciChecks;
      legacyPackages.ciApps = test.ciAppDerivations;
      legacyPackages.verifyScripts = test.verify.scripts;
      legacyPackages.systemTests = test.systemTests;
      legacyPackages.testApps = test.testAppDerivations;
      legacyPackages.testFixtures.execution = test.executionSandbox;
      legacyPackages.testFixtures.native = wrix.rustPackage.testFixtures;
    };
}
