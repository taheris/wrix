#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=tests/lib/live-sandbox.sh
source "$SCRIPT_DIR/../lib/live-sandbox.sh"

wrix_require_live_sandbox
if ! command -v ssh >/dev/null 2>&1; then
  wrix_live_skip "ssh not on PATH"
fi
cd "$REPO_ROOT"

TEST_TMP=$(mktemp -d -t wrix-git-ssh-bootstrap.XXXXXX)
cleanup() {
  rm -rf "$TEST_TMP"
  wrix_remove_image_ref "${IMAGE_REF:-}"
}
trap cleanup EXIT

PASSED=0
FAILED=0

pass() {
  local message="$1"
  printf '  PASS: %s\n' "$message"
  PASSED=$((PASSED + 1))
}

fail() {
  local message="$1"
  printf '  FAIL: %s\n' "$message" >&2
  FAILED=$((FAILED + 1))
}

assert_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    fail "$label: missing '$needle'"
    return 1
  fi
}

assert_not_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" == *"$needle"* ]]; then
    fail "$label: unexpected '$needle'"
    return 1
  fi
}

assert_equals() {
  local label="$1"
  local actual="$2"
  local expected="$3"
  if [[ "$actual" != "$expected" ]]; then
    fail "$label: got '$actual', expected '$expected'"
    return 1
  fi
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

write_host_key() {
  local path="$1"
  local comment="$2"
  local parent
  parent="$(dirname "$path")"
  mkdir -p "$parent"
  chmod 700 "$parent"
  wrix_make_ed25519_key "$path" "$comment"
  chmod 600 "$path"
}

setup_parity_repo() {
  local repo="$1"
  mkdir -p "$repo"
  git -C "$repo" init -q
  git -C "$repo" config user.name "Wrix Parity"
  git -C "$repo" config user.email "parity@example.invalid"
  printf 'initial\n' >"$repo/README.md"
  git -C "$repo" add README.md
  git -C "$repo" commit -qm "initial"
  git -C "$repo" remote add origin "git@github.com:example/wrix-parity.git"
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

write_ambient_ssh_home() {
  local home="$1"
  local default_key="$home/.ssh/id_ed25519"
  local config_key="$home/.ssh/config-identity"
  mkdir -p "$home/.ssh"
  chmod 700 "$home/.ssh"
  wrix_make_ed25519_key "$default_key" "ambient-default"
  wrix_make_ed25519_key "$config_key" "ambient-config"
  cat >"$home/.ssh/config" <<EOF
Host github.com
  IdentityFile $config_key
EOF
  chmod 600 "$home/.ssh/config"
}

assert_strict_helper_args() {
  local label="$1"
  local args="$2"
  local key_path="$3"
  local known_hosts="$4"
  local forbidden_identity="$5"

  assert_contains "$label" "$args" $'-F\n/dev/null' || return 1
  assert_contains "$label" "$args" "BatchMode=yes" || return 1
  assert_contains "$label" "$args" "IdentitiesOnly=yes" || return 1
  assert_contains "$label" "$args" "StrictHostKeyChecking=yes" || return 1
  assert_contains "$label" "$args" "UserKnownHostsFile=$known_hosts" || return 1
  assert_contains "$label" "$args" "GlobalKnownHostsFile=/dev/null" || return 1
  assert_contains "$label" "$args" "IdentityAgent=none" || return 1
  assert_contains "$label" "$args" "IdentityFile=none" || return 1
  assert_contains "$label" "$args" $'-i\n'"$key_path" || return 1
  assert_not_contains "$label" "$args" "StrictHostKeyChecking=no" || return 1
  assert_not_contains "$label" "$args" "$forbidden_identity" || return 1
}

probe_host_github_auth() {
  local helper="$1"
  local key_path="$2"
  local home="$3"
  local out="$TEST_TMP/host-github.out"
  local err="$TEST_TMP/host-github.err"
  local rc detail lower

  rc=0
  if PATH="$PATH" HOME="$home" WRIX_DEPLOY_KEY="$key_path" \
    "$helper" -o ConnectTimeout=15 -T git@github.com >"$out" 2>"$err"; then
    rc=0
  else
    rc=$?
  fi

  detail="$(cat "$out" "$err")"
  lower="$(printf '%s' "$detail" | tr '[:upper:]' '[:lower:]')"
  case "$lower" in
    *"host key verification failed"* | *"no ed25519 host key is known"* | *"no ecdsa host key is known"* | *"no rsa host key is known"* | *"remote host identification has changed"*)
      fail "host GitHub SSH probe failed host-key verification separately: $detail"
      return 1
      ;;
  esac
  case "$lower" in
    *"permission denied (publickey)"* | *"successfully authenticated"* | *"repository not found"* | *"authentication failed"* | *"access denied"*)
      return 0
      ;;
  esac

  fail "host GitHub SSH probe did not reach authentication or repository authorization (rc=$rc): $detail"
  return 1
}

