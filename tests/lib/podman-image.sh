#!/usr/bin/env bash

wrapix_test_image_refs() {
  local image_name="$1"
  printf '%s\n' \
    "$image_name:latest" \
    "localhost/$image_name:latest" \
    "docker.io/library/$image_name:latest"
}

wrapix_remove_test_image_refs() {
  local target_ref="$2"

  if podman image exists "$target_ref"; then
    podman rmi "$target_ref" >/dev/null
  fi
}

wrapix_tag_loaded_image_id() {
  local image_name="$1"
  local target_ref="$2"
  local ref loaded_id

  while IFS= read -r ref; do
    if loaded_id=$(podman image inspect --format '{{.Id}}' "$ref" 2>/dev/null); then
      [[ -n "$loaded_id" ]] || continue
      podman tag "$loaded_id" "$target_ref"
      return 0
    fi
  done < <(wrapix_test_image_refs "$image_name")

  echo "FAIL: image $image_name not found after podman load" >&2
  podman images >&2
  return 1
}
