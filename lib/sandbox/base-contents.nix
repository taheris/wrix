# Universal bottom-of-closure for `wrix-base-image` (specs/image-builder.md
# § Base Image Layering). Membership: a path belongs here iff it varies only
# with the nixpkgs pin AND is genuinely shared — a library or runtime every
# profile already closes over, not a profile-specific leaf. Profile-specific
# toolchains (e.g. `pkgs.rustc`; the rust profile uses fenix's toolchain) stay
# out. Shared so the membership verifier can close over the real list.
{
  pkgs,
}:

[
  pkgs.glibc
  pkgs.getent.provider
  pkgs.gcc-unwrapped.lib
  pkgs.openssl.out
  pkgs.cacert
  pkgs.bashInteractive
  pkgs.coreutils
]
