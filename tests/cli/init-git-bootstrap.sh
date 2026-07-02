#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-init-git-bootstrap.XXXXXX)"
cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  return 1
}

build_wrix() {
  if [[ -n "${WRIX_BIN:-}" ]]; then
    printf '%s\n' "$WRIX_BIN"
    return 0
  fi

  local out_link="$TEST_TMP/wrix"
  nix build --no-warn-dirty --out-link "$out_link" "$REPO_ROOT#wrix"
  printf '%s\n' "$out_link/bin/wrix"
}

assert_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    fail "$label: missing '$needle' in output: $haystack"
  fi
}

assert_not_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" == *"$needle"* ]]; then
    fail "$label: unexpected '$needle' in output: $haystack"
  fi
}

mode_of() {
  local path="$1"
  case "$(uname -s)" in
    Darwin | FreeBSD) stat -f '%Lp' "$path" ;;
    *) stat -c '%a' "$path" ;;
  esac
}

assert_mode() {
  local path="$1"
  local expected="$2"
  local actual
  actual="$(mode_of "$path")"
  if [[ "$actual" != "$expected" ]]; then
    fail "$path has mode $actual, expected $expected"
  fi
}

setup_repo() {
  local name="$1"
  local repo="$TEST_TMP/$name"
  mkdir -p "$repo"
  git -C "$repo" init -q
  git -C "$repo" config user.name "Wrix Test"
  git -C "$repo" config user.email "wrix-test@example.invalid"
  printf 'initial\n' >"$repo/README.md"
  git -C "$repo" add README.md
  git -C "$repo" commit -qm "initial"
  git -C "$repo" remote add origin "git@github.com:example/$name.git"
  printf '%s\n' "$repo"
}

write_key() {
  local path="$1"
  local parent
  parent="$(dirname "$path")"
  mkdir -p "$parent"
  chmod 700 "$parent"
  : >"$path"
  chmod 600 "$path"
}

