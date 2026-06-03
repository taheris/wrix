# Test sandbox image built with `hello` as a stand-in for the
# consumer-supplied claude/pi agents (and for the `beads` overlay), so
# the host-side podman verifiers exercise the real image.nix +
# entrypoint code paths against a minimal closure.
#
# Exposed via the flake as `packages.test-image-base` so verifier shell
# scripts can `nix build .#test-image-base` without re-instantiating the
# profile + image inputs themselves.
#
# `claudeConfig` is forwarded straight through to image.nix and only
# affects the streamLayeredImage customisation layer (top of closure):
# perturbing it lets the install-delta verifier build two images that
# share every base-layer blob and differ only in the top-of-closure
# layer, exercising the launcher's per-blob-dedup install transport.
#
# `shipNix` adds `pkgs.nix` to the profile's `packages` (a nix-shipping
# profile, as `image-builder.md` § Hook Installation describes; bead-use
# images omit nix by default). The leaf renames to `nix` so the resulting
# image tags as `wrapix-nix` and does not collide with the base variant in
# the platform store. `tests/sandbox/nix-in-container.sh` builds this
# variant to exercise live additive in-container Nix as the unprivileged
# runtime user (specs/sandbox.md FR #13).
{
  pkgs,
  treefmt ? null,
  claudeConfig ? { },
  shipNix ? false,
}:

let
  testPkgs = pkgs.extend (_final: _prev: { beads = pkgs.hello; });
  profiles = import ../../lib/sandbox/profiles.nix {
    pkgs = testPkgs;
    inherit treefmt;
  };
  profile =
    if shipNix then
      profiles.base
      // {
        name = "nix";
        packages = (profiles.base.packages or [ ]) ++ [ testPkgs.nix ];
      }
    else
      profiles.base;
in
import ../../lib/sandbox/image.nix {
  pkgs = testPkgs;
  inherit profile;
  entrypointPkg = testPkgs.hello;
  entrypointSh = ../../lib/sandbox/linux/entrypoint.sh;
  inherit claudeConfig;
  claudeSettings = { };
}
