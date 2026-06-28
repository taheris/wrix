#!/usr/bin/env bash
set -euo pipefail

wrix_unique_image_ref() {
  local image_name="$1"

  printf 'localhost/%s-%s:latest\n' "$image_name" "$$"
}

wrix_remove_test_image_ref() {
  local target_ref="$1"

  if podman image exists "$target_ref"; then
    podman rmi "$target_ref" >/dev/null
  fi
}

wrix_loaded_image_refs() {
  sed -n 's/^Loaded image(s): //p; s/^Loaded image: //p' \
    | tr ',' '\n' \
    | sed 's/^[[:space:]]*//; s/[[:space:]]*$//'
}

wrix_image_short_name() {
  local ref="$1"
  local short_name

  short_name="${ref##*/}"
  printf '%s\n' "${short_name%%:*}"
}

wrix_loaded_ref_for_image() {
  local image_name="$1"
  local load_out="$2"
  local ref

  while IFS= read -r ref; do
    [[ -n "$ref" ]] || continue
    if [[ "$(wrix_image_short_name "$ref")" == "$image_name" ]]; then
      printf '%s\n' "$ref"
      return 0
    fi
  done < <(printf '%s\n' "$load_out" | wrix_loaded_image_refs)

  return 1
}

wrix_is_nix_descriptor() {
  local image_source="$1"

  [[ -f "$image_source" ]] || return 1
  jq -e 'type == "object" and (.oci_layout | type == "string") and (.digest | type == "string")' \
    "$image_source" >/dev/null 2>&1
}

wrix_containers_storage_ref() {
  local target_ref="$1"
  local store_ref store_spec

  store_ref="containers-storage:$target_ref"
  store_spec=$(podman info \
    --format '{{.Store.GraphDriverName}}@{{.Store.GraphRoot}}+{{.Store.RunRoot}}' \
    2>/dev/null) || store_spec=""
  if [[ "$store_spec" == *@*+* ]]; then
    store_ref="containers-storage:[$store_spec]$target_ref"
  fi

  printf '%s\n' "$store_ref"
}

wrix_load_descriptor_image() {
  local image_source="$1"
  local target_ref="$2"
  local oci_layout oci_ref store_ref loaded_id

  command -v skopeo >/dev/null 2>&1 || {
    echo "SKIP: skopeo not on PATH" >&2
    return 77
  }

  oci_layout=$(jq -er '.oci_layout | strings | select(length > 0)' "$image_source")
  oci_ref=$(jq -er '.oci_ref // "latest" | strings | select(length > 0)' "$image_source")
  store_ref=$(wrix_containers_storage_ref "$target_ref")

  if ! skopeo --insecure-policy copy --quiet "oci:$oci_layout:$oci_ref" "$store_ref"; then
    echo "FAIL: skopeo copy nix descriptor image" >&2
    return 1
  fi

  loaded_id=$(podman image inspect --format '{{.Id}}' "$target_ref")
  if [[ -z "$loaded_id" ]]; then
    echo "FAIL: descriptor image $target_ref has no image ID" >&2
    return 1
  fi
}

wrix_load_stream_image() {
  local image_stream="$1"
  local image_name="$2"
  local target_ref="$3"
  local stream_log load_out loaded_ref loaded_id

  stream_log=$(mktemp -t wrix-podman-load.XXXXXX)
  if ! load_out=$("$image_stream" 2>"$stream_log" | podman load 2>&1); then
    cat "$stream_log" >&2
    rm -f "$stream_log"
    echo "FAIL: podman load" >&2
    printf '%s\n' "$load_out" >&2
    return 1
  fi
  rm -f "$stream_log"

  if ! loaded_ref=$(wrix_loaded_ref_for_image "$image_name" "$load_out"); then
    echo "FAIL: could not determine loaded image ref for $image_name" >&2
    printf '%s\n' "$load_out" >&2
    return 1
  fi

  loaded_id=$(podman image inspect --format '{{.Id}}' "$loaded_ref")
  if [[ -z "$loaded_id" ]]; then
    echo "FAIL: loaded image $loaded_ref has no image ID" >&2
    return 1
  fi

  podman tag "$loaded_id" "$target_ref" >/dev/null
}

wrix_load_test_image() {
  local image_source="$1"
  local image_name="$2"
  local target_ref="$3"

  wrix_remove_test_image_ref "$target_ref"

  if wrix_is_nix_descriptor "$image_source"; then
    wrix_load_descriptor_image "$image_source" "$target_ref"
  else
    wrix_load_stream_image "$image_source" "$image_name" "$target_ref"
  fi
}
