#!/usr/bin/env bash
# Verifier for criterion 194 of specs/security.md:
#
#   When the launcher's environment sets WRIX_DEPLOY_KEY and
#   WRIX_SIGNING_KEY to existing files outside $HOME/.ssh/deploy_keys/,
#   the child container observes both env vars set to
#   /etc/wrix/keys/<name>{,-signing}, the files are present at those
#   in-container paths, and `git commit` in the child produces a commit
#   whose `git cat-file -p HEAD` output contains a non-empty `gpgsig`
#   field.
#
# The launcher's resolution block is extracted from the writeShellApplication
# `text = ''…''` body in lib/sandbox/{linux,darwin}/default.nix, the
# `''$` Nix escape is converted back to `$`, and the Nix interpolations
# `${deployKeyExpr}` / `${sshConfig.containerKeyDir}` are substituted with
# concrete values for the test. We run the resulting shell snippet under
# bash with WRIX_DEPLOY_KEY / WRIX_SIGNING_KEY pointing at fixture
# keys generated outside $HOME/.ssh/deploy_keys/ and assert:
#
#   - the host source path mounts to /etc/wrix/keys/<name>{,-signing}
#   - the child-env env vars are set to the in-container paths
#   - the host source path does NOT cross as the child's env value
#
# Final tests source lib/util/git-ssh-setup.sh with the in-container
# WRIX_DEPLOY_KEY / WRIX_SIGNING_KEY pointing at the fixture keys
# (the host-side stand-in for /etc/wrix/keys/) and assert that git SSH
# auth uses the deploy key while commit signing uses the signing key.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
LINUX_LAUNCHER_NIX="$REPO_ROOT/lib/sandbox/linux/default.nix"
DARWIN_LAUNCHER_NIX="$REPO_ROOT/lib/sandbox/darwin/default.nix"
GIT_SSH_SETUP="$REPO_ROOT/lib/util/git-ssh-setup.sh"
BASH_BIN="${BASH:-$(command -v bash)}"

skip() {
  echo "SKIP: $1" >&2
  exit 77
}

for tool in ssh-keygen git awk sed getent; do
  command -v "$tool" >/dev/null 2>&1 || skip "$tool not on PATH"
done

TEST_TMP=$(mktemp -d -t wrix-nested-key.XXXXXX)
trap 'rm -rf "$TEST_TMP"' EXIT

PASSED=0
FAILED=0
pass() {
  printf '  PASS: %s\n' "$1"
  PASSED=$((PASSED + 1))
}
fail() {
  printf '  FAIL: %s\n' "$1" >&2
  FAILED=$((FAILED + 1))
}

# Extract the deploy/signing-key resolution block from a writeShellApplication
# `text = ''…''` body. Capture from the marker comment to the sentinel
# `${stageBeads}` line (exclusive), convert Nix's `''$` escape back to `$`,
# and substitute the two Nix interpolations with concrete values so the
# snippet runs under bash.
extract_keyresolve_block() {
  local source="$1" out="$2"
  # SC2016: single quotes are intentional — the `${…}` patterns are
  # Nix interpolation tokens we want sed to match literally.
  # shellcheck disable=SC2016
  awk '
    /deploy key and signing key/ { capture = 1 }
    /\$\{stageBeads\}/           { capture = 0 }
    capture                       { print }
  ' "$source" \
    | sed -e 's|\${deployKeyExpr}|"myrepo"|g' \
          -e 's|\${sshConfig\.containerKeyDir}|/etc/wrix/keys|g' \
          -e "s/''\\\$/\$/g" \
    > "$out"
}

HOST_KEY_DIR="$TEST_TMP/keys"
mkdir -p "$HOST_KEY_DIR"
HOST_DEPLOY_KEY="$HOST_KEY_DIR/myrepo"
HOST_SIGNING_KEY="$HOST_KEY_DIR/myrepo-signing"
ssh-keygen -t ed25519 -N "" -q -f "$HOST_DEPLOY_KEY" -C nested-key-test >/dev/null
ssh-keygen -t ed25519 -N "" -q -f "$HOST_SIGNING_KEY" -C nested-key-signing-test >/dev/null

EMPTY_HOME="$TEST_TMP/empty-home"
mkdir -p "$EMPTY_HOME/.ssh/deploy_keys"