git_common_dir() {
  local repo="$1"
  local common_dir
  common_dir="$(git -C "$repo" rev-parse --git-common-dir)"
  if [[ "$common_dir" != /* ]]; then
    common_dir="$repo/$common_dir"
  fi
  (cd "$common_dir" && pwd -P)
}

core_ssh_command() {
  local repo="$1"
  git -C "$repo" config --get core.sshCommand
}

assert_strict_helper_args() {
  local label="$1"
  local args="$2"
  local key_path="$3"
  local known_hosts="$4"
  assert_contains "$label" "$args" $'-F\n/dev/null'
  assert_contains "$label" "$args" "BatchMode=yes"
  assert_contains "$label" "$args" "IdentitiesOnly=yes"
  assert_contains "$label" "$args" "StrictHostKeyChecking=yes"
  assert_contains "$label" "$args" "UserKnownHostsFile=$known_hosts"
  assert_contains "$label" "$args" "GlobalKnownHostsFile=/dev/null"
  assert_contains "$label" "$args" "IdentityAgent=none"
  assert_contains "$label" "$args" "IdentityFile=none"
  assert_contains "$label" "$args" $'-i\n'"$key_path"
  assert_not_contains "$label" "$args" "StrictHostKeyChecking=no"
}

write_fake_ssh() {
  local bin_dir="$1"
  mkdir -p "$bin_dir"
  cat >"$bin_dir/ssh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

: "${WRIX_TEST_CAPTURE:?}"
printf '%s\n' "$@" >"$WRIX_TEST_CAPTURE"
SH
  chmod 700 "$bin_dir/ssh"
}

test_common_config_inherited_by_loom_integration() {
  local wrix_bin repo home key output integration command linked_command origin common_dir state_dir
  wrix_bin="$(build_wrix)"
  repo="$(setup_repo common-config)"
  home="$TEST_TMP/home-common"
  key="$home/.ssh/deploy_keys/common-key"
  write_key "$key"

  output="$(cd "$repo" && HOME="$home" "$wrix_bin" init --offline --no-sign --key common-key)"
  assert_contains "init output" "$output" "wrix init: repository policy resolved"

  mkdir -p "$repo/.loom"
  integration="$repo/.loom/integration"
  git -C "$repo" worktree add -q "$integration" -b loom-integration

  command="$(core_ssh_command "$repo")"
  linked_command="$(core_ssh_command "$integration")"
  if [[ "$command" != "$linked_command" ]]; then
    fail "linked worktree did not inherit core.sshCommand"
  fi
  assert_contains "ssh command" "$command" "git rev-parse --git-common-dir"
  assert_not_contains "ssh command" "$command" "$repo"
  assert_not_contains "ssh command" "$command" "$key"
  assert_not_contains "ssh command" "$command" "/nix/store"
  assert_not_contains "ssh command" "$command" "/etc/wrix/keys"
  assert_not_contains "ssh command" "$command" "/workspace"
  assert_not_contains "ssh command" "$command" ".ssh/deploy_keys"

  common_dir="$(git_common_dir "$repo")"
  origin="$(git -C "$integration" config --show-origin --get core.sshCommand)"
  assert_contains "linked config origin" "$origin" "file:$common_dir/config"

  state_dir="$common_dir/wrix"
  if [[ ! -x "$state_dir/git-ssh" ]]; then
    fail "missing executable transport helper at $state_dir/git-ssh"
  fi
  if [[ ! -f "$state_dir/github_known_hosts" ]]; then
    fail "missing pinned GitHub known-hosts at $state_dir/github_known_hosts"
  fi
}

test_strict_context_aware_ssh_helper() {
  local wrix_bin repo home env_key home_key output common_dir state_dir helper known_hosts fake_bin capture args missing_home missing_output missing_capture known_hosts_content
  wrix_bin="$(build_wrix)"
  repo="$(setup_repo strict-helper)"
  home="$TEST_TMP/home-strict"
  env_key="$TEST_TMP/env-deploy-key"
  home_key="$home/.ssh/deploy_keys/strict-key"
  write_key "$env_key"
  write_key "$home_key"
  mkdir -p "$home/.ssh"
  printf 'Host github.com\n  IdentityFile %s\n' "$TEST_TMP/ambient-key" >"$home/.ssh/config"
  chmod 600 "$home/.ssh/config"

  output="$(cd "$repo" && HOME="$home" WRIX_DEPLOY_KEY="$env_key" "$wrix_bin" init --offline --no-sign --key strict-key)"
  assert_contains "init output" "$output" "wrix init: repository policy resolved"

  common_dir="$(git_common_dir "$repo")"
  state_dir="$common_dir/wrix"
  helper="$state_dir/git-ssh"
  known_hosts="$state_dir/github_known_hosts"
  assert_mode "$state_dir" "700"
  assert_mode "$helper" "700"
  assert_mode "$known_hosts" "600"
  known_hosts_content="$(<"$known_hosts")"
  assert_contains "known hosts" "$known_hosts_content" "github.com ssh-ed25519"

  fake_bin="$TEST_TMP/fake-bin"
  write_fake_ssh "$fake_bin"

  capture="$TEST_TMP/ssh-env.args"
  PATH="$fake_bin:$PATH" WRIX_TEST_CAPTURE="$capture" WRIX_DEPLOY_KEY="$env_key" HOME="$home" "$helper" git@github.com "git-upload-pack 'example/strict-helper.git'"
  args="$(<"$capture")"
  assert_contains "env deploy key" "$args" "$env_key"
  assert_not_contains "env deploy key" "$args" "$home_key"
  assert_strict_helper_args "env deploy key" "$args" "$env_key" "$known_hosts"

  capture="$TEST_TMP/ssh-home.args"
  env -u WRIX_DEPLOY_KEY \
    HOME="$home" \
    PATH="$fake_bin:$PATH" \
    WRIX_TEST_CAPTURE="$capture" \
    "$helper" git@github.com "git-upload-pack 'example/strict-helper.git'"
  args="$(<"$capture")"
  assert_contains "home deploy key" "$args" "$home_key"
  assert_not_contains "home deploy key" "$args" "$env_key"
  assert_strict_helper_args "home deploy key" "$args" "$home_key" "$known_hosts"

  missing_home="$TEST_TMP/missing-home"
  mkdir -p "$missing_home"
  missing_output="$TEST_TMP/missing.out"
  missing_capture="$TEST_TMP/missing.args"
  if env -u WRIX_DEPLOY_KEY \
    HOME="$missing_home" \
    PATH="$fake_bin:$PATH" \
    WRIX_TEST_CAPTURE="$missing_capture" \
    "$helper" git@github.com "git-upload-pack 'example/strict-helper.git'" \
    >"$missing_output" 2>&1; then
    fail "helper succeeded without any deploy key"
  fi
  output="$(<"$missing_output")"
  assert_contains "missing deploy key" "$output" "no deploy key resolved"
  if [[ -e "$missing_capture" ]]; then
    fail "helper invoked ssh after failing deploy-key resolution"
  fi
}

ALL_TESTS=(
  test_common_config_inherited_by_loom_integration
  test_strict_context_aware_ssh_helper
)

run_all() {
  local failed=0
  local fn
  for fn in "${ALL_TESTS[@]}"; do
    printf '=== %s ===\n' "$fn"
    if "$fn"; then
      printf 'PASS: %s\n' "$fn"
    else
      printf 'FAIL: %s\n' "$fn" >&2
      failed=$((failed + 1))
    fi
  done
  if [[ "$failed" -ne 0 ]]; then
    printf '%s test(s) failed\n' "$failed" >&2
    return 1
  fi
}

if [[ "$#" -eq 0 ]]; then
  run_all
else
  fn="$1"
  if ! declare -f "$fn" >/dev/null 2>&1; then
    printf 'Unknown function: %s\n' "$fn" >&2
    exit 1
  fi
  "$fn"
fi
