{ inputs, ... }:
{
  perSystem =
    { system, ... }:
    let
      wrapixLib = inputs.wrapix.legacyPackages.${system}.lib;
      sandbox = wrapixLib.mkSandbox { profile = wrapixLib.profiles.base; };
    in
    {
      apps.sandbox = {
        type = "app";
        program = "${sandbox.package}/bin/wrapix";
      };
    };
}