write_probe() {
  local path="$1"
  cat >"$path" <<'PROBE'
#!/usr/bin/env bash
set -euo pipefail

fail_probe() {
  local message="$1"
  printf 'bootstrap probe failed: %s\n' "$message" >&2
  exit 1
}

assert_eq() {
  local label="$1"
  local actual="$2"
  local expected="$3"
  [[ "$actual" = "$expected" ]] || fail_probe "$label: expected '$expected', got '$actual'"
}

name=$(git config --global user.name)
email=$(git config --global user.email)
assert_eq "user.name" "$name" "Smoke Author"
assert_eq "user.email" "$email" "smoke@example.test"

[[ -f /etc/ssh/ssh_known_hosts ]] || fail_probe "/etc/ssh/ssh_known_hosts is missing"
grep -q '^github.com ' /etc/ssh/ssh_known_hosts || fail_probe "GitHub host keys are missing"

ssh_command=$(git config --global --get core.sshCommand)
case "$ssh_command" in
  *"-i $WRIX_DEPLOY_KEY"*"IdentitiesOnly=yes"*"StrictHostKeyChecking=yes"*"UserKnownHostsFile=/etc/ssh/ssh_known_hosts"*) ;;
  *) fail_probe "unexpected core.sshCommand: $ssh_command" ;;
esac
case "${GIT_SSH_COMMAND:-}" in
  *"-i $WRIX_DEPLOY_KEY"*"IdentitiesOnly=yes"*"StrictHostKeyChecking=yes"*"UserKnownHostsFile=/etc/ssh/ssh_known_hosts"*) ;;
  *) fail_probe "unexpected GIT_SSH_COMMAND: ${GIT_SSH_COMMAND:-}" ;;
esac

assert_eq "gpg.format" "$(git config --global --get gpg.format)" "ssh"
assert_eq "user.signingkey" "$(git config --global --get user.signingkey)" "$WRIX_SIGNING_KEY"
assert_eq "commit.gpgsign" "$(git config --global --get commit.gpgsign)" "true"
allowed_signers=$(git config --global --get gpg.ssh.allowedSignersFile)
[[ -f "$allowed_signers" ]] || fail_probe "allowed_signers is missing"
grep -q '^smoke@example.test ' "$allowed_signers" || fail_probe "allowed_signers lacks smoke@example.test"

ssh_out=$(mktemp)
ssh_err=$(mktemp)
ssh_rc=0
if ssh -o BatchMode=yes -o ConnectTimeout=15 -T git@github.com >"$ssh_out" 2>"$ssh_err"; then
  ssh_rc=0
else
  ssh_rc=$?
fi
if grep -Eiq 'Host key verification failed|No .* host key is known|REMOTE HOST IDENTIFICATION HAS CHANGED' "$ssh_err"; then
  sed 's/^/ssh: /' "$ssh_err" >&2
  fail_probe "ssh -T failed host-key verification"
fi
if grep -Fq 'Permission denied (publickey)' "$ssh_err"; then
  printf 'ssh -T git@github.com reached GitHub authentication after host-key verification (rc=%s)\n' "$ssh_rc"
elif grep -Eiq 'successfully authenticated' "$ssh_out" || grep -Eiq 'successfully authenticated' "$ssh_err"; then
  printf 'ssh -T git@github.com authenticated after host-key verification (rc=%s)\n' "$ssh_rc"
else
  sed 's/^/ssh stdout: /' "$ssh_out" >&2
  sed 's/^/ssh stderr: /' "$ssh_err" >&2
  fail_probe "ssh -T did not reach GitHub authentication with pinned host-key verification (rc=$ssh_rc)"
fi

rm -rf smoke-repo
git init -q smoke-repo
cd smoke-repo
git commit --allow-empty -q -m smoke
signature=$(git log -1 --show-signature 2>&1)
printf '%s\n' "$signature"
case "$signature" in
  *'Good "git" signature'*) ;;
  *) fail_probe "git log did not report a good SSH signature" ;;
