#!/usr/bin/env bash
set -euo pipefail

wrix_live_skip() {
  local reason="$1"
  printf 'SKIP: %s\n' "$reason" >&2
  exit 77
}

wrix_require_live_sandbox_linux() {
  local uname_s tool

  uname_s=$(uname -s)
  [[ "$uname_s" = "Linux" ]] || wrix_live_skip "Linux-only verifier (uname=$uname_s)"
  [[ ! -e /run/.containerenv ]] || wrix_live_skip "nested container: rootless podman unavailable"

  for tool in git jq nix podman ssh-keygen; do
    command -v "$tool" >/dev/null 2>&1 || wrix_live_skip "$tool not on PATH"
  done
}

wrix_require_live_sandbox_darwin() {
  local major version tool uname_s

  uname_s=$(uname -s)
  [[ "$uname_s" = "Darwin" ]] || wrix_live_skip "Darwin-only verifier (uname=$uname_s)"

  for tool in container git jq nix ssh-keygen sw_vers; do
    command -v "$tool" >/dev/null 2>&1 || wrix_live_skip "$tool not on PATH"
  done

  version=$(sw_vers -productVersion)
  major="${version%%.*}"
  [[ "$major" =~ ^[0-9]+$ ]] || wrix_live_skip "cannot parse macOS version: $version"
  [[ "$major" -ge 26 ]] || wrix_live_skip "macOS 26+ required (current: $version)"
}

wrix_require_live_sandbox() {
  local uname_s

  uname_s=$(uname -s)
  case "$uname_s" in
    Linux) wrix_require_live_sandbox_linux ;;
    Darwin) wrix_require_live_sandbox_darwin ;;
    *) wrix_live_skip "unsupported live sandbox host: $uname_s" ;;
  esac
}

wrix_live_source_kind() {
  local uname_s

  uname_s=$(uname -s)
  case "$uname_s" in
    Linux) printf '%s\n' "nix-descriptor" ;;
    Darwin) printf '%s\n' "docker-archive" ;;
    *)
      printf 'unsupported live sandbox host: %s\n' "$uname_s" >&2
      return 64
      ;;
  esac
}

wrix_live_image_ref() {
  local name="$1"
  local uname_s

  uname_s=$(uname -s)
  case "$uname_s" in
    Linux) printf 'localhost/wrix-%s:latest\n' "$name" ;;
    Darwin) printf 'wrix-%s:latest\n' "$name" ;;
    *)
      printf 'unsupported live sandbox host: %s\n' "$uname_s" >&2
      return 64
      ;;
  esac
}

wrix_agent_image_attr() {
  local agent="$1"
  local uname_s

  uname_s=$(uname -s)
  case "$uname_s:$agent" in
    Linux:claude) printf '%s\n' "test-image-base" ;;
    Linux:direct) printf '%s\n' "test-image-base-direct" ;;
    Linux:pi) printf '%s\n' "test-image-base-pi" ;;
    Darwin:claude) printf '%s\n' "image-base-claude" ;;
    Darwin:direct) printf '%s\n' "image-base" ;;
    Darwin:pi) printf '%s\n' "image-base-pi" ;;
    Linux:*|Darwin:*)
      printf 'unknown test image agent: %s\n' "$agent" >&2
      return 64
      ;;
    *)
      printf 'unsupported live sandbox host: %s\n' "$uname_s" >&2
      return 64
      ;;
  esac
}

wrix_build_live_launcher() {
  nix build --no-link --print-out-paths --no-warn-dirty .#sandbox-claude.launcher
}

wrix_realize_test_image_source() {
  local agent="$1"
  local image_attr

  image_attr=$(wrix_agent_image_attr "$agent")
  nix build --no-link --print-out-paths --no-warn-dirty ".#$image_attr.source"
}

wrix_write_profile_config() {
  local out="$1"
  local image_ref="$2"
  local image_source="$3"
  local agent="$4"
  local allowlist_csv="${5:-}"
  local source_kind="${6:-}"
  local allowlist_json digest

  [[ -n "$source_kind" ]] || source_kind=$(wrix_live_source_kind)
  allowlist_json=$(jq -Rn --arg csv "$allowlist_csv" '$csv | split(",") | map(select(length > 0))')
  case "$source_kind" in
    nix-descriptor) digest=$(jq -er '.digest | strings | select(test("^sha256:[0-9a-f]{64}$"))' "$image_source") ;;
    docker-archive) digest="" ;;
    *)
      printf 'unsupported image source kind: %s\n' "$source_kind" >&2
      return 64
      ;;
  esac

  jq -n \
    --arg image_ref "$image_ref" \
    --arg image_source "$image_source" \
    --arg source_kind "$source_kind" \
    --arg digest "$digest" \
    --arg agent "$agent" \
    --argjson allowlist "$allowlist_json" \
    '{
      schema: 1,
      system: "test",
      profile: {
        name: "base",
        env: {},
        mounts: [],
        writable_dirs: [],
        network_allowlist: $allowlist
      },
      image: {
        ref: $image_ref,
        source: $image_source,
        source_kind: $source_kind,
        digest: $digest
      },
      agent: { kind: $agent },
      resources: { cpus: null, memory_mb: 4096, pids_limit: 4096 },
      security: { deploy_key: null },
      network: { default_mode: "open", ipv6: "disabled" },
      services: {
        beads: { enable: "auto" },
        nix_cache: { enable: false }
      },
      features: { mcp_runtime: false }
    }' >"$out"
}

wrix_write_spawn_config() {
  local out="$1"
  local workspace="$2"
  local args_json
  shift 2

  if (($# == 0)); then
    args_json='[]'
  else
    args_json=$(printf '%s\0' "$@" | jq -Rs 'split("\u0000")[:-1]')
  fi

  jq -n \
    --arg workspace "$workspace" \
    --argjson args "$args_json" \
    '{
      workspace: $workspace,
      env: [],
      agent_args: $args,
      mounts: []
    }' >"$out"
}

wrix_make_ed25519_key() {
  local path="$1"
  local comment="$2"

  ssh-keygen -t ed25519 -N "" -q -f "$path" -C "$comment" >/dev/null
}

wrix_run_spawn() {
  local launcher="$1"
  local profile_config="$2"
  local spawn_config="$3"
  shift 3

  "$launcher/bin/wrix" --profile-config "$profile_config" spawn --spawn-config "$spawn_config" "$@"
}

wrix_run_with_pty() {
  local command_line="$1"
  local uname_s

  uname_s=$(uname -s)
  case "$uname_s" in
    Linux) script -qefc "$command_line" /dev/null ;;
    Darwin) script -q /dev/null /bin/bash -lc "$command_line" ;;
    *)
      printf 'unsupported live sandbox host: %s\n' "$uname_s" >&2
      return 64
      ;;
  esac
}

wrix_remove_image_ref() {
  local image_ref="$1"
  local uname_s

  [[ -n "$image_ref" ]] || return 0
  uname_s=$(uname -s)
  case "$uname_s" in
    Linux)
      if podman image exists "$image_ref"; then
        podman rmi "$image_ref" >/dev/null
      fi
      ;;
    Darwin)
      if container image inspect "$image_ref" >/dev/null 2>&1; then
        container image delete "$image_ref" >/dev/null
      fi
      ;;
    *)
      printf 'unsupported live sandbox host: %s\n' "$uname_s" >&2
      return 64
      ;;
  esac
}
