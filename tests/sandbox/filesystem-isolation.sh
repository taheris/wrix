#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=tests/lib/live-sandbox.sh
source "$SCRIPT_DIR/../lib/live-sandbox.sh"

TEST_TMP="$(mktemp -d -t wrix-fs-isolation.XXXXXX)"
IMAGE_REF=""

cleanup() {
  if [[ -n "$IMAGE_REF" ]]; then
    wrix_remove_image_ref "$IMAGE_REF" >/dev/null 2>&1 || true # best-effort: cleanup must not mask the verifier result.
  fi
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  exit 1
}

backend="${1:-auto}"
case "$backend" in
  podman) wrix_require_live_sandbox_linux; export WRIX_MICROVM=0 ;;
  krun)
    wrix_require_live_sandbox_linux
    [[ -c /dev/kvm && -r /dev/kvm && -w /dev/kvm ]] || wrix_live_skip "krun requires accessible KVM"
    command -v krun >/dev/null || wrix_live_skip "krun is not on PATH"
    export WRIX_MICROVM=1
    ;;
  apple) wrix_require_live_sandbox_darwin ;;
  auto) wrix_require_live_sandbox ;;
  *) fail "unknown mount backend: $backend" ;;
esac
cd "$REPO_ROOT"

launcher=$(nix build --no-link --print-out-paths --no-warn-dirty .#wrix)
image_source=$(nix build --no-link --print-out-paths --no-warn-dirty .#test-image-git-credentials.source)
IMAGE_REF=$(wrix_live_image_ref "test-mount-controls-$$")

# Each launch gets cold, disposable sources outside the workspace.
for launch in run spawn; do
  root="$TEST_TMP/$launch"
  workspace="$root/workspace"
  profile_config="$root/profile.json"
  spawn_config="$root/spawn.json"
  mkdir -p "$workspace" "$root/sources"
  printf 'workspace-content\n' >"$workspace/testfile.txt"
  printf 'host-secret\n' >"$root/sources/undeclared-sibling-secret"
  mounts='[]'
  for declaration in profile spawn; do
    for mode in ro rw; do
      name="$declaration-$mode"
      mkdir -p "$root/sources/$name-dir"
      printf 'original\n' >"$root/sources/$name-file"
      printf 'original\n' >"$root/sources/$name-dir/sentinel"
      for kind in file dir; do
        mounts=$(jq -c --arg source "$root/sources/$name-$kind" \
          --arg dest "/tmp/wrix-mounts/$name-$kind" --arg mode "$mode" \
          --arg declaration "$declaration" \
          '. + [{source:$source,dest:$dest,mode:$mode,optional:false,declaration:$declaration}]' <<<"$mounts")
      done
    done
  done
  wrix_write_profile_config "$profile_config" "$IMAGE_REF" "$image_source" direct
  jq --argjson mounts "$mounts" --arg missing "$root/absent-optional" \
    '.profile.mounts = ($mounts | map(select(.declaration == "profile") | del(.declaration)))
      + [{source:$missing,dest:"/tmp/wrix-absent-optional",mode:"ro",optional:true}]' \
    "$profile_config" >"$profile_config.tmp"
  mv "$profile_config.tmp" "$profile_config"

  # shellcheck disable=SC2016 # Positional arguments and mount paths are evaluated in the guest.
  probe='
    set -euo pipefail
    [[ "$(< /workspace/testfile.txt)" == workspace-content ]]
    [[ ! -e "$1" && ! -e /tmp/wrix-absent-optional ]]
    if [[ -d /mnt/wrix ]]; then
      [[ -z "$(find /mnt/wrix -name undeclared-sibling-secret -print -quit)" ]]
    fi
    for declaration in profile spawn; do
      [[ "$declaration" == profile || "$2" == spawn ]] || continue
      for mode in ro rw; do
        base="/tmp/wrix-mounts/$declaration-$mode"
        for target in "$base-file" "$base-dir/sentinel"; do
          [[ "$(< "$target")" == original ]]
          if [[ "$mode" == ro ]]; then
            if (printf "changed\n" >"$target") 2>/dev/null; then
              printf "read-only write succeeded: %s\n" "$target" >&2
              exit 93
            fi
          else
            printf "changed\n" >"$target"
            [[ "$(< "$target")" == changed ]]
          fi
        done
        if [[ "$mode" == ro ]]; then
          if (printf "new\n" >"$base-dir/new") 2>/dev/null; then exit 94; fi
        else
          printf "new\n" >"$base-dir/new"
          [[ "$(< "$base-dir/new")" == new ]]
        fi
      done
    done
    printf "FILESYSTEM-ISOLATED\n"
  '
  args=(/bin/bash -c "$probe" probe "$root/sources/undeclared-sibling-secret" "$launch")
  if [[ "$launch" == run ]]; then
    printf -v command_line '%q ' "$launcher/bin/wrix" --profile-config "$profile_config" run "$workspace" -- "${args[@]}"
    result=$(wrix_run_with_pty "$command_line")
  else
    wrix_write_spawn_config "$spawn_config" "$workspace" "${args[@]}"
    jq --argjson mounts "$mounts" \
      '.mounts = ($mounts | map(select(.declaration == "spawn") |
        {host_path:.source,container_path:.dest,read_only:(.mode == "ro")}))' \
      "$spawn_config" >"$spawn_config.tmp"
    mv "$spawn_config.tmp" "$spawn_config"
    result=$(wrix_run_spawn "$launcher" "$profile_config" "$spawn_config")
  fi
  [[ "$result" == *FILESYSTEM-ISOLATED* ]] || fail "$launch workload did not complete: $result"
  for declaration in profile spawn; do
    for mode in ro rw; do
      name="$declaration-$mode"
      expected=original
      if [[ "$mode" == rw && ( "$declaration" == profile || "$launch" == spawn ) ]]; then
        expected=changed
      fi
      [[ "$(<"$root/sources/$name-file")" == "$expected" ]] || fail "$launch $name file host content"
      if [[ "$(uname -s)" == Linux ]]; then
        [[ "$(<"$root/sources/$name-dir/sentinel")" == "$expected" ]] || fail "$launch $name directory host content"
      elif [[ "$mode" == ro ]]; then
        [[ "$(<"$root/sources/$name-dir/sentinel")" == original ]] || fail "$launch $name read-only directory changed"
      fi
      if [[ "$mode" == ro ]]; then
        [[ ! -e "$root/sources/$name-dir/new" ]] || fail "read-only host directory gained a file"
      fi
    done
  done
  [[ "$(<"$root/sources/undeclared-sibling-secret")" == host-secret ]] || fail "host sibling changed"
  [[ ! -e "$root/absent-optional" ]] || fail "optional mount created a host source"

  # Missing required declarations must not reach the workload or create host sources.
  for declaration in profile spawn; do
    [[ "$launch" == spawn || "$declaration" == profile ]] || continue
    if [[ "$declaration" == profile ]]; then
      jq --arg missing "$root/absent-required" \
        '.profile.mounts += [{source:$missing,dest:"/tmp/missing",mode:"rw",optional:false}]' \
        "$profile_config" >"$root/missing-profile.json"
      missing_profile="$root/missing-profile.json"
    else
      missing_profile="$profile_config"
      jq --arg missing "$root/absent-required" \
        '.mounts += [{host_path:$missing,container_path:"/tmp/missing",read_only:false}]' \
        "$spawn_config" >"$root/missing-spawn.json"
      spawn_config="$root/missing-spawn.json"
    fi
    if [[ "$launch" == run ]]; then
      printf -v command_line '%q ' "$launcher/bin/wrix" --profile-config "$missing_profile" run "$workspace" -- /bin/bash -c 'touch /workspace/unexpected-launch'
      if wrix_run_with_pty "$command_line" >"$root/missing.log" 2>&1; then fail "missing profile source admitted"; fi
    else
      jq '.agent_args = ["/bin/bash","-c","touch /workspace/unexpected-launch"]' "$spawn_config" >"$root/missing-probe.json"
      if wrix_run_spawn "$launcher" "$missing_profile" "$root/missing-probe.json" >"$root/missing.log" 2>&1; then fail "missing $declaration source admitted"; fi
    fi
    [[ ! -e "$workspace/unexpected-launch" && ! -e "$root/absent-required" ]] || fail "missing source launch had side effects"
  done
  printf 'PASS: %s %s file/directory profile/spawn mount controls (Darwin directory writeback not asserted)\n' "$backend" "$launch" >&2
done
