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
: >"$tmp/skopeo.log"

cargo build --quiet -p wrix-cli --bin wrix
wrix="$TARGET_DIR/debug/wrix"

cat >"$tmp/bin/container" <<'CONTAINER_SHIM'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${WRIX_TEST_STATE:?}/container.log"
case "${1:-} ${2:-}" in
  'image list') ;;
  'image inspect') exit 1 ;;
  'image load')
    [[ "${3:-}" == "--input" ]]
    [[ -f "${4:-}" ]]
    printf '%s\n' "${4:-}" >"$WRIX_TEST_STATE/loaded-path"
    printf 'Loaded: untagged@sha256:%064d\n' 0
    ;;
  'image tag') ;;
  'run ') : >"$WRIX_TEST_STATE/container-ran" ;;
  *) ;;
esac
CONTAINER_SHIM
chmod +x "$tmp/bin/container"

cat >"$tmp/bin/skopeo" <<'SKOPEO_SHIM'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${WRIX_TEST_STATE:?}/skopeo.log"
[[ "${1:-}" == "--insecure-policy" ]]
[[ "${2:-}" == "copy" ]]
[[ "${3:-}" == "--quiet" ]]
[[ "${4:-}" == docker-archive:* ]]
[[ "${5:-}" == oci-archive:* ]]
printf 'fake OCI archive\n' >"${5#oci-archive:}"
SKOPEO_SHIM
chmod +x "$tmp/bin/skopeo"

image_source="$tmp/image.tar"
digest="sha256:abababababababababababababababababababababababababababababababab"
printf 'not a real tar; container shim must not read it\n' >"$image_source"
profile_config="$tmp/profile.json"
jq -n --arg source "$image_source" --arg digest "$digest" \
  '{schema:1,system:"test",profile:{name:"darwin",env:{},mounts:[],writable_dirs:[],network_allowlist:[]},image:{ref:"wrix-darwin:test",source:$source,source_kind:"docker-archive",digest:$digest},agent:{kind:"direct"},resources:{cpus:null,memory_mb:4096,pids_limit:4096},security:{deploy_key:null},services:{beads:{enable:"auto"},nix_cache:{enable:false}},features:{mcp_runtime:false}}' \
  >"$profile_config"

PATH="$tmp/bin:$PATH" HOME="$tmp/home" WRIX_TEST_STATE="$tmp" "$wrix" --profile-config "$profile_config" run "$tmp/workspace" true

loaded_path=$(<"$tmp/loaded-path")
[[ "$loaded_path" != "$image_source" ]] || fail "Darwin live launcher passed the Docker archive directly to container image load"
[[ "$loaded_path" == */image.oci ]] || fail "Darwin live launcher did not load a converted OCI archive"
[[ ! -e "$loaded_path" ]] || fail "Darwin live launcher left its converted OCI archive behind"
grep -qF -- "--insecure-policy copy --quiet docker-archive:$image_source oci-archive:$loaded_path" "$tmp/skopeo.log" \
  || fail "Darwin live launcher did not convert the Docker archive with skopeo"
grep -qF -- "image load --input $loaded_path" "$tmp/container.log" \
  || fail "Darwin live launcher did not pass the converted OCI archive to container image load"
grep -qF -- "image tag untagged@sha256:$(printf '%064d' 0) wrix-darwin:test" "$tmp/container.log" \
  || fail "Darwin live launcher did not tag the loaded OCI image"
grep -qF -- "run --rm --cap-add CAP_NET_ADMIN" "$tmp/container.log" \
  || fail "Darwin live launcher did not grant temporary NET_ADMIN for firewall setup"
printf 'PASS: Darwin live Rust launcher converts and loads a Docker archive\n'
