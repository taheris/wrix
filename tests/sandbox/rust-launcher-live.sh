#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
CLEANUP_DIRS=()
trap 'cleanup_all' EXIT

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

cleanup_all() {
  local path
  for path in "${CLEANUP_DIRS[@]}"; do
    rm -rf "$path"
  done
}

register_tmp() {
  local path="$1"
  CLEANUP_DIRS+=("$path")
}

require_linux() {
  [[ "$(uname -s)" = "Linux" ]] || skip "Linux-only Rust launcher verifier"
  command -v cargo >/dev/null 2>&1 || skip "cargo not on PATH"
  command -v jq >/dev/null 2>&1 || skip "jq not on PATH"
}

wrix_bin() {
  local target_dir="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
  cargo build --quiet -p wrix-cli --bin wrix
  printf '%s\n' "$target_dir/debug/wrix"
}

setup_fake_runtime() {
  local tmp="$1"
  local state="$tmp/state"
  local bin="$tmp/bin"
  mkdir -p "$state" "$bin"
  : >"$state/podman.log"
  : >"$state/skopeo.log"
  : >"$state/rmi.log"

  cat >"$bin/podman" <<'PODMAN_SHIM'
#!/usr/bin/env bash
set -euo pipefail
state="${WRIX_TEST_STATE:?}"
printf '%s\n' "$*" >>"$state/podman.log"

image_id() {
  case "$1" in
    localhost/wrix-live:test|localhost/wrix-live:latest) printf 'live-id\n' ;;
    localhost/wrix-current:live) printf 'current-id\n' ;;
    localhost/wrix-recent:old) printf 'recent-id\n' ;;
    localhost/wrix-stale:old) printf 'stale-id\n' ;;
    *) printf '%s\n' "$1" ;;
  esac
}

image_digest() {
  case "$1" in
    localhost/wrix-live:test|localhost/wrix-live:latest|localhost/wrix-current:live) printf '%s\n' "${WRIX_TEST_DIGEST:?}" ;;
    *) printf '<no value>\n' ;;
  esac
}

managed_label() {
  case "$1" in
    localhost/wrix-live:test|localhost/wrix-live:latest|localhost/wrix-current:live|localhost/wrix-recent:old|localhost/wrix-stale:old) printf 'true\n' ;;
    *) printf '<no value>\n' ;;
  esac
}

case "${1:-}" in
  image)
    case "${2:-}" in
      inspect)
        target="${!#}"
        if [[ "$target" == sha256:* ]]; then
          [[ -f "$state/digest-present" ]] || exit 1
          printf '%s\n' "$target"
          exit 0
        fi
        [[ -f "$state/digest-present" || "$target" == localhost/wrix-* ]] || exit 1
        case "${4:-}" in
          '{{.Id}}') image_id "$target" ;;
          '{{.Digest}}') image_digest "$target" ;;
          '{{ index .Config.Labels "wrix.managed" }}') managed_label "$target" ;;
          *) printf 'sha256:fake\n' ;;
        esac
        ;;
      *) exit 0 ;;
    esac
    ;;
  images)
    if [[ -f "$state/images-list" ]]; then
      cat "$state/images-list"
    fi
    ;;
  info)
    printf 'overlay@%s/graph+%s/run\n' "$state" "$state"
    ;;
  ps)
    ;;
  rmi)
    printf '%s\n' "${2:-}" >>"$state/rmi.log"
    ;;
  run)
    : >"$state/container-ran"
    ;;
  tag)
    : >"$state/digest-present"
    ;;
  *) ;;
esac
PODMAN_SHIM
  chmod +x "$bin/podman"

  cat >"$bin/skopeo" <<'SKOPEO_SHIM'
#!/usr/bin/env bash
set -euo pipefail
state="${WRIX_TEST_STATE:?}"
printf '%s\n' "$*" >>"$state/skopeo.log"
for arg in "$@"; do
  case "$arg" in
    nix:*|docker-archive:*|oci-archive:*) exit 2 ;;
    containers-storage:*) : >"$state/digest-present" ;;
  esac
done
SKOPEO_SHIM
  chmod +x "$bin/skopeo"
}

