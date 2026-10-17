#!/usr/bin/env bash
set -euo pipefail

if ! root=$(realpath -- "${1:?image root is required}" 2>/dev/null) \
  || [[ "$root" == / || "$root" == /nix || "$root" == /nix/var || "$root" == /nix/var/nix \
  || "$root" == /nix/store || "$root" == /nix/store/* \
  || ! -f "$root/etc/wrix/private-nix-version" || -L "$root/etc/wrix/private-nix-version" ]] \
  || [[ "$(<"$root/etc/wrix/private-nix-version")" != 1 ]]; then
  printf 'private Nix initialization requires a staged image root\n' >&2
  exit 1
fi
cd "$root"

# Do not let a staged path redirect initialization into another tree.
for path in nix nix/store nix/var nix/var/nix nix/var/nix/db \
  nix/var/nix/temproots nix/var/nix/builds nix/var/nix/gc.lock \
  nix/var/nix/profiles nix/var/nix/profiles/per-user nix/var/nix/profiles/per-user/wrix \
  nix/var/nix/gcroots nix/var/nix/gcroots/per-user nix/var/nix/gcroots/per-user/wrix \
  nix/var/nix/gcroots/auto nix/var/log nix/var/log/nix nix/var/log/nix/drvs; do
  if [[ -L "$path" ]]; then
    printf 'private Nix initialization rejects staged symlinks: %s\n' "$path" >&2
    exit 1
  fi
done

mkdir -p nix/store nix/var/nix/db nix/var/nix/temproots nix/var/nix/builds \
  nix/var/nix/profiles/per-user/wrix nix/var/nix/gcroots/per-user/wrix \
  nix/var/nix/gcroots/auto nix/var/log/nix/drvs
chown 0:0 nix nix/var nix/var/nix \
  nix/var/nix/profiles nix/var/nix/profiles/per-user \
  nix/var/nix/gcroots nix/var/nix/gcroots/per-user nix/var/nix/gcroots/auto
chmod 0755 nix nix/var nix/var/nix \
  nix/var/nix/profiles nix/var/nix/profiles/per-user \
  nix/var/nix/gcroots nix/var/nix/gcroots/per-user

# This runs only on the staged customization tree under dockerTools' fakeroot.
chown 1000:1000 nix/store nix/var/nix/builds \
  nix/var/nix/profiles/per-user/wrix nix/var/nix/gcroots/per-user/wrix
chmod 0755 nix/store nix/var/nix/builds \
  nix/var/nix/profiles/per-user/wrix nix/var/nix/gcroots/per-user/wrix
chown -R 1000:1000 nix/var/nix/db nix/var/nix/temproots nix/var/log/nix
chmod -R u+rwX nix/var/nix/db nix/var/nix/temproots nix/var/log/nix
chmod 0700 nix/var/nix/db nix/var/nix/temproots
chmod 1777 nix/var/nix/gcroots/auto

# Nix opens this lock without creating it in the protected state ancestor.
touch nix/var/nix/gc.lock
chown 1000:1000 nix/var/nix/gc.lock
chmod 0600 nix/var/nix/gc.lock
