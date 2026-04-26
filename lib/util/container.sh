#!/usr/bin/env bash
# Container runtime abstraction — Apple container CLI (Darwin) / podman (Linux).
#
# Source this file to get cr_* functions that dispatch to the correct runtime.
# On Darwin, assumes macOS 26+ with the Apple container CLI installed.

if [[ -n "${CR:-}" ]]; then
  : # Caller pre-set the runtime (tests, overrides)
elif [[ "$(uname)" == "Darwin" ]]; then
  # Prefer Apple container CLI on Darwin — podman binary may exist but
  # podman machine is typically off to save resources.
  if command -v container >/dev/null 2>&1; then
    CR=container
  else
    CR=podman
  fi
else
  CR=podman
fi

cr_image_exists() {
  case "$CR" in
    container) container image inspect "$1" >/dev/null 2>&1 ;;
    podman) podman image exists "$1" 2>/dev/null ;;
  esac
}

cr_image_tag() {
  case "$CR" in
    container) container image tag "$1" "$2" ;;
    podman) podman tag "$1" "$2" ;;
  esac
}

cr_image_delete() {
  case "$CR" in
    container) container image delete "$1" 2>/dev/null || true ;;
    podman) podman rmi "$1" 2>/dev/null || true ;;
  esac
}

cr_image_prune() {
  case "$CR" in
    container) container image prune 2>/dev/null || true ;;
    podman) podman image prune -f 2>/dev/null || true ;;
  esac
}

cr_exists() {
  case "$CR" in
    container) container inspect "$1" >/dev/null 2>&1 ;;
    podman) podman container exists "$1" ;;
  esac
}

cr_is_running() {
  case "$CR" in
    container)
      local state
      state=$(container inspect "$1" 2>/dev/null | jq -r '.[0] | .state // .status // empty') || return 1
      [[ "$state" == "running" ]]
      ;;
    podman)
      [[ "$(podman inspect --format '{{.State.Running}}' "$1" 2>/dev/null)" == "true" ]]
      ;;
  esac
}

cr_status() {
  case "$CR" in
    container) container inspect "$1" 2>/dev/null | jq -r '.[0] | .state // .status // "not found"' ;;
    podman) podman inspect --format '{{.State.Status}}' "$1" 2>/dev/null || echo "not found" ;;
  esac
}

cr_exit_code() {
  case "$CR" in
    container) container inspect "$1" 2>/dev/null | jq -r '.[0] | .exitCode // .exit_code // empty' ;;
    podman) podman inspect --format '{{.State.ExitCode}}' "$1" 2>/dev/null ;;
  esac
}

cr_container_image() {
  case "$CR" in
    container) container inspect "$1" 2>/dev/null | jq -r '.[0] | .image // empty' ;;
    podman) podman inspect --format '{{.Image}}' "$1" 2>/dev/null ;;
  esac
}

cr_image_id() {
  case "$CR" in
    container) container image inspect "$1" 2>/dev/null | jq -r '.[0] | .digest // .id // empty' ;;
    podman) podman image inspect --format '{{.Id}}' "$1" 2>/dev/null ;;
  esac
}

cr_stop() {
  case "$CR" in
    container) container stop "$1" 2>/dev/null || true ;;
    podman) podman stop "$1" 2>/dev/null || true ;;
  esac
}

cr_rm() {
  case "$CR" in
    container) container stop "$1" 2>/dev/null || true; container rm "$1" 2>/dev/null || true ;;
    podman) podman rm -f "$1" 2>/dev/null || true ;;
  esac
}

cr_exec() {
  local name="$1"
  shift
  "$CR" exec "$name" "$@"
}

cr_exec_it() {
  local name="$1"
  shift
  case "$CR" in
    container) container exec -t -i "$name" "$@" ;;
    podman) podman exec -it "$name" "$@" ;;
  esac
}

cr_logs() {
  "$CR" logs "$1" 2>&1
}

cr_logs_tail() {
  local name="$1" n="${2:-50}"
  case "$CR" in
    container) container logs "$name" 2>&1 | tail -n "$n" ;;
    podman) podman logs --tail "$n" "$name" 2>&1 ;;
  esac
}

cr_wait() {
  case "$CR" in
    container)
      while cr_is_running "$1"; do
        sleep 1
      done
      ;;
    podman)
      podman wait "$1"
      ;;
  esac
}

cr_network_exists() {
  case "$CR" in
    container) container network inspect "$1" >/dev/null 2>&1 ;;
    podman) podman network exists "$1" ;;
  esac
}

cr_network_create() {
  if cr_network_exists "$1"; then
    return 0
  fi
  case "$CR" in
    container) container network create "$1" >/dev/null 2>&1 || cr_network_exists "$1" ;;
    podman) podman network create "$1" >/dev/null 2>&1 || podman network exists "$1" ;;
  esac
}

cr_network_connect() {
  local network="$1" name="$2"
  case "$CR" in
    container)
      # Apple Containers on the default network can communicate directly
      # via container IPs — dynamic network connect is not needed.
      return 0
      ;;
    podman)
      podman network connect "$network" "$name"
      ;;
  esac
}

cr_network_has() {
  local name="$1" network="$2"
  case "$CR" in
    container) return 0 ;;
    podman)
      podman inspect "$name" --format '{{range $k, $v := .NetworkSettings.Networks}}{{$k}} {{end}}' \
        | grep -qw "$network"
      ;;
  esac
}

cr_ps_names() {
  case "$CR" in
    container) container list --all --format json 2>/dev/null | jq -r '.[].configuration.id // empty' 2>/dev/null ;;
    podman) podman ps --format '{{.Names}}' 2>/dev/null ;;
  esac
}

cr_ps_names_by_prefix() {
  local prefix="$1"
  cr_ps_names | grep "^${prefix}" || true
}

cr_cp() {
  local name="$1" src="$2" dst="$3"
  case "$CR" in
    container) cat "$src" | cr_exec "$name" sh -c "cat > '$dst'" ;;
    podman) podman cp "$src" "${name}:${dst}" ;;
  esac
}

cr_label() {
  local name="$1" key="$2"
  case "$CR" in
    container) cr_exec "$name" cat "/tmp/cr-labels/${key}" 2>/dev/null || echo "" ;;
    podman) podman inspect --format "{{index .Config.Labels \"${key}\"}}" "$name" 2>/dev/null || echo "" ;;
  esac
}

cr_set_labels() {
  local name="$1"
  shift
  case "$CR" in
    container)
      local cmds="mkdir -p /tmp/cr-labels"
      while [[ $# -gt 0 ]]; do
        local key="${1%%=*}"
        local val="${1#*=}"
        cmds="$cmds && echo '$val' > /tmp/cr-labels/$key"
        shift
      done
      cr_exec "$name" sh -c "$cmds" 2>/dev/null || true
      ;;
    podman)
      ;;
  esac
}