# ----------------------------------------------------------------------------
# Test 1: Linux launcher routes env-first source to /etc/wrix/keys/<name>
# ----------------------------------------------------------------------------
test_linux_env_first_resolution() {
  local block="$TEST_TMP/linux-keys.sh"
  extract_keyresolve_block "$LINUX_LAUNCHER_NIX" "$block"

  local probe="$TEST_TMP/linux-probe.sh"
  cat >"$probe" <<EOF
set -e
HOME=$EMPTY_HOME
VOLUME_ARGS=""
DEPLOY_KEY_ARGS=""
WRIX_DEPLOY_KEY=$HOST_DEPLOY_KEY
WRIX_SIGNING_KEY=$HOST_SIGNING_KEY
source $block
printf 'VOLUME_ARGS=%s\n' "\$VOLUME_ARGS"
printf 'DEPLOY_KEY_ARGS=%s\n' "\$DEPLOY_KEY_ARGS"
EOF

  local out
  if ! out=$(bash "$probe" 2>&1); then
    fail "Linux probe exited non-zero: $out"
    return
  fi

  grep -qF -- "-v $HOST_DEPLOY_KEY:/etc/wrix/keys/myrepo:ro" <<<"$out" \
    || { fail "Linux: deploy-key host→container mount missing in VOLUME_ARGS"; return; }
  grep -qF -- "-v $HOST_SIGNING_KEY:/etc/wrix/keys/myrepo-signing:ro" <<<"$out" \
    || { fail "Linux: signing-key host→container mount missing in VOLUME_ARGS"; return; }
  grep -qF -- "-e WRIX_DEPLOY_KEY=/etc/wrix/keys/myrepo" <<<"$out" \
    || { fail "Linux: child WRIX_DEPLOY_KEY not set to in-container path"; return; }
  grep -qF -- "-e WRIX_SIGNING_KEY=/etc/wrix/keys/myrepo-signing" <<<"$out" \
    || { fail "Linux: child WRIX_SIGNING_KEY not set to in-container path"; return; }
  if grep -qF -- "WRIX_DEPLOY_KEY=$HOST_DEPLOY_KEY" <<<"$out"; then
    fail "Linux: host source path leaked into child env"
    return
  fi
  pass "Linux launcher env-first source routes to /etc/wrix/keys/<name>{,-signing}"
}

# ----------------------------------------------------------------------------
# Test 2: Darwin launcher routes env-first source to /etc/wrix/keys/<name>
# (mount surface is staging+FILE_MOUNTS rather than direct -v, but
# in-container destination + child env stay identical to Linux.)
# ----------------------------------------------------------------------------
test_darwin_env_first_resolution() {
  local block="$TEST_TMP/darwin-keys.sh"
  extract_keyresolve_block "$DARWIN_LAUNCHER_NIX" "$block"

  local staging_root="$TEST_TMP/darwin-staging"
  mkdir -p "$staging_root"

  local probe="$TEST_TMP/darwin-probe.sh"
  cat >"$probe" <<EOF
set -e
HOME=$EMPTY_HOME
STAGING_ROOT=$staging_root
MOUNT_ARGS=""
FILE_MOUNTS=""
DEPLOY_KEY_ARGS=""
WRIX_DEPLOY_KEY=$HOST_DEPLOY_KEY
WRIX_SIGNING_KEY=$HOST_SIGNING_KEY
source $block
printf 'MOUNT_ARGS=%s\n' "\$MOUNT_ARGS"
printf 'FILE_MOUNTS=%s\n' "\$FILE_MOUNTS"
printf 'DEPLOY_KEY_ARGS=%s\n' "\$DEPLOY_KEY_ARGS"
EOF

  local out
  if ! out=$(bash "$probe" 2>&1); then
    fail "Darwin probe exited non-zero: $out"
    return
  fi

  grep -qF "/mnt/wrix/deploy_keys/myrepo:/etc/wrix/keys/myrepo" <<<"$out" \
    || { fail "Darwin: deploy-key in-container destination missing in FILE_MOUNTS"; return; }
  grep -qF "/mnt/wrix/deploy_keys/myrepo-signing:/etc/wrix/keys/myrepo-signing" <<<"$out" \
    || { fail "Darwin: signing-key in-container destination missing in FILE_MOUNTS"; return; }
  grep -qF -- "-e WRIX_DEPLOY_KEY=/etc/wrix/keys/myrepo" <<<"$out" \
    || { fail "Darwin: child WRIX_DEPLOY_KEY not set to in-container path"; return; }
  grep -qF -- "-e WRIX_SIGNING_KEY=/etc/wrix/keys/myrepo-signing" <<<"$out" \
    || { fail "Darwin: child WRIX_SIGNING_KEY not set to in-container path"; return; }
  if grep -qF -- "WRIX_DEPLOY_KEY=$HOST_DEPLOY_KEY" <<<"$out"; then
    fail "Darwin: host source path leaked into child env"
    return
  fi
  # The fixture keys must have been staged under STAGING_ROOT (cp-from-host
  # is the macOS analogue of Linux's bind mount; verifies the host source
  # was actually read).
  test -f "$staging_root/deploy_keys/myrepo" \
    || { fail "Darwin: fixture deploy key not staged under STAGING_ROOT"; return; }
  test -f "$staging_root/deploy_keys/myrepo-signing" \
    || { fail "Darwin: fixture signing key not staged under STAGING_ROOT"; return; }
  pass "Darwin launcher env-first source routes to /etc/wrix/keys/<name>{,-signing}"
}

