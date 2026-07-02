#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-init-verify.XXXXXX)"
cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  return 1
}

skip() {
  local message="$1"
  printf 'SKIP: %s\n' "$message" >&2
  exit 77
}

require_tools() {
  command -v git >/dev/null 2>&1 || skip "git not on PATH"
  command -v ssh >/dev/null 2>&1 || skip "ssh not on PATH"
}

build_wrix() {
  if [[ -n "${WRIX_BIN:-}" ]]; then
    printf '%s\n' "$WRIX_BIN"
    return 0
  fi

  command -v nix >/dev/null 2>&1 || skip "nix not on PATH and WRIX_BIN is unset"
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

assert_file_absent() {
  local path="$1"
  if [[ -e "$path" ]]; then
    fail "unexpected file exists: $path"
  fi
}

canonical_dir() {
  local path="$1"
  (cd "$path" && pwd -P)
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

add_integration_worktree() {
  local repo="$1"
  local integration="$repo/.loom/integration"
  mkdir -p "$repo/.loom"
  git -C "$repo" worktree add -q "$integration" -b "loom-integration-$(basename "$repo")"
  canonical_dir "$integration"
}

write_deploy_key() {
  local home="$1"
  local mode="$2"
  local key="$home/.ssh/deploy_keys/verify-key"
  mkdir -p "$(dirname "$key")"
  chmod 700 "$home/.ssh" "$home/.ssh/deploy_keys"
  : >"$key"
  chmod "$mode" "$key"
  printf '%s\n' "$key"
}

write_fake_git() {
  local real_git="$1"
  local mode_file="$2"
  local capture_dir="$3"
  local bin_dir="$TEST_TMP/fake-git"
  mkdir -p "$bin_dir"
  cat >"$bin_dir/git" <<SH
#!/usr/bin/env bash
set -euo pipefail

for arg in "\$@"; do
  if [[ "\$arg" == "ls-remote" ]]; then
    mkdir -p "$capture_dir"
    pwd -P >"$capture_dir/cwd"
    printf '%s\n' "\$@" >"$capture_dir/args"
    env | sort >"$capture_dir/env"
    mode="\$(<"$mode_file")"
    case "\$mode" in
      success)
        printf '%s\tHEAD\n' "0123456789012345678901234567890123456789"
        exit 0
        ;;
      host-key)
        printf 'Host key verification failed.\n' >&2
        exit 128
        ;;
      auth)
        printf 'Permission denied (publickey).\nfatal: Could not read from remote repository.\n' >&2
        exit 128
        ;;
      fail-if-online)
        printf 'unexpected online verification\n' >&2
        exit 99
        ;;
      *)
        printf 'unknown fake git mode: %s\n' "\$mode" >&2
        exit 98
        ;;
    esac
  fi
done
exec "$real_git" "\$@"
SH
  chmod 700 "$bin_dir/git"
  printf '%s\n' "$bin_dir"
}

reset_capture() {
  local capture_dir="$1"
  rm -rf "$capture_dir"
  mkdir -p "$capture_dir"
}

run_init() {
  local repo="$1"
  local home="$2"
  local deploy_key="$3"
  local wrix_bin="$4"
  local fake_git="$5"
  local bin_dir path_value
  shift 5
  bin_dir="$(dirname "$wrix_bin")"
  path_value="$fake_git:$bin_dir:$PATH"
  (cd "$repo" && GIT_SSH_COMMAND="ambient ssh" SSH_AUTH_SOCK="$TEST_TMP/agent.sock" WRIX_SHOULD_NOT_LEAK=1 PATH="$path_value" HOME="$home" WRIX_DEPLOY_KEY="$deploy_key" "$wrix_bin" init "$@")
}

expect_failure() {
  local output_file="$1"
  shift
  if "$@" >"$output_file" 2>&1; then
    fail "expected command to fail: $*"
  fi
}

assert_online_capture() {
  local capture_dir="$1"
  local expected_cwd="$2"
  local deploy_key="$3"
  local home="$4"
  local cwd args env_output
  cwd="$(<"$capture_dir/cwd")"
  args="$(<"$capture_dir/args")"
  env_output="$(<"$capture_dir/env")"
  if [[ "$cwd" != "$expected_cwd" ]]; then
    fail "online verifier cwd was $cwd, expected $expected_cwd"
  fi
  assert_contains "ls-remote args" "$args" "ls-remote"
  assert_contains "ls-remote args" "$args" "origin"
  assert_contains "ls-remote args" "$args" "HEAD"
  assert_contains "online env" "$env_output" "GIT_CONFIG_GLOBAL=/dev/null"
  assert_contains "online env" "$env_output" "GIT_CONFIG_NOSYSTEM=1"
  assert_contains "online env" "$env_output" "GIT_TERMINAL_PROMPT=0"
  assert_contains "online env" "$env_output" "GIT_SSH_VARIANT=ssh"
  assert_contains "online env" "$env_output" "HOME=$home"
  assert_contains "online env" "$env_output" "WRIX_DEPLOY_KEY=$deploy_key"
  assert_not_contains "online env" "$env_output" "GIT_SSH_COMMAND="
  assert_not_contains "online env" "$env_output" "SSH_AUTH_SOCK="
  assert_not_contains "online env" "$env_output" "WRIX_SHOULD_NOT_LEAK="
}

