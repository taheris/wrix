{ pkgs }:

let
  hooks = import ./bundle.nix { inherit pkgs; };
  wrappers = import ./wrappers.nix { inherit pkgs; };

in
{
  inherit (wrappers) prePushChecks skipIfMissing;

  prekHooks = hooks;
}
