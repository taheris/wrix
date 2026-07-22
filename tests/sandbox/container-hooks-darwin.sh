#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=tests/lib/live-sandbox.sh
source "$SCRIPT_DIR/../lib/live-sandbox.sh"

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  exit 1
}

run_hook_case() (
  local stage="$1"
  local test_tmp workspace home_dir deploy_key profile_config spawn_config
  local launcher image_source image_ref sentinel command output

  wrix_require_live_sandbox_darwin
  cd "$REPO_ROOT"

  test_tmp=$(mktemp -d -t "wrix-container-$stage.XXXXXX")
  workspace="$test_tmp/workspace"
  home_dir="$test_tmp/home"
  deploy_key="$test_tmp/deploy-key"
  profile_config="$test_tmp/profile.json"
  spawn_config="$test_tmp/spawn.json"
  output="$test_tmp/run.log"
  image_ref=$(wrix_live_image_ref "container-$stage-$$")

  cleanup() {
    local status="$?"
    trap - EXIT
    if ! wrix_remove_image_ref "$image_ref"; then
      printf 'WARN: could not remove test image %s\n' "$image_ref" >&2
    fi
    rm -rf "$test_tmp"
    exit "$status"
  }
  trap cleanup EXIT

  mkdir -p "$workspace" "$home_dir"
  git -C "$workspace" init -q -b main
  git -C "$workspace" config user.email test@example.com
  git -C "$workspace" config user.name Test
  printf 'seed\n' >"$workspace/seed.txt"

  cat >"$workspace/.pre-commit-config.yaml" <<YAML
repos:
  - repo: local
    hooks:
      - id: wrapper-on-path
        name: wrapper-on-path
        entry: skip-if-missing nonexistent-tool-xyz -- false
        language: system
        stages: [$stage]
        always_run: true
        pass_filenames: false
      - id: sentinel
        name: sentinel
        entry: /workspace/.git/sentinel-$stage.sh
        language: system
        stages: [$stage]
        always_run: true
        pass_filenames: false
YAML

  cat >"$workspace/.git/sentinel-$stage.sh" <<SCRIPT
#!/usr/bin/env bash
set -euo pipefail
touch /workspace/.git/sentinel-fired-$stage
SCRIPT
  chmod 755 "$workspace/.git/sentinel-$stage.sh"
  sentinel="$workspace/.git/sentinel-fired-$stage"

  case "$stage" in
    pre-commit)
      command='cd /workspace && git add -A && git commit -m test'
      ;;
    pre-push)
      git -C "$workspace" init --bare -q remote.git
      git -C "$workspace" remote add origin file:///workspace/remote.git
      git -C "$workspace" add -A
      git -C "$workspace" commit -q -m initial
      command='cd /workspace && git push origin main'
      ;;
    *)
      fail "unsupported hook stage: $stage"
      ;;
  esac

  launcher=$(wrix_build_live_launcher)
  image_source=$(wrix_realize_test_image_source direct)
  wrix_write_profile_config "$profile_config" "$image_ref" "$image_source" direct
  wrix_write_spawn_config "$spawn_config" "$workspace" bash -lc "$command"
  wrix_make_ed25519_key "$deploy_key" "container-$stage-test"

  if ! HOME="$home_dir" WRIX_DEPLOY_KEY="$deploy_key" WRIX_GIT_SIGN=0 WRIX_NETWORK=open \
    wrix_run_spawn "$launcher" "$profile_config" "$spawn_config" >"$output" 2>&1; then
    cat "$output" >&2
    fail "profile-container $stage command returned non-zero"
  fi
  [[ -f "$sentinel" ]] || fail "$stage sentinel did not fire"

  if [[ "$stage" = "pre-push" ]]; then
    git -C "$workspace/remote.git" rev-parse refs/heads/main >/dev/null \
      || fail "profile-container push did not reach its bare remote"
  fi
)

test_pre_commit_fires_in_darwin_profile_container() {
  run_hook_case pre-commit
  printf 'PASS: pre-commit hook fires in a Darwin profile container\n' >&2
}

test_pre_push_fires_in_darwin_profile_container() {
  run_hook_case pre-push
  printf 'PASS: pre-push hook fires in a Darwin profile container\n' >&2
}

ALL_TESTS=(
  test_pre_commit_fires_in_darwin_profile_container
  test_pre_push_fires_in_darwin_profile_container
)

run_all() {
  local failed=0
  local test_name
  for test_name in "${ALL_TESTS[@]}"; do
    if ! bash "${BASH_SOURCE[0]}" "$test_name"; then
      failed=$((failed + 1))
    fi
  done
  [[ "$failed" -eq 0 ]]
}

if [[ "$#" -eq 0 ]]; then
  run_all
else
  test_name="$1"
  if ! declare -f "$test_name" >/dev/null 2>&1; then
    printf 'Unknown function: %s\n' "$test_name" >&2
    exit 1
  fi
  "$test_name"
fi
