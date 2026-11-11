{ pkgs }:
let
  tools = import ../../lib/verify/tools.nix { inherit pkgs; };
in
tools.overrideAttrs (old: {
  pname = "native-private-nix-initializer-owner";
  nativeCheckInputs = (old.nativeCheckInputs or [ ]) ++ [
    pkgs.fakeroot
    pkgs.bash
    pkgs.coreutils
  ];
  cargoTestFlags = [
    "--package"
    "wrix-verify-tools"
    "--test"
    "private_nix_initializer"
  ];
  checkFlags = [
    "--nocapture"
    "--ignored"
    "--exact"
    "test::native_private_nix_initialization"
  ];
})
