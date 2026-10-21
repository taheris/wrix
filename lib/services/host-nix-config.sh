#!/usr/bin/env bash
set -euo pipefail

wrix_host_fail() {
  local message="$1"
  printf 'wrix: project Nix cache is not active: %s\n' "$message" >&2
  printf 'wrix: add this user or a wrix group to trusted-users and restart the Nix daemon, or set cache.enable = false in wrix.toml (nixCache = false also opts out).\n' >&2
  return 1
}

wrix_host_json_string() {
  local json="$1"
  local key="$2"
  printf '%s\n' "$json" | awk -v key="$key" '
    $0 ~ "\"" key "\"" {
      value = $0
      sub(".*\"" key "\"[[:space:]]*:[[:space:]]*\"", "", value)
      sub("\".*", "", value)
      print value
      exit
    }
  '
}

wrix_host_json_boolean() {
  local json="$1"
  local key="$2"
  printf '%s\n' "$json" | awk -v key="$key" '
    $0 ~ "^[[:space:]]*\"" key "\"[[:space:]]*:" {
      value = $0
      sub(".*:[[:space:]]*", "", value)
      sub("[,[:space:]]*$", "", value)
      print value
      exit
    }
  '
}

wrix_host_require_value() {
  local name="$1"
  local value="$2"
  if [[ -z "$value" ]]; then
    wrix_host_fail "missing $name in service endpoint metadata"
    return 1
  fi
}

wrix_host_require_public_key() {
  local public_key="$1"
  if [[ -z "$public_key" ]]; then
    wrix_host_fail "missing cache public key in service state"
    return 1
  fi
  if ! [[ "$public_key" =~ ^[^:]+:[A-Za-z0-9+/]{43}=$ ]]; then
    wrix_host_fail "invalid cache public key in service state; run env -u NIX_CONFIG wrix service cache rotate-key"
    return 1
  fi
}

wrix_host_config_snapshot() {
  local nix_bin="${WRIX_NIX_BIN:-nix}"
  if "$nix_bin" config show 2>/dev/null; then # compatibility: older Nix rejects the config subcommand.
    return 0
  fi
  "$nix_bin" show-config 2>/dev/null # compatibility: newer Nix may omit the legacy alias.
}

wrix_host_config_line_value() {
  local config="$1"
  local key="$2"
  printf '%s\n' "$config" | awk -F '=' -v key="$key" '
    $1 ~ "^[[:space:]]*" key "[[:space:]]*$" {
      value = $2
      sub("^[[:space:]]*", "", value)
      sub("[[:space:]]*$", "", value)
      print value
      exit
    }
  '
}

wrix_host_assert_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    wrix_host_fail "host Nix does not honor $label ($needle)"
    return 1
  fi
}

wrix_host_assert_line_equals() {
  local label="$1"
  local config="$2"
  local key="$3"
  local expected="$4"
  local actual
  actual="$(wrix_host_config_line_value "$config" "$key")"
  if [[ "$actual" != "$expected" ]]; then
    wrix_host_fail "host Nix does not honor $label"
    return 1
  fi
}

wrix_host_reject_conflicting_hook() {
  local config="$1"
  local existing
  existing="$(wrix_host_config_line_value "$config" "post-build-hook")"
  if [[ -n "$existing" && "$existing" != *"wrix-cache"* ]]; then
    wrix_host_fail "existing non-wrix post-build-hook conflicts: $existing"
    return 1
  fi
}

