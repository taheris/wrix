#!/usr/bin/env bash
set -euo pipefail

skip() {
  local reason="$1"
  printf 'SKIP: %s\n' "$reason" >&2
  exit 77
}

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  exit 1
}

[[ "$(uname -s)" = "Darwin" ]] || skip "Darwin-only image-load verifier"
command -v cargo >/dev/null 2>&1 || skip "cargo not on PATH"
command -v jq >/dev/null 2>&1 || skip "jq not on PATH"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"

tmp=$(mktemp -d -t wrix-darwin-load.XXXXXX)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/workspace" "$tmp/home"
: >"$tmp/container.log"

cargo build --quiet -p wrix-cli --bin wrix
wrix="$TARGET_DIR/debug/wrix"

cat >"$tmp/bin/container" <<'CONTAINER_SHIM'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${WRIX_TEST_STATE:?}/container.log"
case "${1:-} ${2:-}" in
  'image list') ;;
  'image inspect') exit 1 ;;
  'image load') : >"$WRIX_TEST_STATE/loaded" ;;
  'image tag') ;;
  'run ') : >"$WRIX_TEST_STATE/container-ran" ;;
  *) ;;
esac
CONTAINER_SHIM
chmod +x "$tmp/bin/container"

image_source="$tmp/image.tar"
digest="sha256:abababababababababababababababababababababababababababababababab"
printf 'not a real tar; container shim must not read it\n' >"$image_source"
profile_config="$tmp/profile.json"
jq -n --arg source "$image_source" --arg digest "$digest" \
  '{schema:1,system:"test",profile:{name:"darwin",env:{},mounts:[],writable_dirs:[],network_allowlist:[]},image:{ref:"wrix-darwin:test",source:$source,source_kind:"docker-archive",digest:$digest},agent:{kind:"direct"},resources:{cpus:null,memory_mb:4096,pids_limit:4096},security:{deploy_key:null},services:{beads:{enable:"auto"},nix_cache:{enable:false}},features:{mcp_runtime:false}}' \
  >"$profile_config"

PATH="$tmp/bin:$PATH" HOME="$tmp/home" WRIX_TEST_STATE="$tmp" "$wrix" --profile-config "$profile_config" run "$tmp/workspace" true

grep -qF -- "image load --input $image_source" "$tmp/container.log" || fail "Darwin live launcher did not call container image load --input"
if grep -qE 'docker-archive:|oci-archive:|nix:' "$tmp/container.log"; then
  fail "Darwin live launcher used a non-Apple load transport"
fi
printf 'PASS: Darwin live Rust launcher uses container image load --input\n'
