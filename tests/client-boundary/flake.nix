{
  inputs.wrix.url = "path:../..";

  outputs =
    { wrix, ... }:
    let
      system = "aarch64-darwin";
      api = wrix.legacyPackages.${system}.lib;
      pkgs = import wrix.inputs.nixpkgs { inherit system; };
      linuxPkgs = import wrix.inputs.nixpkgs { system = "aarch64-linux"; };
      fixtureTool =
        name:
        (pkgs.writeShellScriptBin name ''
          set -euo pipefail
          echo "preparation must not execute host tooling" >&2
          exit 99
        '').overrideAttrs
          {
            allowSubstitutes = true;
            preferLocalBuild = false;
          };
      fixtureController = fixtureTool "wrix-host-controller";
      fixtureRuntime = fixtureTool "container";
      fixtureManagement = pkgs.symlinkJoin {
        name = "boundary-management";
        paths = [
          pkgs.nix
          (fixtureTool "launchctl")
        ];
      };
      profile = api.profiles.base // {
        hostPackages = [ ];
        hostEnv = { };
      };
      project = api.mkSandbox {
        inherit profile;
        agent = "direct";
        agentPkg = linuxPkgs.hello;
      };
    in
    {
      devShells.${system} = {
        default = api.mkDevShell {
          inherit profile;
          prekHooks = false;
          nixCache = false;
        };
        project = project.devShell {
          prekHooks = false;
          nixCache = false;
        };
      };
      packages.${system} = {
        preparation-binding = import (wrix.outPath + "/lib/services/control-binding.nix") {
          inherit pkgs;
          controller = fixtureController;
          runtime = fixtureRuntime;
          management = fixtureManagement;
          image.source = pkgs.writeText "boundary-unrealized-image" "not realized during host preparation";
        };
        client = wrix.packages.${system}.wrix;
        configuration = project.image.profileConfig;
        changed-configuration =
          (api.mkSandbox {
            profile = profile // {
              env.BOUNDARY_FIXTURE = "changed";
            };
            agent = "direct";
            agentPkg = linuxPkgs.hello;
          }).image.profileConfig;
        unrelated-source = pkgs.writeText "unrelated-consumer-source" (builtins.readFile ./consumer.txt);
      };
    };
}
