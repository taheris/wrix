#!/bin/sh
# shellcheck disable=SC3040
set -euo pipefail

store_root="$1"
runtime_root="$2"
export NIX_CONFIG="build-users-group =
min-free = 0"

nix-store --store "$store_root" --import >/dev/null
nix-store --store "$store_root" --check-validity "$runtime_root"
mkdir -p \
  "$store_root/nix/var/log/nix/drvs" \
  "$store_root/nix/var/nix/gcroots/per-user" \
  "$store_root/nix/var/nix/profiles/per-user"
chmod 755 \
  "$store_root/nix/var/nix/gcroots" \
  "$store_root/nix/var/nix/gcroots/per-user" \
  "$store_root/nix/var/nix/profiles" \
  "$store_root/nix/var/nix/profiles/per-user"
chmod -R a+rwX "$store_root/nix/var/log"

# Use logical /nix/store paths: the volume is mounted at /nix during normal operation.
gc_root="$store_root/nix/var/nix/gcroots/wrix-builder-runtime"
ln -sfn "$runtime_root" "$gc_root.$$"
mv -Tf "$gc_root.$$" "$gc_root"
nix-store --store "$store_root" --verify --check-contents