write_profile_config() {
  local path="$1"
  local workspace="$2"
  local descriptor="$3"
  local digest="$4"
  local mount_source="${5:-}"
  local env_json="${6:-}"
  local mounts_json='[]'
  [[ -n "$env_json" ]] || env_json='{}'
  if [[ -n "$mount_source" ]]; then
    mounts_json=$(jq -n --arg source "$mount_source" '[{source:$source,dest:"/mnt/custom",mode:"ro"}]')
  fi
  jq -n \
    --arg source "$descriptor" \
    --arg digest "$digest" \
    --argjson env "$env_json" \
    --argjson mounts "$mounts_json" \
    '{schema:1,system:"test",profile:{name:"live",env:$env,mounts:$mounts,writable_dirs:[],network_allowlist:[]},image:{ref:"localhost/wrix-live:test",source:$source,source_kind:"nix-descriptor",digest:$digest},agent:{kind:"direct"},resources:{cpus:null,memory_mb:4096,pids_limit:4096},security:{deploy_key:null},services:{beads:{enable:"auto"},nix_cache:{enable:false}},features:{mcp_runtime:false}}' \
    >"$path"
  mkdir -p "$workspace"
}

write_descriptor() {
  local path="$1"
  local layout="$2"
  local digest="$3"
  mkdir -p "$layout"
  jq -n --arg digest "$digest" --arg layout "$layout" '{schema:1,source_kind:"nix-descriptor",digest:$digest,oci_layout:$layout,oci_ref:"latest"}' >"$path"
}

run_live_run() {
  local tmp="$1"
  local profile_config="$2"
  local workspace="$3"
  local digest="$4"
  local wrix="$5"
  PATH="$tmp/bin:$PATH" \
  HOME="$tmp/home" \
  XDG_CACHE_HOME="$tmp/cache" \
  WRIX_IMAGE_KEEP_FILE="$tmp/state/image-mru.json" \
  WRIX_TEST_STATE="$tmp/state" \
  WRIX_TEST_DIGEST="$digest" \
    "$wrix" --profile-config "$profile_config" run "$workspace" true
}

run_live_spawn() {
  local tmp="$1"
  local profile_config="$2"
  local workspace="$3"
  local digest="$4"
  local wrix="$5"
  local spawn_config="$tmp/spawn.json"
  local deploy_key="$tmp/deploy-key"
  printf 'deploy-key\n' >"$deploy_key"
  jq -n --arg workspace "$workspace" '{workspace:$workspace,env:[],agent_args:["true"],mounts:[]}' >"$spawn_config"
  PATH="$tmp/bin:$PATH" \
  HOME="$tmp/home" \
  XDG_CACHE_HOME="$tmp/cache" \
  WRIX_DEPLOY_KEY="$deploy_key" \
  WRIX_GIT_SIGN=0 \
  WRIX_IMAGE_KEEP_FILE="$tmp/state/image-mru.json" \
  WRIX_TEST_STATE="$tmp/state" \
  WRIX_TEST_DIGEST="$digest" \
    "$wrix" --profile-config "$profile_config" spawn --spawn-config "$spawn_config"
}

test_linux_custom_mounts_env_reach_live_launcher() {
  require_linux
  local tmp wrix workspace descriptor layout profile_config mount_dir digest
  tmp=$(mktemp -d -t wrix-live.XXXXXX)
  register_tmp "$tmp"
  setup_fake_runtime "$tmp"
  wrix=$(wrix_bin)
  workspace="$tmp/workspace"
  descriptor="$tmp/descriptor.json"
  layout="$tmp/oci-layout"
  profile_config="$tmp/profile.json"
  mount_dir="$tmp/mount-dir"
  digest="sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  mkdir -p "$mount_dir"
  printf 'payload\n' >"$mount_dir/payload"
  write_descriptor "$descriptor" "$layout" "$digest"
  write_profile_config "$profile_config" "$workspace" "$descriptor" "$digest" "$mount_dir" '{"WRIX_TEST_ENV":"live-value"}'

  run_live_run "$tmp" "$profile_config" "$workspace" "$digest" "$wrix"

  [[ -f "$tmp/state/container-ran" ]] || fail "live launcher did not reach podman run"
  grep -q -- '-e WRIX_TEST_ENV=live-value' "$tmp/state/podman.log" || fail "custom env was not passed to podman run"
  grep -q -- ':/mnt/custom:ro' "$tmp/state/podman.log" || fail "custom mount was not passed to podman run"
  printf 'PASS: custom mounts/env reached the live Rust launcher container argv\n'
}

