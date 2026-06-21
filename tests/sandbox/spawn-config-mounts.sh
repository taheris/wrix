#!/usr/bin/env bash
# Verifier for criterion 132 of specs/sandbox.md:
#
#   On Linux, each `SpawnConfig.mounts` entry becomes a
#   `-v <host_path>:<container_path>` podman argument, with `:ro` appended
#   when `read_only: true`. A missing or empty `mounts` list produces no
#   additional `-v` flags.
#
# The parser + renderer live in the launcher script body in
# `lib/sandbox/linux/default.nix`. We build the launcher via `nix build`
# and exercise it under `WRIX_DRY_RUN=1`, which runs the SpawnConfig
# parse + mount rendering but skips the profile-mount staging loop and
# the `podman run` invocation. The dry-run dump prints one `MOUNT=-v …`
# line per parsed SpawnConfig entry, so assertions can inspect the
# rendered argv shape without a container runtime.
#
# Hermetic: temp host_path strings only need to exist as strings in the
# dry-run; podman is never invoked. No dolt / sccache / workspace host
# state required.
#
# Style: mirrors tests/sandbox/darwin-mount-classifier.sh — same skip
# gate, build step, and ALL_TESTS dispatch.

set -euo pipefail

if ! uname -s | grep -q Linux; then
  echo "skip: linux-only test (uname=$(uname -s))" >&2
  exit 77
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP=$(mktemp -d -t wrix-spawn-mounts.XXXXXX)
cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

PASSED=0
FAILED=0

pass() { printf '  PASS: %s\n' "$1"; PASSED=$((PASSED + 1)); }
fail() { printf '  FAIL: %s\n' "$1" >&2; FAILED=$((FAILED + 1)); }

# Build the Linux launcher once. profile.base keeps the profile-mount
# leg empty so SPAWN_MOUNTS is the only source of -v output in the
# dry-run dump.
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

WRIX="$LAUNCHER_LINK/bin/wrix"
if [[ ! -x "$WRIX" ]]; then
  echo "fixture error: launcher binary not built at $WRIX" >&2
  exit 1
fi

PROFILE_CONFIG="$TEST_TMP/profile-config.json"
cat > "$PROFILE_CONFIG" <<'JSON'
{"schema":1,"system":"test","profile":{"name":"base","env":{},"mounts":[],"writable_dirs":[],"network_allowlist":[]},"image":{"ref":"wrix-base:test","source":"/nix/store/fake-image","source_kind":"nix-descriptor","digest":"sha256:test"},"agent":{"kind":"direct"},"resources":{"cpus":null,"memory_mb":4096,"pids_limit":4096},"security":{"deploy_key":null},"network":{"default_mode":"open","ipv6":"disabled"},"services":{"beads":{"enable":"auto"},"nix_cache":{"enable":true}},"features":{"mcp_runtime":false}}
JSON

# Workspace dir referenced by every spawn-config we generate. Need not
# match host layout — podman is never invoked, the dry-run dump simply
# echoes whatever the JSON says.
WORKSPACE="$TEST_TMP/workspace"
mkdir -p "$WORKSPACE"