# ----------------------------------------------------------------------------
# Test 3: git commit signed with WRIX_SIGNING_KEY produces non-empty gpgsig
#
# git-ssh-setup.sh is what the entrypoint sources inside the sandbox. Given
# the launcher routes WRIX_SIGNING_KEY to the in-container key path, the
# entrypoint should be able to produce a signed commit. We exercise the same
# code path on the host by sourcing git-ssh-setup.sh with the fixture keys
# (the host-side stand-in for /etc/wrix/keys/).
# ----------------------------------------------------------------------------
test_git_signing_produces_gpgsig() {
  local repo="$TEST_TMP/repo"
  local signing_home="$TEST_TMP/signing-home"
  mkdir -p "$signing_home"
  git init -q "$repo"

  local err_log="$TEST_TMP/git-sign.log"
  # shellcheck disable=SC2030,SC2031
  if ! (
    set -e
    cd "$repo"
    # Isolate git config to $signing_home: HOME alone is not enough — git
    # also reads XDG_CONFIG_HOME/git/config and the GIT_CONFIG_GLOBAL
    # override. If a caller's XDG path is read-only (e.g. some sandbox
    # mounts), `git commit` errors at config-lock time before signing
    # gets exercised at all.
    unset XDG_CONFIG_HOME GIT_CONFIG_GLOBAL GIT_CONFIG_SYSTEM
    export HOME="$signing_home"
    export WRIX_DEPLOY_KEY="$HOST_DEPLOY_KEY"
    export WRIX_SIGNING_KEY="$HOST_SIGNING_KEY"
    export WRIX_GIT_SIGN=1
    export GIT_AUTHOR_NAME=test
    export GIT_AUTHOR_EMAIL=test@example.com
    export GIT_COMMITTER_NAME=test
    export GIT_COMMITTER_EMAIL=test@example.com
    # shellcheck source=/dev/null
    source "$GIT_SSH_SETUP"
    git commit --allow-empty -q -m "test-signed"
    git cat-file -p HEAD | grep -q '^gpgsig'
  ) >"$err_log" 2>&1; then
    fail "git commit did not produce a non-empty gpgsig"
    sed 's/^/    /' "$err_log" >&2
    git -C "$repo" cat-file -p HEAD 2>&1 | sed 's/^/    /' >&2 || true
    return
  fi
  pass "git commit produces non-empty gpgsig with WRIX_SIGNING_KEY routed via env-first"
}

# ----------------------------------------------------------------------------
# Test 4: bare SSH config is written for $HOME and the effective user's home
# ----------------------------------------------------------------------------
test_ssh_config_written_for_effective_user_home() {
  local home_dir="$TEST_TMP/home-config"
  local effective_home="$TEST_TMP/effective-home"
  local fake_bin="$TEST_TMP/fake-user-bin"
  local err_log="$TEST_TMP/ssh-config-home.log"
  mkdir -p "$home_dir" "$effective_home" "$fake_bin"

  cat >"$fake_bin/id" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" = "-u" ]]; then
  printf '4242\n'
else
  echo "unexpected fake id invocation: $*" >&2
  exit 64
fi
EOF
  chmod +x "$fake_bin/id"

  cat >"$fake_bin/getent" <<EOF
#!$BASH_BIN
set -euo pipefail
if [[ "\${1:-}" = "passwd" && "\${2:-}" = "4242" ]]; then
  printf 'wrix:x:4242:4242::%s:/bin/sh\n' "$effective_home"
else
  echo "unexpected fake getent invocation: \$*" >&2
  exit 64
fi
EOF
  chmod +x "$fake_bin/getent"

  # shellcheck disable=SC2030,SC2031
  if ! (
    set -e
    unset XDG_CONFIG_HOME GIT_CONFIG_GLOBAL GIT_CONFIG_SYSTEM WRIX_SIGNING_KEY
    export HOME="$home_dir"
    export PATH="$fake_bin:$PATH"
    export WRIX_DEPLOY_KEY="$HOST_DEPLOY_KEY"
    # shellcheck source=/dev/null
    source "$GIT_SSH_SETUP"
    grep -qF "IdentityFile $HOST_DEPLOY_KEY" "$home_dir/.ssh/config"
    grep -qF "IdentityFile $HOST_DEPLOY_KEY" "$effective_home/.ssh/config"
  ) >"$err_log" 2>&1; then
    fail "SSH config was not written for both HOME and effective user home"
    sed 's/^/    /' "$err_log" >&2
    return
  fi

  pass "bare SSH config is available to OpenSSH's effective-user home lookup"
}

