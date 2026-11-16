{ pkgs }:
{
  version = 1;
  package = import ../util/rust-tool.nix {
    inherit pkgs;
    package = "wrix-runtime-env";
    scopedSource = true;
    description = "Private worker environment delivery";
  };
}
