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

LAUNCHER=$(wrix_build_live_launcher)
IMAGE_SOURCE=$(wrix_realize_test_image_source claude)
IMAGE_REF=$(wrix_live_image_ref "git-ssh-bootstrap-$$")
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

test_fresh_container_git_ssh_bootstrap

echo
echo "Results: $PASSED passed, $FAILED failed"
[[ "$FAILED" -eq 0 ]]
