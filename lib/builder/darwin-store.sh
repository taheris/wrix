#!/usr/bin/env bash
set -euo pipefail

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

wrix_builder_seed_nix_volume() {
  local seed_container="$1"

  if ! "$WRIX_BUILDER_STORE_EXPORT" | container run \
    --rm \
    -i \
    --name "$seed_container" \
    -v "$NIX_VOLUME:/persistent-root/nix" \
    --entrypoint /bin/sh \
    "$BUILDER_IMAGE" \
    -c "$(cat "$WRIX_BUILDER_STORE_IMPORT")" \
    wrix-builder-import /persistent-root "$BUILDER_RUNTIME_ROOT"; then
    echo "Error: Failed to import and verify the persistent Nix store; volume preserved" >&2
    return 1
  fi
}