# ----------------------------------------------------------------------------
# Test 5: normal git-over-SSH uses WRIX_DEPLOY_KEY even without env inheritance
# ----------------------------------------------------------------------------
test_git_ssh_command_uses_deploy_key_from_git_config() {
  local repo="$TEST_TMP/ssh-repo"
  local ssh_home="$TEST_TMP/ssh-home"
  local fake_bin="$TEST_TMP/fake-ssh-bin"
  local ssh_argv_log="$TEST_TMP/fake-ssh.argv"
  local err_log="$TEST_TMP/git-ssh.log"
  mkdir -p "$ssh_home" "$fake_bin"
  git init -q "$repo"
  git -C "$repo" remote add origin git@github.com:owner/repo.git

  cat >"$fake_bin/ssh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
: "${WRIX_FAKE_SSH_ARGV_LOG:?}"
printf '%s\n' "$@" > "$WRIX_FAKE_SSH_ARGV_LOG"
printf '0000'
EOF
  chmod +x "$fake_bin/ssh"

  # shellcheck disable=SC2030,SC2031
  if ! (
    set -e
    cd "$repo"
    unset XDG_CONFIG_HOME GIT_CONFIG_GLOBAL GIT_CONFIG_SYSTEM WRIX_SIGNING_KEY
    export HOME="$ssh_home"
    export WRIX_DEPLOY_KEY="$HOST_DEPLOY_KEY"
    # shellcheck source=/dev/null
    source "$GIT_SSH_SETUP"

    expected="ssh -i $HOST_DEPLOY_KEY -o IdentitiesOnly=yes"
    [[ "${GIT_SSH_COMMAND:-}" = "$expected" ]]
    [[ "$(git config --global --get core.sshCommand)" = "$expected" ]]

    export PATH="$fake_bin:$PATH"
    export WRIX_FAKE_SSH_ARGV_LOG="$ssh_argv_log"
    unset GIT_SSH_COMMAND
    git ls-remote origin >/dev/null
  ) >"$err_log" 2>&1; then
    fail "git ls-remote did not use deploy-key SSH setup"
    sed 's/^/    /' "$err_log" >&2
    return
  fi

  if [[ ! -f "$ssh_argv_log" ]]; then
    fail "fake ssh was not invoked by git ls-remote"
    return
  fi

  local -a ssh_argv
  mapfile -t ssh_argv < "$ssh_argv_log"
  local saw_identity=0
  local saw_identities_only=0
  local i
  for ((i = 0; i < ${#ssh_argv[@]}; i++)); do
    if [[ "${ssh_argv[$i]}" = "-i" && "${ssh_argv[$((i + 1))]:-}" = "$HOST_DEPLOY_KEY" ]]; then
      saw_identity=1
    fi
    if [[ "${ssh_argv[$i]}" = "-o" && "${ssh_argv[$((i + 1))]:-}" = "IdentitiesOnly=yes" ]]; then
      saw_identities_only=1
    fi
  done

  if [[ "$saw_identity" -ne 1 ]]; then
    fail "git ssh command did not pass deploy key with -i"
    printf '    %s\n' "${ssh_argv[@]}" >&2
    return
  fi
  if [[ "$saw_identities_only" -ne 1 ]]; then
    fail "git ssh command did not pass IdentitiesOnly=yes"
    printf '    %s\n' "${ssh_argv[@]}" >&2
    return
  fi

  pass "git-over-SSH uses WRIX_DEPLOY_KEY with IdentitiesOnly=yes"
}

ALL_TESTS=(
  test_linux_env_first_resolution
  test_darwin_env_first_resolution
  test_git_signing_produces_gpgsig
  test_ssh_config_written_for_effective_user_home
  test_git_ssh_command_uses_deploy_key_from_git_config
)

for fn in "${ALL_TESTS[@]}"; do
  echo "=== $fn ==="
  "$fn" || true
done

echo
echo "Results: $PASSED passed, $FAILED failed"
[[ "$FAILED" -eq 0 ]]
