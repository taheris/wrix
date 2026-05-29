#!/usr/bin/env bash
# Verifier for criterion 134 of specs/sandbox.md:
#
#   On Darwin, the same mount classifier handles `profile.mounts` and
#   `SpawnConfig.mounts` — one mechanism, not two. Directories are
#   staged + copied at launch, regular files copy-from-parent-dir, and
#   entries whose `host_path` is a Unix socket cause the launcher to
#   fail loudly before the container starts. (VirtioFS does not pass
#   socket operations, so a silently-mounted socket would dead-end at
#   the first `connect()`.)
#
# The classifier lives in the launcher script body in
# `lib/sandbox/darwin/default.nix`. We build the launcher via
# `nix build` (Darwin-only — the Darwin launcher imports
# `darwinSandbox` which is platform-gated) and exercise it under
# `WRAPIX_DRY_RUN=1`, which runs parsing + classification but skips
# the macOS container CLI, image load, and the `container run`
# invocation. The dry-run dump exposes the classifier's resolved
# MOUNT_ARGS / DIR_MOUNTS / FILE_MOUNTS for assertion.
#
# Hermetic: temp dir/file/socket sources are created in mktemp space,
# no dolt / sccache / host workspace is required.
#
# Style: mirrors tests/sandbox/missing-image-env.sh and
# tests/sandbox/custom-mounts-env.sh — same pass/fail helpers and
# ALL_TESTS dispatch.

set -euo pipefail

if ! uname -s | grep -q Darwin; then
  echo "skip: darwin-only test (uname=$(uname -s))" >&2
  exit 77
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP=$(mktemp -d -t wrapix-mount-cls.XXXXXX)
cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

PASSED=0
FAILED=0

pass() { printf '  PASS: %s\n' "$1"; PASSED=$((PASSED + 1)); }
fail() { printf '  FAIL: %s\n' "$1" >&2; FAILED=$((FAILED + 1)); }

# Build the Darwin launcher once, profile.mounts empty so the classifier's
# input is exactly the SpawnConfig.mounts entry the test provides.
LAUNCHER_LINK="$TEST_TMP/launcher"
nix build \
  --impure --no-warn-dirty \
  --out-link "$LAUNCHER_LINK" \
  --expr "
    let
      flake = builtins.getFlake \"git+file://$REPO_ROOT\";
      system = builtins.currentSystem;
      lib = flake.legacyPackages.\${system}.lib;
    in (lib.mkSandbox { profile = lib.profiles.base; }).launcher
  "

WRAPIX="$LAUNCHER_LINK/bin/wrapix"
if [[ ! -x "$WRAPIX" ]]; then
  echo "fixture error: launcher binary not built at $WRAPIX" >&2
  exit 1
fi

# Workspace dir referenced by every spawn-config we generate.
WORKSPACE="$TEST_TMP/workspace"
mkdir -p "$WORKSPACE"

# write_spawn_config <out_file> <mounts_json>
write_spawn_config() {
  local out_file="$1"
  local mounts_json="$2"
  cat > "$out_file" <<EOF
{
  "workspace": "$WORKSPACE",
  "image_ref": "wrapix-base:test",
  "image_source": "",
  "env": [],
  "agent_args": [],
  "mounts": $mounts_json
}
EOF
}

# run_launcher <config_file> <stdout_file> <stderr_file>
# Returns the launcher exit code without aborting under `set -e`.
run_launcher() {
  local config="$1"
  local out="$2"
  local err="$3"
  local rc=0
  WRAPIX_DRY_RUN=1 "$WRAPIX" spawn --spawn-config "$config" >"$out" 2>"$err" || rc=$?
  echo "$rc"
}

# ============================================================================
# Temp directory: classifier emits a dir-staging mount (cp -rL is skipped
# under WRAPIX_DRY_RUN=1, but the mount intent is recorded).
# ============================================================================
test_temp_dir_classified_as_dir_staging() {
  local host_dir
  host_dir=$(mktemp -d -t wrapix-cls-dir.XXXXXX)

  local config="$TEST_TMP/dir.json"
  write_spawn_config "$config" \
    "[{\"host_path\":\"$host_dir\",\"container_path\":\"/mnt/test-dir\",\"read_only\":false}]"

  local out="$TEST_TMP/dir.out" err="$TEST_TMP/dir.err" rc
  rc=$(run_launcher "$config" "$out" "$err")

  if [[ "$rc" != "0" ]]; then
    fail "temp dir: launcher exited $rc; stderr=$(cat "$err")"
    rm -rf "$host_dir"
    return 1
  fi

  if ! grep -qE '^MOUNT_ARGS=.* -v [^ ]+/dir0:/mnt/wrapix/dir0( |$)' "$out"; then
    fail "temp dir: MOUNT_ARGS missing host-staging:/mnt/wrapix/dir0; got: $(grep MOUNT_ARGS "$out")"
    rm -rf "$host_dir"
    return 1
  fi
  if ! grep -qE '^DIR_MOUNTS=/mnt/wrapix/dir0:/mnt/test-dir(,|$)' "$out"; then
    fail "temp dir: DIR_MOUNTS missing /mnt/wrapix/dir0:/mnt/test-dir; got: $(grep DIR_MOUNTS "$out")"
    rm -rf "$host_dir"
    return 1
  fi
  rm -rf "$host_dir"
  pass "Temp directory mount → dir-staging classification"
}

