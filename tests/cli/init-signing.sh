#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-init-signing.XXXXXX)"
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
  command -v ssh-keygen >/dev/null 2>&1 || skip "ssh-keygen not on PATH"
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

assert_equals() {
  local label="$1"
  local actual="$2"
  local expected="$3"
  if [[ "$actual" != "$expected" ]]; then
    fail "$label: got '$actual', expected '$expected'"
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
  assert_equals "mode for $path" "$actual" "$expected"
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
  ssh-keygen -q -t ed25519 -N '' -f "$path" >/dev/null
  printf '%s\n' "$path"
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

run_init() {
  local repo="$1"
  local home="$2"
  local wrix_bin="$3"
  local bin_dir
  shift 3
  bin_dir="$(dirname "$wrix_bin")"
  (cd "$repo" && env -u WRIX_SIGNING_KEY -u GIT_AUTHOR_EMAIL -u GIT_COMMITTER_EMAIL PATH="$bin_dir:$PATH" HOME="$home" "$wrix_bin" init "$@")
}

assert_stable_config_value() {
  local label="$1"
  local value="$2"
  local repo="$3"
  local home="$4"
  assert_not_contains "$label" "$value" "$repo"
  assert_not_contains "$label" "$value" "$home"
  assert_not_contains "$label" "$value" "/nix/store"
  assert_not_contains "$label" "$value" "/etc/wrix/keys"
  assert_not_contains "$label" "$value" "/workspace"
  assert_not_contains "$label" "$value" ".ssh/deploy_keys"
}

test_signing_required_by_default() {
  require_tools
  local wrix_bin bin_dir repo home deploy_key signing_key output common_dir allowed_signers allowed_content public_key integration missing_repo missing_home missing_output no_sign_repo no_sign_home config_repo config_home
  wrix_bin="$(build_wrix)"
  bin_dir="$(dirname "$wrix_bin")"

  repo="$(setup_repo signing-default)"
  home="$TEST_TMP/home-signing"
  deploy_key="$(write_key "$home/.ssh/deploy_keys/signing-key")"
  signing_key="$(write_key "$home/.ssh/deploy_keys/signing-key-signing")"

  output="$(run_init "$repo" "$home" "$wrix_bin" --offline --key signing-key)"
  assert_contains "default signing output" "$output" "sign_commits: true"
  if [[ ! -f "$deploy_key" ]]; then
    fail "deploy key fixture was not created at $deploy_key"
  fi

  assert_equals "gpg.format" "$(git -C "$repo" config --get gpg.format)" "ssh"
  assert_equals "commit.gpgsign" "$(git -C "$repo" config --get commit.gpgsign)" "true"
  assert_equals "gpg.ssh.program" "$(git -C "$repo" config --get gpg.ssh.program)" "wrix-git-sign"
  assert_equals "gpg.ssh.allowedSignersFile" "$(git -C "$repo" config --get gpg.ssh.allowedSignersFile)" "wrix/allowed_signers"
  assert_equals "user.signingkey" "$(git -C "$repo" config --get user.signingkey)" "wrix/signing-key/signing-key-signing"

  assert_stable_config_value "gpg.ssh.program" "$(git -C "$repo" config --get gpg.ssh.program)" "$repo" "$home"
  assert_stable_config_value "gpg.ssh.allowedSignersFile" "$(git -C "$repo" config --get gpg.ssh.allowedSignersFile)" "$repo" "$home"
  assert_stable_config_value "user.signingkey" "$(git -C "$repo" config --get user.signingkey)" "$repo" "$home"

  common_dir="$(git_common_dir "$repo")"
  allowed_signers="$common_dir/wrix/allowed_signers"
  if [[ ! -f "$allowed_signers" ]]; then
    fail "allowed signers file was not generated at $allowed_signers"
  fi
  assert_mode "$allowed_signers" "600"
  public_key="$(ssh-keygen -y -f "$signing_key")"
  allowed_content="$(<"$allowed_signers")"
  assert_contains "allowed signers" "$allowed_content" "wrix-test@example.invalid $public_key"

  printf 'signed\n' >"$repo/signed.txt"
  git -C "$repo" add signed.txt
  env -u WRIX_SIGNING_KEY -u GIT_AUTHOR_EMAIL -u GIT_COMMITTER_EMAIL PATH="$bin_dir:$PATH" HOME="$home" git -C "$repo" commit -qm "signed commit"
  env -u WRIX_SIGNING_KEY -u GIT_AUTHOR_EMAIL -u GIT_COMMITTER_EMAIL PATH="$bin_dir:$PATH" HOME="$home" git -C "$repo" verify-commit HEAD >/dev/null

  mkdir -p "$repo/.loom"
  integration="$repo/.loom/integration"
  git -C "$repo" worktree add -q "$integration" -b loom-integration
  env -u WRIX_SIGNING_KEY -u GIT_AUTHOR_EMAIL -u GIT_COMMITTER_EMAIL PATH="$bin_dir:$PATH" HOME="$home" git -C "$integration" verify-commit HEAD >/dev/null

  missing_repo="$(setup_repo signing-missing-env)"
  missing_home="$TEST_TMP/home-missing-env"
  mkdir -p "$missing_home"
  missing_output="$TEST_TMP/missing-env.out"
  if (cd "$missing_repo" && WRIX_SIGNING_KEY="$TEST_TMP/absent-signing-key" PATH="$bin_dir:$PATH" HOME="$missing_home" "$wrix_bin" init --offline --key missing-key) >"$missing_output" 2>&1; then
    fail "wrix init succeeded with a missing WRIX_SIGNING_KEY"
  fi
  assert_contains "missing WRIX_SIGNING_KEY" "$(<"$missing_output")" "WRIX_SIGNING_KEY does not point at a file"

  missing_repo="$(setup_repo signing-missing-home)"
  missing_home="$TEST_TMP/home-missing-home"
  mkdir -p "$missing_home"
  missing_output="$TEST_TMP/missing-home.out"
  if (cd "$missing_repo" && env -u WRIX_SIGNING_KEY PATH="$bin_dir:$PATH" HOME="$missing_home" "$wrix_bin" init --offline --key missing-key) >"$missing_output" 2>&1; then
    fail "wrix init succeeded without a fallback signing key"
  fi
  assert_contains "missing home signing key" "$(<"$missing_output")" "fallback signing key does not exist"

  no_sign_repo="$(setup_repo signing-disabled-flag)"
  no_sign_home="$TEST_TMP/home-no-sign"
  write_key "$no_sign_home/.ssh/deploy_keys/no-sign-key" >/dev/null
  output="$(run_init "$no_sign_repo" "$no_sign_home" "$wrix_bin" --offline --key no-sign-key --no-sign)"
  assert_contains "--no-sign output" "$output" "sign_commits: false"
  assert_equals "--no-sign commit.gpgsign" "$(git -C "$no_sign_repo" config --get commit.gpgsign)" "false"

  config_repo="$(setup_repo signing-disabled-config)"
  config_home="$TEST_TMP/home-config-no-sign"
  write_key "$config_home/.ssh/deploy_keys/config-key" >/dev/null
  cat >"$config_repo/wrix.toml" <<'TOML'
[wrix.git]
sign_commits = false
TOML
  output="$(run_init "$config_repo" "$config_home" "$wrix_bin" --offline --key config-key)"
  assert_contains "config disabled output" "$output" "sign_commits: false"
  assert_equals "config disabled commit.gpgsign" "$(git -C "$config_repo" config --get commit.gpgsign)" "false"
}

ALL_TESTS=(
  test_signing_required_by_default
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