wrix_host_write_hook_source() {
  local hook_dir="$1"
  local hook_bin="$2"
  local publish_bin="$3"
  local workspace_hash="$4"
  local owner_uid="$5"
  local owner_gid="$6"
  local state_root="$7"
  local cache_root="$8"
  local manifest_path="$9"
  local cache_config="${10}"
  local home_word state_home_word cache_home_word
  printf -v home_word '%q' "$HOME"
  printf -v state_home_word '%q' "${XDG_STATE_HOME:-$HOME/.local/state}"
  printf -v cache_home_word '%q' "${XDG_CACHE_HOME:-$HOME/.cache}"
  mkdir -p "$hook_dir/bin"
  printf '%s\n' "$cache_config" >"$hook_dir/cache-config.json"
  chmod 0444 "$hook_dir/cache-config.json"
  cat >"$hook_dir/bin/wrix-cache-post-build-hook" <<EOF
#!${WRIX_BASH_BIN:-/usr/bin/env bash}
set -euo pipefail
export HOME=$home_word
export XDG_STATE_HOME=$state_home_word
export XDG_CACHE_HOME=$cache_home_word
exec "$hook_bin" \\
  --workspace-hash "$workspace_hash" \\
  --owner-uid "$owner_uid" \\
  --owner-gid "$owner_gid" \\
  --state-root "$state_root" \\
  --cache-root "$cache_root" \\
  --manifest "$manifest_path" \\
  --publisher-helper "$publish_bin" \\
  --config "\${BASH_SOURCE[0]%/*}/../cache-config.json"
EOF
  chmod 0555 "$hook_dir/bin/wrix-cache-post-build-hook"
}

wrix_host_store_hook() {
  local hook_dir="$1"
  local nix_store_bin="${WRIX_NIX_STORE_BIN:-nix-store}"
  "$nix_store_bin" --add-fixed sha256 --recursive "$hook_dir"
}