test_linux_sets_is_sandbox_without_fakeuid() {
  require_linux
  local tmp wrix workspace descriptor layout profile_config digest
  tmp=$(mktemp -d -t wrix-live.XXXXXX)
  register_tmp "$tmp"
  setup_fake_runtime "$tmp"
  wrix=$(wrix_bin)
  workspace="$tmp/workspace"
  descriptor="$tmp/descriptor.json"
  layout="$tmp/oci-layout"
  profile_config="$tmp/profile.json"
  digest="sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
  write_descriptor "$descriptor" "$layout" "$digest"
  write_profile_config "$profile_config" "$workspace" "$descriptor" "$digest"

  run_live_run "$tmp" "$profile_config" "$workspace" "$digest" "$wrix"

  grep -q -- '-e IS_SANDBOX=1' "$tmp/state/podman.log" || fail "IS_SANDBOX=1 was not passed on the default Linux boundary"
  if grep -q -- 'LD_PRELOAD=/lib/libfakeuid.so' "$tmp/state/podman.log"; then
    fail "default Linux boundary passed libfakeuid LD_PRELOAD"
  fi
  printf 'PASS: live Rust Linux launcher sets IS_SANDBOX without libfakeuid\n'
}

test_linux_archiveless_install_uses_oci_layout() {
  require_linux
  local tmp wrix workspace descriptor layout profile_config digest
  tmp=$(mktemp -d -t wrix-live.XXXXXX)
  register_tmp "$tmp"
  setup_fake_runtime "$tmp"
  wrix=$(wrix_bin)
  workspace="$tmp/workspace"
  descriptor="$tmp/descriptor.json"
  layout="$tmp/oci-layout"
  profile_config="$tmp/profile.json"
  digest="sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
  write_descriptor "$descriptor" "$layout" "$digest"
  write_profile_config "$profile_config" "$workspace" "$descriptor" "$digest"

  run_live_run "$tmp" "$profile_config" "$workspace" "$digest" "$wrix"

  grep -qF -- "oci:$layout:latest" "$tmp/state/skopeo.log" || fail "live launcher did not copy descriptor OCI layout"
  if grep -qE '(^| )(nix:|(docker|oci)-archive:|load($| ))' "$tmp/state/skopeo.log" "$tmp/state/podman.log"; then
    fail "live launcher used a legacy nix/archive/load transport"
  fi
  printf 'PASS: live Rust Linux launcher uses OCI-layout descriptor install\n'
}

test_linux_second_spawn_skips_loaded_image() {
  require_linux
  local tmp wrix workspace descriptor layout profile_config digest
  tmp=$(mktemp -d -t wrix-live.XXXXXX)
  register_tmp "$tmp"
  setup_fake_runtime "$tmp"
  wrix=$(wrix_bin)
  workspace="$tmp/workspace"
  descriptor="$tmp/descriptor.json"
  layout="$tmp/oci-layout"
  profile_config="$tmp/profile.json"
  digest="sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
  write_descriptor "$descriptor" "$layout" "$digest"
  write_profile_config "$profile_config" "$workspace" "$descriptor" "$digest"

  run_live_spawn "$tmp" "$profile_config" "$workspace" "$digest" "$wrix"
  [[ -s "$tmp/state/skopeo.log" ]] || fail "first spawn did not install the descriptor image"
  : >"$tmp/state/skopeo.log"
  : >"$tmp/state/podman.log"
  run_live_spawn "$tmp" "$profile_config" "$workspace" "$digest" "$wrix"

  [[ ! -s "$tmp/state/skopeo.log" ]] || fail "second spawn invoked skopeo despite digest hit"
  grep -qFx -- "image inspect --format {{.Id}} $digest" "$tmp/state/podman.log" || fail "second spawn did not preflight the content digest"
  printf 'PASS: second live Rust spawn skips an already-loaded image\n'
}