# ============================================================================
# Temp regular file: classifier picks the parent-dir-staging path; the
# file mount renders as parent-dir bind + entrypoint copy intent.
# ============================================================================
test_temp_file_classified_as_parent_dir_staging() {
  local host_file
  host_file=$(mktemp -t wrapix-cls-file.XXXXXX)
  local parent
  parent=$(dirname "$host_file")
  local base
  base=$(basename "$host_file")

  local config="$TEST_TMP/file.json"
  write_spawn_config "$config" \
    "[{\"host_path\":\"$host_file\",\"container_path\":\"/etc/test-file\",\"read_only\":true}]"

  local out="$TEST_TMP/file.out" err="$TEST_TMP/file.err" rc
  rc=$(run_launcher "$config" "$out" "$err")

  if [[ "$rc" != "0" ]]; then
    fail "temp file: launcher exited $rc; stderr=$(cat "$err")"
    rm -f "$host_file"
    return 1
  fi

  if ! grep -qE "^MOUNT_ARGS=.* -v ${parent}:/mnt/wrapix/file0( |$)" "$out"; then
    fail "temp file: MOUNT_ARGS missing parent-dir bind for $parent; got: $(grep MOUNT_ARGS "$out")"
    rm -f "$host_file"
    return 1
  fi
  if ! grep -qE "^FILE_MOUNTS=/mnt/wrapix/file0/${base}:/etc/test-file(,|$)" "$out"; then
    fail "temp file: FILE_MOUNTS missing parent-dir-staging entry; got: $(grep FILE_MOUNTS "$out")"
    rm -f "$host_file"
    return 1
  fi
  rm -f "$host_file"
  pass "Temp regular file mount → parent-dir-staging classification"
}

# ============================================================================
# Temp Unix socket: classifier rejects before container run, naming the
# host_path and citing VirtioFS in the error.
# ============================================================================
test_temp_socket_rejected_before_container_run() {
  local sock_dir
  sock_dir=$(mktemp -d -t wrapix-cls-sock.XXXXXX)
  local sock_path="$sock_dir/test.sock"

  # Create a bound unix socket on disk and exit; the filesystem entry
  # persists so the launcher's classifier sees a real socket file.
  if ! command -v python3 >/dev/null 2>&1; then
    fail "temp socket: python3 not available; cannot create test socket"
    rm -rf "$sock_dir"
    return 1
  fi
  python3 -c "
import socket, sys
s = socket.socket(socket.AF_UNIX)
s.bind('$sock_path')
s.close()
"
  if [[ ! -S "$sock_path" ]]; then
    fail "temp socket: failed to create unix socket at $sock_path"
    rm -rf "$sock_dir"
    return 1
  fi

  local config="$TEST_TMP/sock.json"
  write_spawn_config "$config" \
    "[{\"host_path\":\"$sock_path\",\"container_path\":\"/run/test.sock\",\"read_only\":false}]"

  local out="$TEST_TMP/sock.out" err="$TEST_TMP/sock.err" rc
  rc=$(run_launcher "$config" "$out" "$err")

  if [[ "$rc" == "0" ]]; then
    fail "temp socket: launcher did not exit non-zero (got rc=0); stdout=$(cat "$out"); stderr=$(cat "$err")"
    rm -rf "$sock_dir"
    return 1
  fi
  if ! grep -qF "$sock_path" "$err"; then
    fail "temp socket: stderr does not name the host_path ($sock_path); stderr=$(cat "$err")"
    rm -rf "$sock_dir"
    return 1
  fi
  if ! grep -qFi "virtiofs" "$err"; then
    fail "temp socket: stderr does not cite VirtioFS; stderr=$(cat "$err")"
    rm -rf "$sock_dir"
    return 1
  fi
  rm -rf "$sock_dir"
  pass "Temp Unix socket mount → rejected before container run, error cites VirtioFS and host_path"
}

ALL_TESTS=(
  test_temp_dir_classified_as_dir_staging
  test_temp_file_classified_as_parent_dir_staging
  test_temp_socket_rejected_before_container_run
)

run_all() {
  local fn rc
  for fn in "${ALL_TESTS[@]}"; do
    echo "=== $fn ==="
    rc=0
    "$fn" || rc=$?
    if [[ "$rc" -ne 0 && "$rc" -ne 77 ]]; then
      fail "$fn returned $rc without calling fail()"
    fi
  done
  echo
  echo "Results: $PASSED passed, $FAILED failed"
  [[ "$FAILED" -eq 0 ]]
}

if [[ $# -eq 0 ]]; then
  run_all
else
  fn="$1"
  if ! declare -f "$fn" >/dev/null 2>&1; then
    echo "Unknown function: $fn" >&2
    exit 1
  fi
  "$fn"
fi
