#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=tests/lib/live-sandbox.sh
source "$SCRIPT_DIR/../lib/live-sandbox.sh"

wrix_require_live_sandbox
command -v script >/dev/null 2>&1 || wrix_live_skip "script not on PATH"
cd "$REPO_ROOT"

TEST_TMP=$(mktemp -d -t wrix-git-grants.XXXXXX)
cleanup() {
  rm -rf "$TEST_TMP"
  wrix_remove_image_ref "${IMAGE_REF:-}"
}
trap cleanup EXIT

LAUNCHER=$(wrix_build_live_launcher)
IMAGE_SOURCE=$(nix build --no-link --print-out-paths --no-warn-dirty .#test-image-git-credentials.source)
IMAGE_REF=$(wrix_live_image_ref "git-grants-$$")
PROFILE_CONFIG="$TEST_TMP/profile.json"
HOME_DIR="$TEST_TMP/home"
CACHE_DIR="$TEST_TMP/cache"
KEY_DIR="$HOME_DIR/.ssh/deploy_keys"
DEPLOY_KEY="$TEST_TMP/explicit-deploy"
SIGNING_KEY="$TEST_TMP/explicit-signing"
mkdir -p "$KEY_DIR" "$CACHE_DIR"
chmod 700 "$HOME_DIR/.ssh" "$KEY_DIR"
wrix_make_ed25519_key "$DEPLOY_KEY" "grant-deploy"
wrix_make_ed25519_key "$SIGNING_KEY" "grant-signing"
cp "$DEPLOY_KEY" "$KEY_DIR/grant-key"
cp "$SIGNING_KEY" "$KEY_DIR/grant-key-signing"
wrix_make_ed25519_key "$HOME_DIR/.ssh/id_ed25519" "ambient-identity"
wrix_write_profile_config "$PROFILE_CONFIG" "$IMAGE_REF" "$IMAGE_SOURCE" direct
jq '.security.deploy_key = "grant-key"' "$PROFILE_CONFIG" >"$PROFILE_CONFIG.key"
mv "$PROFILE_CONFIG.key" "$PROFILE_CONFIG"

launch_case() {
  local mode="$1" workspace="$2" deploy="$3" sign="$4" ambient="$5"
  shift 5
  local spawn_config="$TEST_TMP/case.json" command_line key variable granted
  local -a cmd
  cmd=("$LAUNCHER/bin/wrix" --profile-config "$PROFILE_CONFIG")
  if [[ "$mode" == spawn ]]; then
    wrix_write_spawn_config "$spawn_config" "$workspace" "$@"
    jq --argjson deploy "$deploy" --argjson sign "$sign" \
      '.git = {deploy: $deploy, sign: $sign} | .env = [["WRIX_EFFECTIVE_GIT_SIGN", "1"], ["WRIX_GIT_SIGN", "1"], ["WRIX_DEPLOY_KEY", "/not/forwarded"], ["WRIX_SIGNING_KEY", "/not/forwarded"]]' \
      "$spawn_config" >"$spawn_config.grants"
    mv "$spawn_config.grants" "$spawn_config"
    cmd+=(spawn --spawn-config "$spawn_config")
  else
    cmd+=(run)
    if [[ "$deploy" == true ]]; then cmd+=(--git-deploy); else cmd+=(--no-git-deploy); fi
    if [[ "$sign" == true ]]; then cmd+=(--git-sign); else cmd+=(--no-git-sign); fi
    cmd+=("$workspace" -- "$@")
  fi
  (
    export HOME="$HOME_DIR" XDG_CACHE_HOME="$CACHE_DIR"
    export WRIX_GIT_SIGN=1 WRIX_EFFECTIVE_GIT_SIGN=1
    for key in deploy signing; do
      case "$key" in
        deploy) variable=WRIX_DEPLOY_KEY; granted="$deploy" ;;
        signing) variable=WRIX_SIGNING_KEY; granted="$sign" ;;
      esac
      case "$ambient:$granted" in
        fallback:*) unset "$variable" ;;
        invalid:false | missing:*) export "$variable=$TEST_TMP/missing-$key" ;;
        *) export "$variable=$TEST_TMP/explicit-$key" ;;
      esac
    done
    if [[ "$mode" == spawn ]]; then
      "${cmd[@]}"
    else
      printf -v command_line '%q ' "${cmd[@]}"
      wrix_run_with_pty "$command_line"
    fi
  )
}

write_key_probe() {
  local path="$1"
  cat >"$path" <<'PROBE'
#!/usr/bin/env bash
set -euo pipefail

deploy="$1"
sign="$2"
expected=""
if [[ "$deploy" == true ]]; then
  [[ "${WRIX_DEPLOY_KEY:-}" == /etc/wrix/keys/grant-key ]]
  [[ -f "$WRIX_DEPLOY_KEY" ]]
  expected=grant-key
else
  [[ ! -v WRIX_DEPLOY_KEY ]]
fi
if [[ "$sign" == true ]]; then
  [[ "${WRIX_SIGNING_KEY:-}" == /etc/wrix/keys/grant-key-signing ]]
  [[ -f "$WRIX_SIGNING_KEY" ]]
  expected="${expected:+$expected$'\n'}grant-key-signing"
  [[ "$WRIX_EFFECTIVE_GIT_SIGN" == 1 ]]
else
  [[ ! -v WRIX_SIGNING_KEY ]]
  [[ "$WRIX_EFFECTIVE_GIT_SIGN" == 0 ]]
fi
[[ ! -v WRIX_GIT_SIGN ]]
actual=""
if [[ -d /etc/wrix/keys ]]; then
  actual=$(find /etc/wrix/keys -type f -printf '%f\n' | sort)
fi
[[ "$actual" == "$expected" ]]
[[ ! -e "$HOME/.ssh/id_ed25519" ]]
[[ "$(git config --get commit.gpgsign)" == "$sign" ]]
printf 'keys verified\n' > /workspace/probe-passed
PROBE
}

