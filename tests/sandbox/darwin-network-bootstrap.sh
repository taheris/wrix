#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=tests/lib/live-sandbox.sh
source "$SCRIPT_DIR/../lib/live-sandbox.sh"

TEST_TMP="$(mktemp -d -t wrix-darwin-network-bootstrap.XXXXXX)"

cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  return 1
}

assert_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    fail "$label: missing '$needle' in output: $haystack"
  fi
}

test_darwin_network_bootstrap() {
  wrix_require_live_sandbox_darwin
  cd "$REPO_ROOT"

  local command_line image_ref image_source launcher output poison_error poison_profile
  local profile_config spawn_config stage_line verification_line workspace
  local -a command
  workspace="$TEST_TMP/workspace"
  mkdir -p "$workspace/bin"

  cat >"$workspace/assert-bootstrap.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

[[ -f /run/wrix-network-ready ]] || {
  printf 'network bootstrap marker is missing\n' >&2
  exit 1
}
[[ "$PATH" == /workspace/bin:* ]] || {
  printf 'workspace poison directory is not reachable during stage two\n' >&2
  exit 1
}
[[ "${1:-}" == "alpha" ]] || {
  printf 'first bootstrap argument was not preserved\n' >&2
  exit 1
}
[[ "${2:-}" == "two words" ]] || {
  printf 'second bootstrap argument was not preserved\n' >&2
  exit 1
}
seen=0
while read -r field value _rest; do
  case "$field" in
    CapInh:|CapPrm:|CapEff:|CapBnd:|CapAmb:)
      [[ "$value" =~ ^[0-9A-Fa-f]+$ ]] || exit 1
      low="${value: -8}"
      if (( (16#$low & 16#1000) != 0 )); then
        printf 'NET_ADMIN survived in %s\n' "$field" >&2
        exit 1
      fi
      seen=$((seen + 1))
      ;;
  esac
done < /proc/self/status
[[ "$seen" -eq 5 ]] || {
  printf 'Linux capability state could not be verified\n' >&2
  exit 1
}
[[ ! -e /workspace/poison.log ]] || {
  printf 'bootstrap executed a workspace-controlled tool\n' >&2
  exit 1
}
printf 'DARWIN_BOOTSTRAP_STAGE_TWO=%s|%s\n' "$1" "$2"
EOF
  chmod +x "$workspace/assert-bootstrap.sh"

  local tool
  for tool in nft capsh getent awk sort grep; do
    cat >"$workspace/bin/$tool" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' "$0" >>/workspace/poison.log
exit 97
EOF
    chmod +x "$workspace/bin/$tool"
  done

  launcher=$(wrix_build_live_launcher)
  image_source=$(wrix_realize_test_image_source direct)
  image_ref=$(wrix_unique_image_ref "wrix-test-bootstrap-env")
  profile_config="$TEST_TMP/profile.json"
  poison_profile="$TEST_TMP/poison-profile.json"
  spawn_config="$TEST_TMP/spawn.json"
  poison_error="$TEST_TMP/poison.err"
  wrix_write_profile_config "$profile_config" "$image_ref" "$image_source" direct

  jq '.profile.env.BASH_ENV = "/workspace/profile-poison.sh"' "$profile_config" >"$poison_profile"
  if "$launcher/bin/wrix" --profile-config "$poison_profile" run "$workspace" true 2>"$poison_error"; then
    fail "Darwin launcher accepted BASH_ENV from ProfileConfig"
    return 1
  fi
  assert_contains "ProfileConfig bootstrap env rejection" "$(<"$poison_error")" "bootstrap-sensitive" || return 1

  jq -n --arg workspace "$workspace" \
    '{workspace:$workspace,env:[["LD_PRELOAD","/workspace/spawn-poison.so"]],agent_args:["true"],mounts:[]}' \
    >"$spawn_config"
  if "$launcher/bin/wrix" --profile-config "$profile_config" spawn --spawn-config "$spawn_config" 2>"$poison_error"; then
    fail "Darwin launcher accepted LD_PRELOAD from SpawnConfig"
    return 1
  fi
  assert_contains "SpawnConfig bootstrap env rejection" "$(<"$poison_error")" "bootstrap-sensitive" || return 1

  command=(
    "$launcher/bin/wrix" --profile-config "$profile_config" run "$workspace"
    /workspace/assert-bootstrap.sh alpha 'two words'
  )
  printf -v command_line '%q ' "${command[@]}"
  output=$(WRIX_NETWORK=open wrix_run_with_pty "$command_line")

  assert_contains "firewall verification" "$output" "Network policy verified:" || return 1
  assert_contains "stage two" "$output" "DARWIN_BOOTSTRAP_STAGE_TWO=alpha|two words" || return 1
  [[ ! -e "$workspace/poison.log" ]] || fail "bootstrap executed a workspace-controlled tool"

  verification_line=$(printf '%s\n' "$output" | awk '/Network policy verified:/ { print NR; exit }')
  stage_line=$(printf '%s\n' "$output" | awk '/DARWIN_BOOTSTRAP_STAGE_TWO=/ { print NR; exit }')
  if [[ "$verification_line" -ge "$stage_line" ]]; then
    fail "stage two ran before the packaged bootstrap reported firewall verification"
  fi

  printf 'PASS: packaged Darwin launcher verifies network bootstrap before stage two\n'
}

test_darwin_network_bootstrap
