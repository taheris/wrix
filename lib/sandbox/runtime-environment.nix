{ pkgs }:
{
  version = 1;
  package = import ../util/rust-tool.nix {
    inherit pkgs;
    package = "wrix-runtime-env";
    description = "Private worker environment delivery";
  };
}
