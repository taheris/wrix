#!/usr/bin/env bash
set -euo pipefail

wrix_builder_ssh_keygen() {
  local keygen="${WRIX_BUILDER_SSH_KEYGEN:-ssh-keygen}"
  "$keygen" "$@"
}

wrix_builder_base64() {
  local base64_bin="${WRIX_BUILDER_BASE64:-base64}"
  "$base64_bin"
}

wrix_builder_ensure_key_pair() {
  local private_key="$1"
  local comment="$2"
  local public_key="$private_key.pub"

  if [[ -f "$private_key" ]]; then
    chmod 600 "$private_key"
    if [[ ! -f "$public_key" || "$private_key" -nt "$public_key" ]]; then
      wrix_builder_ssh_keygen -y -f "$private_key" > "$public_key"
    fi
    chmod 644 "$public_key"
    return 0
  fi

  rm -f "$public_key"
  wrix_builder_ssh_keygen -q -t ed25519 -f "$private_key" -N "" -C "$comment" </dev/null
  chmod 600 "$private_key"
  chmod 644 "$public_key"
}

wrix_builder_write_host_key_metadata() {
  local host_public_key="$1"
  local output_path="$2"

  wrix_builder_base64 < "$host_public_key" | tr -d '\n' > "$output_path"
  chmod 644 "$output_path"
}

wrix_builder_ensure_key_material() {
  local keys_dir="$1"
  local ssh_port="$2"
  local host_key="$keys_dir/host_ed25519"
  local client_key="$keys_dir/client_ed25519"
  local known_hosts="$keys_dir/known_hosts"
  local public_host_key_base64="$keys_dir/public_host_key_base64"
  local host_public_key

  mkdir -p "$keys_dir"
  chmod 700 "$keys_dir"

  wrix_builder_ensure_key_pair "$host_key" "wrix-builder-host"
  wrix_builder_ensure_key_pair "$client_key" "wrix-builder-client"

  host_public_key=$(cat "$host_key.pub")
  printf '[localhost]:%s %s\n' "$ssh_port" "$host_public_key" > "$known_hosts"
  chmod 600 "$known_hosts"

  wrix_builder_write_host_key_metadata "$host_key.pub" "$public_host_key_base64"
}

wrix_builder_require_key_material() {
  local keys_dir="$1"
  local path
  local missing=0

  for path in \
    "$keys_dir/host_ed25519" \
    "$keys_dir/host_ed25519.pub" \
    "$keys_dir/client_ed25519" \
    "$keys_dir/client_ed25519.pub" \
    "$keys_dir/public_host_key_base64"; do
    if [[ ! -f "$path" ]]; then
      printf 'Error: missing builder SSH key material: %s\n' "$path" >&2
      missing=1
    fi
  done

  if [[ "$missing" -ne 0 ]]; then
    printf '%s\n' "Run 'wrix-builder start' as the builder host user to create keys." >&2
    return 1
  fi
}