# write_spawn_config <out_file> <mounts_field_or_empty>
# mounts_field_or_empty is either a complete `"mounts": [...]` clause
# (with trailing comma if not last) or empty to elide the field
# entirely.
write_spawn_config() {
  local out_file="$1"
  local mounts_field="$2"
  cat > "$out_file" <<EOF
{
  "workspace": "$WORKSPACE",
  "image_ref": "wrix-base:test",
  "image_source": "",
  "env": [],
  "agent_args": []${mounts_field:+,
  $mounts_field}
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
  WRIX_DRY_RUN=1 "$WRIX" --profile-config "$PROFILE_CONFIG" spawn --spawn-config "$config" >"$out" 2>"$err" || rc=$?
  echo "$rc"
}

# Count `^MOUNT=-v ` lines in the dry-run dump.
count_mount_lines() {
  local out="$1"
  # grep -c exits 1 on zero matches; we want that to flow through as the count 0
  grep -cE '^MOUNT=-v ' "$out" || true
}

# Dump every `^MOUNT=` line in the dry-run output for fail-message diagnostics.
# best-effort: empty grep output is meaningful diagnostic context (means the
# launcher emitted no MOUNT= lines at all), so zero matches is not a failure.
dump_mount_lines() {
  grep '^MOUNT=' "$1" || true
}

# ============================================================================
# Two-entry mounts list: one read_only:true (renders :ro), one
# read_only:false (no :ro). Exactly two extra MOUNT=-v lines.
# ============================================================================
test_two_mounts_with_one_ro() {
  local host_rw="$TEST_TMP/rw-src"
  local host_ro="$TEST_TMP/ro-src"

  local config="$TEST_TMP/two.json"
  write_spawn_config "$config" \
    "\"mounts\": [
      {\"host_path\":\"$host_rw\",\"container_path\":\"/mnt/rw\",\"read_only\":false},
      {\"host_path\":\"$host_ro\",\"container_path\":\"/mnt/ro\",\"read_only\":true}
    ]"

  local out="$TEST_TMP/two.out" err="$TEST_TMP/two.err" rc
  rc=$(run_launcher "$config" "$out" "$err")
  if [[ "$rc" != "0" ]]; then
    fail "two mounts: launcher exited $rc; stderr=$(cat "$err")"
    return 1
  fi

  local count
  count=$(count_mount_lines "$out")
  if [[ "$count" != "2" ]]; then
    fail "two mounts: expected 2 MOUNT=-v lines, got $count: $(dump_mount_lines "$out")"
    return 1
  fi

  if ! grep -qxF "MOUNT=-v $host_rw:/mnt/rw" "$out"; then
    fail "two mounts: rw entry missing or :ro-tainted; got: $(grep '^MOUNT=' "$out")"
    return 1
  fi
  if ! grep -qxF "MOUNT=-v $host_ro:/mnt/ro:ro" "$out"; then
    fail "two mounts: ro entry missing :ro; got: $(grep '^MOUNT=' "$out")"
    return 1
  fi
  pass "Two SpawnConfig.mounts → two -v lines, :ro exactly on read_only entry"
}

# ============================================================================
# Missing mounts field: no extra MOUNT=-v lines. Empty list default
# matches the loom-side `#[serde(default, skip_serializing_if = ...)]`.
# ============================================================================
test_missing_mounts_field_zero_lines() {
  local config="$TEST_TMP/missing.json"
  write_spawn_config "$config" ""

  local out="$TEST_TMP/missing.out" err="$TEST_TMP/missing.err" rc
  rc=$(run_launcher "$config" "$out" "$err")
  if [[ "$rc" != "0" ]]; then
    fail "missing mounts: launcher exited $rc; stderr=$(cat "$err")"
    return 1
  fi

  local count
  count=$(count_mount_lines "$out")
  if [[ "$count" != "0" ]]; then
    fail "missing mounts: expected 0 MOUNT=-v lines, got $count: $(dump_mount_lines "$out")"
    return 1
  fi
  pass "Missing mounts field → zero -v lines (empty-list default)"
}

# ============================================================================
# Explicit empty mounts list: same zero-line contract as missing field.
# ============================================================================
test_empty_mounts_list_zero_lines() {
  local config="$TEST_TMP/empty.json"
  write_spawn_config "$config" "\"mounts\": []"

  local out="$TEST_TMP/empty.out" err="$TEST_TMP/empty.err" rc
  rc=$(run_launcher "$config" "$out" "$err")
  if [[ "$rc" != "0" ]]; then
    fail "empty mounts: launcher exited $rc; stderr=$(cat "$err")"
    return 1
  fi

  local count
  count=$(count_mount_lines "$out")
  if [[ "$count" != "0" ]]; then
    fail "empty mounts: expected 0 MOUNT=-v lines, got $count: $(dump_mount_lines "$out")"
    return 1
  fi
  pass "Explicit empty mounts list → zero -v lines"
}

ALL_TESTS=(
  test_two_mounts_with_one_ro
  test_missing_mounts_field_zero_lines
  test_empty_mounts_list_zero_lines
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
