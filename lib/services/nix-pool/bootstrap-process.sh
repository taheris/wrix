#!/usr/bin/env bash
set -euo pipefail

# A resource-classification witness, not repository, pool or client authority.
nix_pool_bootstrap_parent() {
  local pid="$1" proc="$2" field first second third fourth rest parent="" root_uid=false
  [[ "$pid" =~ ^[1-9][0-9]*$ && "${#pid}" -le 10 ]] || return 1
  while read -r field first second third fourth rest; do
    case "$field" in
      PPid:)
        [[ -z "$second$third$fourth$rest" ]] || return 1
        parent="$first"
        ;;
      Uid:)
        [[ "$first" == 0 && "$second" == 0 && "$third" == 0 && "$fourth" == 0 && -z "$rest" ]] || return 1
        root_uid=true
        ;;
    esac
  done < "$proc/$pid/status"
  [[ "$root_uid" == true && "$parent" =~ ^(0|[1-9][0-9]*)$ ]] || return 1
  printf '%s\n' "$parent"
}

nix_pool_bootstrap_kernel() {
  local pid="$1" proc="$2" field first second third fourth rest root_uid=false kernel=false
  [[ "$pid" =~ ^[1-9][0-9]*$ && "${#pid}" -le 10 ]] || return 1
  while read -r field first second third fourth rest; do
    case "$field" in
      Uid:)
        [[ "$first" == 0 && "$second" == 0 && "$third" == 0 && "$fourth" == 0 && -z "$rest" ]] || return 1
        root_uid=true
        ;;
      Kthread:)
        [[ "$first" == 1 && -z "$second$third$fourth$rest" ]] || return 1
        kernel=true
        ;;
    esac
  done < "$proc/$pid/status"
  [[ "$root_uid" == true && "$kernel" == true ]]
}

nix_pool_bootstrap_root_kernel() {
  local cgroup="$1" proc="$2" pid
  local -a remaining=()
  mapfile -t remaining < "$cgroup/cgroup.procs"
  for pid in "${remaining[@]}"; do
    if ! nix_pool_bootstrap_kernel "$pid" "$proc"; then
      printf 'Unaccounted userspace process %s remains outside the native resource groups.\n' "$pid" >&2
      return 1
    fi
  done
}

nix_pool_prepare_supervisor() {
  local cgroup="$1" proc="$2" pid="$3" chain supervisor="$1/wrix-nix-pool-supervisor"
  local -a initial=()
  local -A allowed=()
  [[ ! -e "$supervisor" && ! -L "$supervisor" ]] || return 1
  chain=$(nix_pool_bootstrap_chain "$pid" "$proc") || return 1
  while read -r pid; do allowed["$pid"]=1; done <<< "$chain"
  mkdir "$supervisor"
  mapfile -t initial < "$cgroup/cgroup.procs"
  for pid in "${initial[@]}"; do
    if [[ "$pid" =~ ^[1-9][0-9]*$ && -v allowed["$pid"] ]]; then
      printf '%s\n' "$pid" > "$supervisor/cgroup.procs"
    elif ! nix_pool_bootstrap_kernel "$pid" "$proc"; then
      printf 'Unexpected native bootstrap process %s; resource admission refused.\n' "$pid" >&2
      return 1
    fi
  done
  nix_pool_bootstrap_root_kernel "$cgroup" "$proc"
}

nix_pool_bootstrap_chain() {
  local pid="$1" proc="$2" parent _step
  local -A seen=()
  for _step in {1..8}; do
    [[ "$pid" =~ ^[1-9][0-9]*$ && ! -v seen["$pid"] ]] || return 1
    seen["$pid"]=1
    parent=$(nix_pool_bootstrap_parent "$pid" "$proc") || return 1
    printf '%s\n' "$pid"
    if [[ "$parent" == 0 ]]; then
      [[ "$pid" == 1 ]] || return 1
      return 0
    fi
    pid="$parent"
  done
  return 1
}