esac
PROBE
  chmod +x "$path"
}

write_parity_container_probe() {
  local path="$1"
  cat >"$path" <<'PROBE'
#!/usr/bin/env bash
set -euo pipefail

fail_probe() {
  local message="$1"
  printf 'container parity probe failed: %s\n' "$message" >&2
  exit 1
}

assert_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  [[ "$haystack" == *"$needle"* ]] || fail_probe "$label missing '$needle'"
}

assert_not_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  [[ "$haystack" != *"$needle"* ]] || fail_probe "$label unexpectedly contained '$needle'"
}

assert_eq() {
  local label="$1"
  local actual="$2"
  local expected="$3"
  [[ "$actual" = "$expected" ]] || fail_probe "$label: expected '$expected', got '$actual'"
}

cd /workspace
case "${WRIX_DEPLOY_KEY:-}" in
  /etc/wrix/keys/*) ;;
  *) fail_probe "WRIX_DEPLOY_KEY was not staged under /etc/wrix/keys: ${WRIX_DEPLOY_KEY:-}" ;;
esac
case "${WRIX_SIGNING_KEY:-}" in
  /etc/wrix/keys/*-signing) ;;
  *) fail_probe "WRIX_SIGNING_KEY was not staged under /etc/wrix/keys: ${WRIX_SIGNING_KEY:-}" ;;
esac

ssh_command="$(git config --get core.sshCommand)"
assert_contains "core.sshCommand" "$ssh_command" "git rev-parse --git-common-dir"
assert_contains "core.sshCommand" "$ssh_command" "wrix/git-ssh"
assert_not_contains "core.sshCommand" "$ssh_command" "/etc/wrix/keys"
assert_not_contains "core.sshCommand" "$ssh_command" ".ssh/deploy_keys"
assert_eq "gpg.ssh.program" "$(git config --get gpg.ssh.program)" "wrix-git-sign"
assert_eq "gpg.ssh.allowedSignersFile" "$(git config --get gpg.ssh.allowedSignersFile)" "wrix/allowed_signers"
assert_eq "user.signingkey" "$(git config --get user.signingkey)" "wrix/signing-key/parity-key-signing"

common_dir=$(git rev-parse --git-common-dir)
if [[ "$common_dir" != /* ]]; then
  common_dir="/workspace/$common_dir"
fi
helper="$common_dir/wrix/git-ssh"
known_hosts="$common_dir/wrix/github_known_hosts"
[[ -x "$helper" ]] || fail_probe "missing executable helper: $helper"
[[ -f "$known_hosts" ]] || fail_probe "missing pinned known-hosts: $known_hosts"

capture=/workspace/container-ssh.args
rm -f "$capture"
SSH_AUTH_SOCK=/workspace/agent.sock PATH="/workspace/fake-bin:$PATH" WRIX_TEST_CAPTURE="$capture" \
  "$helper" git@github.com "git-upload-pack 'example/container.git'"
args="$(<"$capture")"
assert_contains "container helper args" "$args" $'-F\n/dev/null'
assert_contains "container helper args" "$args" "BatchMode=yes"
assert_contains "container helper args" "$args" "IdentitiesOnly=yes"
assert_contains "container helper args" "$args" "StrictHostKeyChecking=yes"
assert_contains "container helper args" "$args" "UserKnownHostsFile=$known_hosts"
assert_contains "container helper args" "$args" "GlobalKnownHostsFile=/dev/null"
assert_contains "container helper args" "$args" "IdentityAgent=none"
assert_contains "container helper args" "$args" "IdentityFile=none"
assert_contains "container helper args" "$args" $'-i\n/etc/wrix/keys/'
assert_not_contains "container helper args" "$args" ".ssh/deploy_keys"
assert_not_contains "container helper args" "$args" "/workspace/agent.sock"
assert_not_contains "container helper args" "$args" "StrictHostKeyChecking=no"

git commit --allow-empty -qm "container signed parity"
git verify-commit HEAD >/dev/null
PROBE
  chmod +x "$path"
}

LAUNCHER=$(wrix_build_live_launcher)
IMAGE_SOURCE=$(wrix_realize_test_image_source claude)
IMAGE_REF=$(wrix_live_image_ref "git-ssh-bootstrap-$$")
wrix_remove_image_ref "$IMAGE_REF"
PROFILE_CONFIG="$TEST_TMP/profile.json"
SPAWN_CONFIG="$TEST_TMP/spawn.json"
WORKSPACE="$TEST_TMP/workspace"
HOME_DIR="$TEST_TMP/home"
XDG_CACHE_HOME="$TEST_TMP/cache"
HOST_KEY_DIR="$TEST_TMP/keys"
HOST_DEPLOY_KEY="$HOST_KEY_DIR/loom-nix"
HOST_SIGNING_KEY="$HOST_KEY_DIR/loom-nix-signing"
mkdir -p "$WORKSPACE" "$HOME_DIR" "$XDG_CACHE_HOME" "$HOST_KEY_DIR"
wrix_make_ed25519_key "$HOST_DEPLOY_KEY" "git-ssh-bootstrap-deploy"
wrix_make_ed25519_key "$HOST_SIGNING_KEY" "git-ssh-bootstrap-signing"
write_probe "$WORKSPACE/bootstrap-probe.sh"
wrix_write_profile_config "$PROFILE_CONFIG" "$IMAGE_REF" "$IMAGE_SOURCE" claude
wrix_write_spawn_config "$SPAWN_CONFIG" "$WORKSPACE" bash /workspace/bootstrap-probe.sh

test_fresh_container_git_ssh_bootstrap() {
  local out="$TEST_TMP/bootstrap.out"
  local err="$TEST_TMP/bootstrap.err"
  local rc=0

  HOME="$HOME_DIR" XDG_CACHE_HOME="$XDG_CACHE_HOME" \
    GIT_AUTHOR_NAME="Smoke Author" GIT_AUTHOR_EMAIL="smoke@example.test" \
    WRIX_DEPLOY_KEY="$HOST_DEPLOY_KEY" WRIX_SIGNING_KEY="$HOST_SIGNING_KEY" WRIX_GIT_SIGN=1 \
    wrix_run_spawn "$LAUNCHER" "$PROFILE_CONFIG" "$SPAWN_CONFIG" >"$out" 2>"$err" || rc=$?

  if [[ "$rc" -ne 0 ]]; then
    fail "fresh container did not bootstrap git identity, SSH known_hosts, and signing"
    sed 's/^/    /' "$out" >&2
    sed 's/^/    /' "$err" >&2
    return
  fi

  pass "fresh container bootstraps git identity, pinned SSH, and SSH signing"
}

test_host_container_and_loom_helper() {
  local wrix_bin bin_dir workspace repo home env_deploy env_signing home_deploy home_signing ambient_home fake_bin output common_dir state_dir helper known_hosts env_signing_public home_signing_public allowed_signers allowed_content integration command linked_command origin capture args missing_output missing_capture host_deploy_public env_deploy_public parity_spawn out err rc

  wrix_bin="$LAUNCHER/bin/wrix"
  bin_dir="$(dirname "$wrix_bin")"
  workspace="$TEST_TMP/parity-workspace"
  repo="$workspace"
  home="$TEST_TMP/parity-home"
  env_deploy="$TEST_TMP/parity-env-deploy"
  env_signing="$TEST_TMP/parity-env-signing"
  home_deploy="$home/.ssh/deploy_keys/parity-key"
  home_signing="$home/.ssh/deploy_keys/parity-key-signing"
  ambient_home="$TEST_TMP/parity-ambient-home"
  fake_bin="$workspace/fake-bin"
  parity_spawn="$TEST_TMP/parity-spawn.json"
  out="$TEST_TMP/parity-container.out"
  err="$TEST_TMP/parity-container.err"

  setup_parity_repo "$repo" || { fail "failed to create parity repository"; return; }
  write_host_key "$env_deploy" "parity-env-deploy" || { fail "failed to create env deploy key"; return; }
  write_host_key "$env_signing" "parity-env-signing" || { fail "failed to create env signing key"; return; }
  write_host_key "$home_deploy" "parity-home-deploy" || { fail "failed to create home deploy key"; return; }
  write_host_key "$home_signing" "parity-home-signing" || { fail "failed to create home signing key"; return; }
  write_ambient_ssh_home "$ambient_home" || { fail "failed to create ambient SSH home"; return; }
  write_fake_ssh "$fake_bin" || { fail "failed to create fake ssh"; return; }

  if ! output="$(cd "$repo" && PATH="$bin_dir:$PATH" HOME="$home" WRIX_DEPLOY_KEY="$env_deploy" WRIX_SIGNING_KEY="$env_signing" "$wrix_bin" init --offline --key parity-key 2>&1)"; then
    fail "wrix init with env keys failed: $output"
    return
  fi
  assert_contains "env init output" "$output" "wrix init: repository policy resolved" || return 0

  common_dir="$(git_common_dir "$repo")"
  state_dir="$common_dir/wrix"
  helper="$state_dir/git-ssh"
  known_hosts="$state_dir/github_known_hosts"
  allowed_signers="$state_dir/allowed_signers"
  [[ -x "$helper" ]] || { fail "missing executable host transport helper at $helper"; return; }
  [[ -f "$known_hosts" ]] || { fail "missing host pinned known-hosts at $known_hosts"; return; }
  [[ -f "$allowed_signers" ]] || { fail "missing allowed signers at $allowed_signers"; return; }

  env_signing_public="$(ssh-keygen -y -f "$env_signing")"
  home_signing_public="$(ssh-keygen -y -f "$home_signing")"
  allowed_content="$(<"$allowed_signers")"
  assert_contains "env signing allowed_signers" "$allowed_content" "$env_signing_public" || return 0
  assert_not_contains "env signing allowed_signers" "$allowed_content" "$home_signing_public" || return 0

  mkdir -p "$repo/.loom"
  integration="$repo/.loom/integration"
  git -C "$repo" worktree add -q "$integration" -b loom-integration || { fail "failed to create .loom/integration worktree"; return; }
  command="$(git -C "$repo" config --get core.sshCommand)"
  linked_command="$(git -C "$integration" config --get core.sshCommand)"
  assert_equals "loom integration inherited core.sshCommand" "$linked_command" "$command" || return 0
  origin="$(git -C "$integration" config --show-origin --get core.sshCommand)"
  assert_contains "loom integration config origin" "$origin" "file:$common_dir/config" || return 0
  assert_equals "loom integration signing program" "$(git -C "$integration" config --get gpg.ssh.program)" "wrix-git-sign" || return 0

  capture="$TEST_TMP/parity-ssh-env.args"
  if ! PATH="$fake_bin:$PATH" WRIX_TEST_CAPTURE="$capture" WRIX_DEPLOY_KEY="$env_deploy" HOME="$home" SSH_AUTH_SOCK="$TEST_TMP/agent.sock" \
    "$helper" git@github.com "git-upload-pack 'example/parity.git'"; then
    fail "transport helper failed with WRIX_DEPLOY_KEY"
    return
  fi
  args="$(<"$capture")"
  assert_strict_helper_args "env deploy key" "$args" "$env_deploy" "$known_hosts" "$home_deploy" || return 0

  capture="$TEST_TMP/parity-ssh-home.args"
  if ! env -u WRIX_DEPLOY_KEY \
    PATH="$fake_bin:$PATH" \
    WRIX_TEST_CAPTURE="$capture" \
    HOME="$home" \
    SSH_AUTH_SOCK="$TEST_TMP/agent.sock" \
    "$helper" git@github.com "git-upload-pack 'example/parity.git'"; then
    fail "transport helper failed with HOME fallback deploy key"
    return
  fi
  args="$(<"$capture")"
  assert_strict_helper_args "home deploy key" "$args" "$home_deploy" "$known_hosts" "$env_deploy" || return 0

  missing_output="$TEST_TMP/parity-missing-deploy.out"
  missing_capture="$TEST_TMP/parity-missing-deploy.args"
  if env -u WRIX_DEPLOY_KEY \
    PATH="$fake_bin:$PATH" \
    WRIX_TEST_CAPTURE="$missing_capture" \
    HOME="$ambient_home" \
    SSH_AUTH_SOCK="$TEST_TMP/agent.sock" \
    "$helper" git@github.com "git-upload-pack 'example/parity.git'" \
    >"$missing_output" 2>&1; then
    fail "transport helper succeeded with only ambient SSH identities"
    return
  fi
  output="$(<"$missing_output")"
  assert_contains "missing deploy output" "$output" "no deploy key resolved" || return 0
  if [[ -e "$missing_capture" ]]; then
    fail "transport helper invoked ssh after deploy-key resolution failed"
    return
  fi

  probe_host_github_auth "$helper" "$env_deploy" "$home" || return 0

  PATH="$bin_dir:$PATH" HOME="$home" WRIX_SIGNING_KEY="$env_signing" \
    git -C "$integration" commit --allow-empty -qm "env signed parity" || { fail "integration signed commit with env key failed"; return; }
  PATH="$bin_dir:$PATH" HOME="$home" WRIX_SIGNING_KEY="$env_signing" \
    git -C "$integration" verify-commit HEAD >/dev/null || { fail "integration env-signed commit did not verify"; return; }

  if ! output="$(cd "$repo" && env -u WRIX_DEPLOY_KEY -u WRIX_SIGNING_KEY PATH="$bin_dir:$PATH" HOME="$home" "$wrix_bin" init --offline --key parity-key 2>&1)"; then
    fail "wrix init with HOME fallback keys failed: $output"
    return
  fi
  assert_contains "home init output" "$output" "wrix init: repository policy resolved" || return 0
  allowed_content="$(<"$allowed_signers")"
  assert_contains "home signing allowed_signers" "$allowed_content" "$home_signing_public" || return 0
  assert_not_contains "home signing allowed_signers" "$allowed_content" "$env_signing_public" || return 0

  env -u WRIX_SIGNING_KEY \
    PATH="$bin_dir:$PATH" \
    HOME="$home" \
    git -C "$repo" commit --allow-empty -qm "home signed parity" || { fail "host signed commit with HOME fallback key failed"; return; }
  env -u WRIX_SIGNING_KEY \
    PATH="$bin_dir:$PATH" \
    HOME="$home" \
    git -C "$repo" verify-commit HEAD >/dev/null || { fail "host home-signed commit did not verify"; return; }

  missing_output="$TEST_TMP/parity-missing-signing.out"
  if env -u WRIX_SIGNING_KEY \
    PATH="$bin_dir:$PATH" \
    HOME="$ambient_home" \
    SSH_AUTH_SOCK="$TEST_TMP/agent.sock" \
    git -C "$repo" commit --allow-empty -qm "missing signing parity" \
    >"$missing_output" 2>&1; then
    fail "git signing succeeded with only ambient SSH identities"
    return
  fi
  output="$(<"$missing_output")"
  assert_contains "missing signing output" "$output" "fallback signing key does not exist" || return 0

  if ! output="$(cd "$repo" && PATH="$bin_dir:$PATH" HOME="$home" WRIX_DEPLOY_KEY="$env_deploy" WRIX_SIGNING_KEY="$env_signing" "$wrix_bin" init --offline --key parity-key 2>&1)"; then
    fail "wrix init restoring env signing key failed: $output"
    return
  fi
  assert_contains "restore env init output" "$output" "wrix init: repository policy resolved" || return 0

  write_parity_container_probe "$repo/container-parity-probe.sh" || { fail "failed to write container parity probe"; return; }
  wrix_write_spawn_config "$parity_spawn" "$workspace" bash /workspace/container-parity-probe.sh
  rc=0
  HOME="$home" XDG_CACHE_HOME="$XDG_CACHE_HOME" \
    GIT_AUTHOR_NAME="Wrix Parity" GIT_AUTHOR_EMAIL="parity@example.invalid" \
    WRIX_DEPLOY_KEY="$env_deploy" WRIX_SIGNING_KEY="$env_signing" WRIX_GIT_SIGN=1 \
    wrix_run_spawn "$LAUNCHER" "$PROFILE_CONFIG" "$parity_spawn" >"$out" 2>"$err" || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    fail "container did not use initialized repo Git helper/signing config"
    sed 's/^/    /' "$out" >&2
    sed 's/^/    /' "$err" >&2
    return
  fi

  env_deploy_public="$(ssh-keygen -y -f "$env_deploy")"
  host_deploy_public="$(ssh-keygen -y -f "$home_deploy")"
  assert_not_contains "deploy public keys differ" "$env_deploy_public" "$host_deploy_public" || return 0

  pass "host, container, and .loom/integration use strict context-resolved Git helpers"
}

ALL_TESTS=(
  test_fresh_container_git_ssh_bootstrap
  test_host_container_and_loom_helper
)

run_requested_tests() {
  local fn
  if [[ "$#" -eq 0 ]]; then
    for fn in "${ALL_TESTS[@]}"; do
      "$fn"
    done
    return
  fi

  fn="$1"
  if ! declare -f "$fn" >/dev/null 2>&1; then
    printf 'Unknown function: %s\n' "$fn" >&2
    exit 1
  fi
  "$fn"
}

run_requested_tests "$@"

echo
echo "Results: $PASSED passed, $FAILED failed"
[[ "$FAILED" -eq 0 ]]
