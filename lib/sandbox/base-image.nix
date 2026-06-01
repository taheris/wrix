# Shared wrapix-base-image: the universal, nixpkgs-pin-dependent bottom of the
# image closure. Profile images chain on top via `fromImage` (see
# specs/image-builder.md § Base Image Layering), so the base's tar loads into the
# platform store once and never re-emits when profile-level inputs change.
#
# Membership rule: a store path belongs here iff its hash depends only on the
# nixpkgs pin, never on a profile-level input. Per-profile content (wrapper
# scripts, merged Claude settings, MCP configs, the agent runtime selection)
# stays in the per-profile top layers.
#
# Always built with `buildLayeredImage` (a tar in the store) on every platform,
# not `streamLayeredImage` (an executable script): only a tar can be consumed as
# the `fromImage` of the per-profile image on both Linux and Darwin.
{
  pkgs,
}:

let
  inherit (pkgs) dockerTools;
in
dockerTools.buildLayeredImage {
  name = "wrapix-base-image";
  tag = "latest";
  maxLayers = 64;

  contents = [
    pkgs.glibc
    pkgs.gcc-unwrapped.lib
    pkgs.llvmPackages.libllvm
    pkgs.openssl
    pkgs.cacert
    pkgs.bashInteractive
    pkgs.coreutils
    pkgs.rustc
  ];
}
