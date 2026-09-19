#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=tests/lib/live-sandbox.sh
source "$SCRIPT_DIR/../lib/live-sandbox.sh"
wrix_require_live_sandbox
cd "$REPO_ROOT"

TEST_TMP=$(mktemp -d -t wrix-pi-auth-isolation.XXXXXX)
IMAGE_REF=""
NAMES=()
PIDS=()
case "$(uname -s)" in
  Linux) RUNTIME=podman ;;
  Darwin) RUNTIME=container ;;
esac
REAL_RUNTIME=$(command -v "$RUNTIME")
cleanup() {
  local name pid
  for name in "${NAMES[@]}"; do
    "$REAL_RUNTIME" kill "$name" >/dev/null 2>&1 || true # best-effort: container may already have exited under --rm
  done
  for pid in "${PIDS[@]}"; do
    wait "$pid" 2>/dev/null || true # best-effort: SIGKILL and already-waited launchers are expected
  done
  rm -rf "$TEST_TMP"
  wrix_remove_image_ref "$IMAGE_REF"
}
trap cleanup EXIT

wait_for() {
  local path attempt
  for ((attempt = 0; attempt < 1200; attempt++)); do
    for path in "$@"; do
      [[ ! -e "$path" ]] || return 0
    done
    # Bounded readiness polling: VirtioFS notifications across host/guest are unreliable.
    sleep 0.1
  done
  printf 'FAIL: timed out waiting for %s\n' "$*" >&2
  return 1
}

LAUNCHER=$(wrix_build_live_launcher)
IMAGE_SOURCE=$(wrix_realize_test_image_source pi)
IMAGE_REF=$(wrix_live_image_ref "pi-auth-isolation-$$")
PROFILE_CONFIG="$TEST_TMP/profile.json"
HOME_DIR="$TEST_TMP/home"
AUTH_DIR="$TEST_TMP/pi"
AUTH_FILE="$AUTH_DIR/auth.json"
DEPLOY_KEY="$TEST_TMP/deploy-key"
mkdir -p "$HOME_DIR" "$AUTH_DIR" "$TEST_TMP/bin"
printf '{"openai-codex":{"type":"oauth","access":"fixture-expired","refresh":"fixture-refresh","expires":1}}\n' >"$AUTH_FILE"
printf 'sibling-canary\n' >"$AUTH_DIR/sibling-secret"
chmod 600 "$AUTH_FILE" "$AUTH_DIR/sibling-secret"
wrix_make_ed25519_key "$DEPLOY_KEY" "pi-auth-isolation-test"
wrix_write_profile_config "$PROFILE_CONFIG" "$IMAGE_REF" "$IMAGE_SOURCE" pi

# Name actual runtime containers so interrupted-launcher cleanup is deterministic.
printf '#!/usr/bin/env bash\nset -euo pipefail\nif [[ "$1" = run ]]; then\n shift\n exec %q run --name "$WRIX_TEST_CONTAINER_NAME" "$@"\nfi\nexec %q "$@"\n' \
  "$REAL_RUNTIME" "$REAL_RUNTIME" >"$TEST_TMP/bin/$RUNTIME"
chmod +x "$TEST_TMP/bin/$RUNTIME"

