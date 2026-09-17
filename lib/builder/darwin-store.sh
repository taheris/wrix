#!/usr/bin/env bash

wrix_builder_nix_volume_exists() {
  container volume inspect "$NIX_VOLUME" >/dev/null 2>&1
}

wrix_builder_nix_volume_is_managed() {
  container volume inspect "$NIX_VOLUME" 2>/dev/null | "$WRIX_BUILDER_JQ" -e '
    ((if type == "array" then .[0] else . end) // {})
    | .configuration.labels["wrix.managed"] == "true"
      and .configuration.labels["wrix.volume.kind"] == "builder-nix"
  ' >/dev/null
}

wrix_builder_create_nix_volume() {
  container volume create \
    --label "wrix.managed=true" \
    --label "wrix.volume.kind=builder-nix" \
    -s "$NIX_VOLUME_SIZE" \
    "$NIX_VOLUME" >/dev/null
}

wrix_builder_remove_nix_volume() {
  if ! wrix_builder_nix_volume_exists; then
    return 0
  fi
  if ! wrix_builder_nix_volume_is_managed; then
    echo "Error: refusing to delete unmanaged volume $NIX_VOLUME" >&2
    return 1
  fi
  container volume delete "$NIX_VOLUME" >/dev/null
}

wrix_builder_seed_nix_volume() {
  local seed_container="$1"

  if ! "$WRIX_BUILDER_STORE_EXPORT" | container run \
    --rm \
    -i \
    --name "$seed_container" \
    -v "$NIX_VOLUME:/persistent-root/nix" \
    --entrypoint /bin/sh \
    "$BUILDER_IMAGE" \
    -c '
      set -euo pipefail
      export NIX_CONFIG="build-users-group ="
      nix-store --store /persistent-root --import >/dev/null
      mkdir -p \
        /persistent-root/nix/var/log/nix/drvs \
        /persistent-root/nix/var/nix/gcroots/per-user \
        /persistent-root/nix/var/nix/profiles/per-user
      chmod 755 \
        /persistent-root/nix/var/nix/gcroots \
        /persistent-root/nix/var/nix/gcroots/per-user \
        /persistent-root/nix/var/nix/profiles \
        /persistent-root/nix/var/nix/profiles/per-user
      chmod -R a+rwX /persistent-root/nix/var/log
      nix-store --store /persistent-root --verify --check-contents
    '; then
    echo "Error: Failed to import and verify the persistent Nix store" >&2
    wrix_builder_remove_nix_volume
    return 1
  fi
}