wrix_host_retain_hook() {
  local root_dir="$1"
  local hook_store="$2"
  local hook_bin="$3"
  local publish_bin="$4"
  local nix_store_bin="${WRIX_NIX_STORE_BIN:-nix-store}"
  local label binary store_path
  mkdir -p "$root_dir"
  "$nix_store_bin" --add-root "$root_dir/wrapper" --indirect --realise "$hook_store" >/dev/null || return 1
  # Fixed-output imports do not record references to the wrapper's runtime.
  for label in hook publisher bash; do
    case "$label" in
      hook) binary="$hook_bin" ;;
      publisher) binary="$publish_bin" ;;
      bash) binary="${WRIX_BASH_BIN:-/usr/bin/env bash}" ;;
    esac
    if [[ "$binary" != /* ]]; then
      binary="$(command -v "$binary")" || return 1
    fi
    case "$binary" in
      /nix/store/*)
        store_path="${binary#/nix/store/}"
        store_path="/nix/store/${store_path%%/*}"
        "$nix_store_bin" --add-root "$root_dir/$label" --indirect --realise "$store_path" >/dev/null || return 1
        ;;
    esac
  done
}

wrix_host_append_nix_config() {
  local cache_root="$1"
  local public_key="$2"
  local hook_path="$3"
  local addition base_config
  addition="extra-substituters = file://$cache_root
extra-trusted-public-keys = $public_key
builders-use-substitutes = true
post-build-hook = $hook_path"
  base_config="${NIX_CONFIG:-}"
  if [[ -n "$base_config" ]]; then
    base_config="$(printf '%s\n' "$base_config" | awk '
      !($0 ~ /^[[:space:]]*post-build-hook[[:space:]]*=/ && $0 ~ /wrix-cache/)
    ')"
  fi
  if [[ -n "$base_config" ]]; then
    export NIX_CONFIG="$base_config
$addition"
  else
    export NIX_CONFIG="$addition"
  fi
}

wrix_host_nix_config_main() {
  local service_bin="${WRIX_SERVICE_BIN:-wrix}"
  local hook_bin="${WRIX_CACHE_HOOK_BIN:-wrix-cache-hook}"
  local publish_bin="${WRIX_CACHE_PUBLISH_BIN:-wrix-cache-publish}"
  local require_trusted cache_config cache_status
  local endpoints workspace_hash state_root cache_root public_key manifest_path hook_build_root hook_store hook_path before_config after_config owner_uid owner_gid

  command -v "${WRIX_NIX_BIN:-nix}" >/dev/null 2>&1 || { wrix_host_fail "nix is not on PATH"; return 1; }
  command -v "${WRIX_NIX_STORE_BIN:-nix-store}" >/dev/null 2>&1 || { wrix_host_fail "nix-store is not on PATH"; return 1; }

  cache_config="$("$service_bin" service cache config)" || return 1
  require_trusted="$(wrix_host_json_boolean "$cache_config" require_trusted_nix)"
  if [[ "$require_trusted" != "true" && "$require_trusted" != "false" ]]; then
    wrix_host_fail "missing require_trusted_nix in resolved cache configuration"
    return 1
  fi

  endpoints="$("$service_bin" service endpoints)" || return 1
  if printf '%s\n' "$endpoints" | grep -q '"cache_http":[[:space:]]*null'; then
    if [[ "${WRIX_HOST_NIX_CONFIG_PRINT:-0}" == "1" ]]; then
      printf '%s\n' "${NIX_CONFIG:-}"
    fi
    return 0
  fi

  before_config="$(wrix_host_config_snapshot)" || return 1
  if [[ "$require_trusted" == "true" ]]; then
    wrix_host_reject_conflicting_hook "$before_config" || return 1
  fi
  workspace_hash="$(wrix_host_json_string "$endpoints" "workspace_hash")"
  state_root="$(wrix_host_json_string "$endpoints" "state_root")"
  cache_root="$(wrix_host_json_string "$endpoints" "cache_root")"
  wrix_host_require_value "workspace_hash" "$workspace_hash" || return 1
  wrix_host_require_value "state_root" "$state_root" || return 1
  wrix_host_require_value "cache_root" "$cache_root" || return 1

  public_key="$(tr -d '\n' <"$state_root/keys/cache.pub")"
  wrix_host_require_public_key "$public_key" || return 1
  manifest_path="$state_root/publish-roots.json"
  owner_uid="$(id -u)"
  owner_gid="$(id -g)"

  hook_build_root="$state_root/hook-wrapper"
  rm -rf "$hook_build_root"
  wrix_host_write_hook_source "$hook_build_root" "$hook_bin" "$publish_bin" "$workspace_hash" "$owner_uid" "$owner_gid" "$state_root" "$cache_root" "$manifest_path" "$cache_config" || return 1
  hook_store="$(wrix_host_store_hook "$hook_build_root")" || return 1
  wrix_host_retain_hook "$state_root/host-nix-roots" "$hook_store" "$hook_bin" "$publish_bin" || return 1
  hook_path="$hook_store/bin/wrix-cache-post-build-hook"

  wrix_host_append_nix_config "$cache_root" "$public_key" "$hook_path"
  after_config="$(wrix_host_config_snapshot)" || return 1

  if [[ "$require_trusted" == "true" ]]; then
    wrix_host_assert_contains "project cache substituter" "$after_config" "file://$cache_root" || return 1
    wrix_host_assert_contains "project cache public key" "$after_config" "$public_key" || return 1
    wrix_host_assert_line_equals "builders-use-substitutes" "$after_config" "builders-use-substitutes" "true" || return 1
    wrix_host_assert_line_equals "post-build-hook" "$after_config" "post-build-hook" "$hook_path" || return 1
  fi

  if [[ "${WRIX_NIX_CACHE_REMINDER:-1}" != "0" ]]; then
    cache_status="$("$service_bin" service cache status)" || return 1
    if printf '%s\n' "$cache_status" | grep -Fxq 'publication roots: 0'; then
      printf 'wrix: project cache publish manifest is empty; run wrix service cache publish to refresh it.\n' >&2
    fi
  fi

  if [[ "${WRIX_HOST_NIX_CONFIG_PRINT:-0}" == "1" ]]; then
    printf '%s\n' "$NIX_CONFIG"
  fi
}

wrix_host_nix_config_main "$@"
