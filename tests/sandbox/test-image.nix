# Test sandbox image built with `hello` as a stand-in for the
# consumer-supplied claude/pi agents (and for the `beads` overlay), so
# the host-side podman verifiers exercise the real image.nix +
# entrypoint code paths against a minimal closure.
#
# Exposed via the flake as `packages.test-image-base` so verifier shell
# scripts can `nix build .#test-image-base` without re-instantiating the
# profile + image inputs themselves.
{
  pkgs,
  treefmt ? null,
}:

let
  testPkgs = pkgs.extend (_final: _prev: { beads = pkgs.hello; });
  profiles = import ../../lib/sandbox/profiles.nix {
    pkgs = testPkgs;
    inherit treefmt;
  };
in
import ../../lib/sandbox/image.nix {
  pkgs = testPkgs;
  profile = profiles.base;
  entrypointPkg = testPkgs.hello;
  entrypointSh = ../../lib/sandbox/linux/entrypoint.sh;
  claudeConfig = { };
  claudeSettings = { };
}