test_linux_delta_bounded_uses_descriptor_transport() {
  require_linux
  local tmp wrix workspace descriptor_a descriptor_b layout_a layout_b profile_a profile_b digest_a digest_b
  tmp=$(mktemp -d -t wrix-live.XXXXXX)
  register_tmp "$tmp"
  setup_fake_runtime "$tmp"
  wrix=$(wrix_bin)
  workspace="$tmp/workspace"
  descriptor_a="$tmp/descriptor-a.json"
  descriptor_b="$tmp/descriptor-b.json"
  layout_a="$tmp/oci-layout-a"
  layout_b="$tmp/oci-layout-b"
  profile_a="$tmp/profile-a.json"
  profile_b="$tmp/profile-b.json"
  digest_a="sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
  digest_b="sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
  write_descriptor "$descriptor_a" "$layout_a" "$digest_a"
  write_descriptor "$descriptor_b" "$layout_b" "$digest_b"
  write_profile_config "$profile_a" "$workspace" "$descriptor_a" "$digest_a"
  write_profile_config "$profile_b" "$workspace" "$descriptor_b" "$digest_b"

  run_live_spawn "$tmp" "$profile_a" "$workspace" "$digest_a" "$wrix"
  rm -f "$tmp/state/digest-present"
  run_live_spawn "$tmp" "$profile_b" "$workspace" "$digest_b" "$wrix"

  grep -qF -- "oci:$layout_a:latest" "$tmp/state/skopeo.log" || fail "first changed image did not use descriptor OCI layout"
  grep -qF -- "oci:$layout_b:latest" "$tmp/state/skopeo.log" || fail "second changed image did not use descriptor OCI layout"
  if grep -qE '(^| )(nix:|(docker|oci)-archive:|load($| ))' "$tmp/state/skopeo.log" "$tmp/state/podman.log"; then
    fail "delta path used a legacy whole-archive transport"
  fi
  printf 'PASS: live Rust delta install path uses descriptor transport for changed images\n'
}

test_linux_image_retention_cleanup_live_path() {
  require_linux
  local tmp wrix workspace descriptor layout profile_config digest first_ref
  tmp=$(mktemp -d -t wrix-live.XXXXXX)
  register_tmp "$tmp"
  setup_fake_runtime "$tmp"
  wrix=$(wrix_bin)
  workspace="$tmp/workspace"
  descriptor="$tmp/descriptor.json"
  layout="$tmp/oci-layout"
  profile_config="$tmp/profile.json"
  digest="sha256:1212121212121212121212121212121212121212121212121212121212121212"
  write_descriptor "$descriptor" "$layout" "$digest"
  write_profile_config "$profile_config" "$workspace" "$descriptor" "$digest"
  cat >"$tmp/state/image-mru.json" <<'JSON'
[{"ref":"localhost/wrix-recent:old","id":"recent-id"}]
JSON
  cat >"$tmp/state/images-list" <<'IMAGES'
localhost/wrix-live test live-id
localhost/wrix-recent old recent-id
localhost/wrix-stale old stale-id
docker.io/library/ubuntu latest user-id
IMAGES

  run_live_run "$tmp" "$profile_config" "$workspace" "$digest" "$wrix"

  first_ref=$(jq -r '.[0].ref' "$tmp/state/image-mru.json")
  [[ "$first_ref" = "localhost/wrix-live:test" ]] || fail "MRU did not record the live launcher image first"
  grep -qFx -- 'localhost/wrix-stale:old' "$tmp/state/rmi.log" || fail "stale wrix image was not pruned"
  if grep -qFx -- 'localhost/wrix-recent:old' "$tmp/state/rmi.log"; then
    fail "recent MRU image was pruned"
  fi
  if grep -qFx -- 'docker.io/library/ubuntu:latest' "$tmp/state/rmi.log"; then
    fail "non-wrix user image was pruned"
  fi
  printf 'PASS: live Rust launcher records MRU and prunes stale wrix images\n'
}

if [[ $# -eq 0 ]]; then
  test_linux_custom_mounts_env_reach_live_launcher
  test_linux_sets_is_sandbox_without_fakeuid
  test_linux_archiveless_install_uses_oci_layout
  test_linux_second_spawn_skips_loaded_image
  test_linux_delta_bounded_uses_descriptor_transport
  test_linux_image_retention_cleanup_live_path
else
  "$1"
fi