assert_fake_git_contract() {
  local fake_git="$1"
  local real_git="$2"
  local mode_file="$3"
  local real_version fake_version output
  printf 'success\n' >"$mode_file"
  real_version="$("$real_git" --version)"
  fake_version="$(PATH="$fake_git:$PATH" git --version)"
  if [[ "$fake_version" != "$real_version" ]]; then
    fail "fake git did not delegate --version"
  fi
  output="$(PATH="$fake_git:$PATH" git ls-remote origin HEAD)"
  assert_contains "fake git ls-remote" "$output" $'\tHEAD'
}

test_online_and_offline_verification() {
  require_tools
  local wrix_bin real_git mode_file capture_dir fake_git repo integration home deploy_key output command output_file config_repo bad_repo bad_home bad_key
  wrix_bin="$(build_wrix)"
  real_git="$(command -v git)"
  mode_file="$TEST_TMP/git-mode"
  capture_dir="$TEST_TMP/online-capture"
  fake_git="$(write_fake_git "$real_git" "$mode_file" "$capture_dir")"
  assert_fake_git_contract "$fake_git" "$real_git" "$mode_file"

  repo="$(setup_repo online-success)"
  integration="$(add_integration_worktree "$repo")"
  home="$TEST_TMP/home-online"
  mkdir -p "$home"
  deploy_key="$(write_deploy_key "$home" 600)"
  printf 'success\n' >"$mode_file"
  reset_capture "$capture_dir"
  output="$(run_init "$repo" "$home" "$deploy_key" "$wrix_bin" "$fake_git" --no-sign --key verify-key)"
  assert_contains "online output" "$output" "online_verify: true"
  assert_online_capture "$capture_dir" "$integration" "$deploy_key" "$home"
  command="$(git -C "$integration" config --get core.sshCommand)"
  assert_contains "linked helper config" "$command" "wrix/git-ssh"

  repo="$(setup_repo offline-flag)"
  home="$TEST_TMP/home-offline"
  mkdir -p "$home"
  deploy_key="$(write_deploy_key "$home" 600)"
  printf 'fail-if-online\n' >"$mode_file"
  reset_capture "$capture_dir"
  output="$(run_init "$repo" "$home" "$deploy_key" "$wrix_bin" "$fake_git" --offline --no-sign --key verify-key)"
  assert_contains "offline output" "$output" "online_verify: false"
  assert_file_absent "$capture_dir/cwd"

  config_repo="$(setup_repo offline-config)"
  cat >"$config_repo/wrix.toml" <<'TOML'
[wrix.init]
online_verify = false
TOML
  reset_capture "$capture_dir"
  output="$(run_init "$config_repo" "$home" "$deploy_key" "$wrix_bin" "$fake_git" --no-sign --key verify-key)"
  assert_contains "config offline output" "$output" "online_verify: false"
  assert_file_absent "$capture_dir/cwd"

  bad_repo="$(setup_repo offline-local-check)"
  bad_home="$TEST_TMP/home-bad-perms"
  mkdir -p "$bad_home"
  bad_key="$(write_deploy_key "$bad_home" 644)"
  reset_capture "$capture_dir"
  output_file="$TEST_TMP/bad-perms.out"
  expect_failure "$output_file" run_init "$bad_repo" "$bad_home" "$bad_key" "$wrix_bin" "$fake_git" --offline --no-sign --key verify-key
  output="$(<"$output_file")"
  assert_contains "offline local permissions" "$output" "deploy key"
  assert_contains "offline local permissions" "$output" "no group or other permissions"
  assert_file_absent "$capture_dir/cwd"

  repo="$(setup_repo host-key-failure)"
  home="$TEST_TMP/home-host-key"
  mkdir -p "$home"
  deploy_key="$(write_deploy_key "$home" 600)"
  printf 'host-key\n' >"$mode_file"
  output_file="$TEST_TMP/host-key.out"
  expect_failure "$output_file" run_init "$repo" "$home" "$deploy_key" "$wrix_bin" "$fake_git" --no-sign --key verify-key
  output="$(<"$output_file")"
  assert_contains "host-key failure" "$output" "online verification failed host-key verification"
  assert_not_contains "host-key failure" "$output" "authentication or repository authorization failed"

  repo="$(setup_repo auth-failure)"
  home="$TEST_TMP/home-auth"
  mkdir -p "$home"
  deploy_key="$(write_deploy_key "$home" 600)"
  printf 'auth\n' >"$mode_file"
  output_file="$TEST_TMP/auth.out"
  expect_failure "$output_file" run_init "$repo" "$home" "$deploy_key" "$wrix_bin" "$fake_git" --no-sign --key verify-key
  output="$(<"$output_file")"
  assert_contains "auth failure" "$output" "authentication or repository authorization failed"
  assert_not_contains "auth failure" "$output" "failed host-key verification"
}

ALL_TESTS=(
  test_online_and_offline_verification
)

run_all() {
  local failed=0
  local fn status
  for fn in "${ALL_TESTS[@]}"; do
    printf '=== %s ===\n' "$fn"
    if "$fn"; then
      printf 'PASS: %s\n' "$fn"
    else
      status="$?"
      if [[ "$status" -eq 77 ]]; then
        printf 'SKIP: %s\n' "$fn" >&2
      else
        printf 'FAIL: %s\n' "$fn" >&2
        failed=$((failed + 1))
      fi
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