start_session() {
  local id="$1" workspace="$TEST_TMP/repo-$1" config="$TEST_TMP/spawn-$1.json"
  local name="wrix-pi-auth-$$-$id" virtiofs=0
  [[ "$RUNTIME" != container ]] || virtiofs=1
  mkdir -p "$workspace"
  cp "$SCRIPT_DIR/pi-auth-storage.mjs" "$workspace/test.mjs"
  # shellcheck disable=SC2016
  wrix_write_spawn_config "$config" "$workspace" bash -lc '
set -euo pipefail
[[ "$WRIX_PI_AUTH_JSON" = /mnt/wrix/pi-agent-auth/auth.json ]]
[[ -L "$HOME/.pi/agent/auth.json" ]]
[[ ! -e /mnt/wrix/pi-agent-auth/sibling-secret ]]
[[ ! -e /mnt/wrix/pi-agent-auth/settings.json ]]
[[ -z "$(find /mnt/wrix/pi-agent-auth -mindepth 1 -maxdepth 1 ! -name auth.json ! -name auth.json.lock -print -quit)" ]]
pi_bin=$(readlink -f "$(command -v pi)")
export PI_AUTH_TEST_PACKAGE="$(dirname "$(dirname "$pi_bin")")/lib/node_modules/pi-monorepo"
IFS= read -r node_shebang < "$PI_AUTH_TEST_PACKAGE/dist/cli.js"
node_bin="${node_shebang#\#!}"
exec "$node_bin" /workspace/test.mjs worker /workspace "$WRIX_TEST_ID" refresh
'
  jq --arg id "$id" --arg virtiofs "$virtiofs" \
    '.env += [["WRIX_TEST_ID", $id], ["PI_AUTH_TEST_VIRTIOFS", $virtiofs]]' "$config" >"$config.tmp"
  mv "$config.tmp" "$config"
  NAMES+=("$name")
  env PATH="$TEST_TMP/bin:$PATH" HOME="$HOME_DIR" WRIX_DEPLOY_KEY="$DEPLOY_KEY" WRIX_GIT_SIGN=0 \
    WRIX_PI_AUTH_FILE="$AUTH_FILE" WRIX_TEST_CONTAINER_NAME="$name" \
    "$LAUNCHER/bin/wrix" --profile-config "$PROFILE_CONFIG" spawn --spawn-config "$config" \
    >"$TEST_TMP/$id.out" 2>"$TEST_TMP/$id.err" &
  PIDS+=("$!")
  wait_for "$workspace/ready-$id"
}

start_session first
start_session second
touch "$TEST_TMP/repo-first/go" "$TEST_TMP/repo-second/go"
wait_for "$TEST_TMP/repo-first/attempting-first"
wait_for "$TEST_TMP/repo-second/attempting-second"
wait_for "$TEST_TMP/repo-first/refresh-started" "$TEST_TMP/repo-second/refresh-started"
touch "$TEST_TMP/repo-first/finish-refresh" "$TEST_TMP/repo-second/finish-refresh"
wait_for "$TEST_TMP/repo-first/done-first"
wait_for "$TEST_TMP/repo-second/done-second"

# The host sees completed writes before either container or launcher exits.
[[ -L "$AUTH_FILE" ]]
case "$(uname -s)" in
  Linux)
    [[ "$(stat -c %a "$AUTH_FILE.wrix-auth")" = 700 ]]
    [[ "$(stat -Lc %a "$AUTH_FILE")" = 600 ]]
    ;;
  Darwin)
    [[ "$(stat -f %Lp "$AUTH_FILE.wrix-auth")" = 700 ]]
    [[ "$(stat -Lf %Lp "$AUTH_FILE")" = 600 ]]
    ;;
esac
jq -e '."openai-codex".refresh == "fixture-rotated" and has("fixture-first") and has("fixture-second")' "$AUTH_FILE" >/dev/null
[[ "$(find "$TEST_TMP" -name refreshes -exec cat {} + | wc -l)" -eq 1 ]]
kill -KILL "${PIDS[0]}"
wait "${PIDS[0]}" 2>/dev/null || true # best-effort: the launcher was deliberately killed
"$REAL_RUNTIME" kill "${NAMES[0]}" >/dev/null
touch "$TEST_TMP/repo-second/exit-second"
wait "${PIDS[1]}"

start_session restart
touch "$TEST_TMP/repo-restart/go"
wait_for "$TEST_TMP/repo-restart/done-restart"
touch "$TEST_TMP/repo-restart/exit-restart"
wait "${PIDS[2]}"
[[ "$(find "$TEST_TMP" -name refreshes -exec cat {} + | wc -l)" -eq 1 ]]
[[ "$(<"$AUTH_DIR/sibling-secret")" = sibling-canary ]]
jq -e '."openai-codex".refresh == "fixture-rotated"' "$AUTH_FILE" >/dev/null
printf 'PASS: %s persistent Pi auth, overlapping refresh, killed launcher, restart, and isolation\n' "$(uname -s)"