test_git_grant_isolation() {
  local mode deploy sign ambient workspace output key variable suffix
  for mode in run spawn; do
    for deploy in false true; do
      for sign in false true; do
        for ambient in valid invalid fallback; do
          workspace="$TEST_TMP/isolation-$mode-$deploy-$sign-$ambient"
          mkdir -p "$workspace"
          write_key_probe "$workspace/probe.sh"
          if ! output=$(launch_case "$mode" "$workspace" "$deploy" "$sign" "$ambient" \
            bash /workspace/probe.sh "$deploy" "$sign" 2>&1); then
            printf '%s\n' "$output" >&2
            return 1
          fi
          [[ -f "$workspace/probe-passed" ]] || { printf '%s\n' "$output" >&2; return 1; }
          printf 'PASS: %s deploy=%s sign=%s ambient=%s\n' "$mode" "$deploy" "$sign" "$ambient"
        done
      done
    done
    for key in deploy signing; do
      workspace="$TEST_TMP/missing-$mode-$key"
      mkdir -p "$workspace"
      variable=WRIX_DEPLOY_KEY; suffix=""
      deploy=true; sign=false
      if [[ "$key" == signing ]]; then variable=WRIX_SIGNING_KEY; suffix=-signing; deploy=false; sign=true; fi
      if output=$(launch_case "$mode" "$workspace" "$deploy" "$sign" missing \
        bash -c 'touch /workspace/started' 2>&1); then
        [[ "$output" == *"$variable=$TEST_TMP/missing-$key"* ]] || return 1
      fi
      [[ "$output" == *"$variable=$TEST_TMP/missing-$key"* && ! -e "$workspace/started" ]]
      mv "$KEY_DIR/grant-key$suffix" "$TEST_TMP/saved-key"
      if output=$(launch_case "$mode" "$workspace" "$deploy" "$sign" fallback \
        bash -c 'touch /workspace/started' 2>&1); then
        [[ "$output" == *"granted $key key unresolved"* ]] || return 1
      fi
      [[ "$output" == *"granted $key key unresolved"* && ! -e "$workspace/started" ]]
      mv "$TEST_TMP/saved-key" "$KEY_DIR/grant-key$suffix"
    done
  done
}

write_signing_probe() {
  local path="$1"
  cat >"$path" <<'PROBE'
#!/usr/bin/env bash
set -euo pipefail

sign="$1"
[[ "$(git config --show-scope --get commit.gpgsign)" == "command"$'\t'"$sign" ]]
git commit --allow-empty -qm "session sign=$sign"
commit=$(git cat-file -p HEAD)
if [[ "$sign" == true ]]; then
  [[ "$commit" == *gpgsig* ]]
  git verify-commit HEAD
else
  [[ "$commit" != *gpgsig* ]]
fi
printf 'signing verified\n' > /workspace/probe-passed
PROBE
}

test_session_local_signing() {
  local mode host_sign sign workspace output
  for mode in run spawn; do
    for host_sign in true false; do
      workspace="$TEST_TMP/signing-$mode-$host_sign"
      mkdir -p "$workspace"
      git -C "$workspace" init -q
      git -C "$workspace" remote add origin git@github.com:example/grants.git
      (cd "$workspace" && HOME="$HOME_DIR" WRIX_DEPLOY_KEY="$DEPLOY_KEY" WRIX_SIGNING_KEY="$SIGNING_KEY" \
        PATH="$LAUNCHER/bin:$PATH" "$LAUNCHER/bin/wrix" init --offline --sign --key grant-key) \
        >"$TEST_TMP/init.out" 2>&1
      git -C "$workspace" config commit.gpgsign "$host_sign"
      cp "$workspace/.git/config" "$workspace/common.before"
      write_signing_probe "$workspace/probe.sh"
      for sign in true false; do
        rm -f "$workspace/probe-passed"
        if ! output=$(launch_case "$mode" "$workspace" false "$sign" valid \
          bash /workspace/probe.sh "$sign" 2>&1); then
          printf '%s\n' "$output" >&2
          return 1
        fi
        [[ -f "$workspace/probe-passed" ]] || { printf '%s\n' "$output" >&2; return 1; }
        cmp "$workspace/common.before" "$workspace/.git/config"
        [[ "$(git -C "$workspace" config --local --get commit.gpgsign)" == "$host_sign" ]]
        printf 'PASS: %s host signing=%s session signing=%s common config unchanged\n' "$mode" "$host_sign" "$sign"
      done
    done
  done
}

case "${1:-all}" in
  test_git_grant_isolation) test_git_grant_isolation ;;
  test_session_local_signing) test_session_local_signing ;;
  all) test_git_grant_isolation; test_session_local_signing ;;
  *) printf 'Unknown function: %s\n' "$1" >&2; exit 1 ;;
esac
