#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=tests/lib/live-sandbox.sh
source "$SCRIPT_DIR/../lib/live-sandbox.sh"

cd "$REPO_ROOT"

TEST_TMP=$(mktemp -d -t wrix-provider-env.XXXXXX)
IMAGE_REF=""
cleanup() {
  rm -rf "$TEST_TMP"
  wrix_remove_image_ref "$IMAGE_REF"
}
trap cleanup EXIT

OPENAI_CANARY="wrix-openai-runtime-canary-$$"
ANTHROPIC_CANARY="wrix-anthropic-runtime-canary-$$"
DEPLOY_KEY="$TEST_TMP/deploy-key"
HOME_DIR="$TEST_TMP/home"
XDG_CACHE_HOME="$TEST_TMP/cache"
PROFILE_CONFIG="$TEST_TMP/profile.json"
SPAWN_CONFIG="$TEST_TMP/spawn.json"
WORKSPACE="$TEST_TMP/workspace"
OUT="$TEST_TMP/launcher.out"
ERR="$TEST_TMP/launcher.err"
mkdir -p "$HOME_DIR" "$XDG_CACHE_HOME" "$WORKSPACE"

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  exit 1
}

write_provider_probe() {
  local workspace="$1"
  local stub_dir="$workspace/bin"
  local stub_path="$stub_dir/loom-direct-runner"

  mkdir -p "$stub_dir"
  cat >"$stub_path" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

probe_workspace="${WRIX_PROVIDER_PROBE_WORKSPACE:-/workspace}"
mkdir -p "$probe_workspace/.wrix"
jq -n \
  --arg agent "${WRIX_AGENT:-}" \
  --arg binary "$(basename "$0")" \
  --arg openai "${OPENAI_API_KEY:-}" \
  --arg anthropic "${ANTHROPIC_API_KEY:-}" \
  '{agent: $agent, binary: $binary, openai: $openai, anthropic: $anthropic}' \
  >"$probe_workspace/.wrix/provider-env.json"
EOF
  chmod +x "$stub_path"
}

assert_provider_observation() {
  local observation="$1"

  [[ -f "$observation" ]] || fail "selected agent did not write provider environment observation"
  if ! jq -e \
    --arg openai "$OPENAI_CANARY" \
    --arg anthropic "$ANTHROPIC_CANARY" \
    '.agent == "direct"
      and .binary == "loom-direct-runner"
      and .openai == $openai
      and .anthropic == $anthropic' \
    "$observation" >/dev/null; then
    fail "selected agent did not receive both runtime provider credentials"
  fi
}

test_provider_probe_contract() {
  local workspace="$TEST_TMP/probe-contract"

  mkdir -p "$workspace"
  write_provider_probe "$workspace"
  WRIX_AGENT=direct \
    WRIX_PROVIDER_PROBE_WORKSPACE="$workspace" \
    OPENAI_API_KEY="$OPENAI_CANARY" \
    ANTHROPIC_API_KEY="$ANTHROPIC_CANARY" \
    "$workspace/bin/loom-direct-runner"
  assert_provider_observation "$workspace/.wrix/provider-env.json"
}

assert_profile_config_is_runtime_only() {
  local profile_config="$1"
  local key

  for key in OPENAI_API_KEY ANTHROPIC_API_KEY; do
    if jq -e --arg key "$key" '.profile.env | has($key)' "$profile_config" >/dev/null; then
      fail "ProfileConfig statically defines provider credential $key"
    fi
    if ! jq -e --arg key "$key" '.security.runtime_secrets[$key] == "optional"' "$profile_config" >/dev/null; then
      fail "ProfileConfig does not declare optional runtime provider credential $key"
    fi
  done
  if grep -aFq -e "$OPENAI_CANARY" -e "$ANTHROPIC_CANARY" "$profile_config"; then
    fail "ProfileConfig contains a runtime provider credential value"
  fi
}

assert_image_is_runtime_only() {
  local image_source="$1"
  local layout config_digest config_blob key digest artifact
  local -a blob_digests

  [[ "$(jq -r '.source_kind' "$image_source")" = "nix-descriptor" ]] \
    || fail "provider verifier requires assembled nix-descriptor metadata"
  for key in OPENAI_API_KEY ANTHROPIC_API_KEY; do
    if jq -e --arg prefix "$key=" \
      'any(.config.env[]?; startswith($prefix))' "$image_source" >/dev/null; then
      fail "image descriptor statically defines provider credential $key"
    fi
  done

  layout=$(jq -er '.oci_layout' "$image_source")
  config_digest=$(jq -er '.oci_manifest.config.digest' "$image_source")
  config_blob="$layout/blobs/sha256/${config_digest#sha256:}"
  [[ -f "$config_blob" ]] || fail "assembled OCI config blob is missing"
  for key in OPENAI_API_KEY ANTHROPIC_API_KEY; do
    if jq -e --arg prefix "$key=" \
      'any(.config.Env[]?; startswith($prefix))' "$config_blob" >/dev/null; then
      fail "assembled OCI config statically defines provider credential $key"
    fi
  done

  mapfile -t blob_digests < <(
    jq -r '[.oci_manifest.config.digest, (.oci_manifest.layers[]?.digest)] | .[]' "$image_source"
  )
  for digest in "${blob_digests[@]}"; do
    [[ "$digest" = sha256:* ]] || fail "assembled OCI metadata contains an invalid blob digest"
    artifact="$layout/blobs/sha256/${digest#sha256:}"
    [[ -f "$artifact" ]] || fail "assembled OCI blob is missing: $digest"
    if grep -aFq -e "$OPENAI_CANARY" -e "$ANTHROPIC_CANARY" "$artifact"; then
      fail "runtime provider credential value was baked into assembled OCI content"
    fi
  done
}

test_provider_artifacts_are_runtime_only() {
  local image_attr image_source image_ref profile_config

  image_attr=$(wrix_agent_image_attr direct)
  image_source=$(
    OPENAI_API_KEY="$OPENAI_CANARY" \
      ANTHROPIC_API_KEY="$ANTHROPIC_CANARY" \
      nix build --impure --no-link --print-out-paths --no-warn-dirty ".#$image_attr.source"
  )
  image_ref=$(wrix_live_image_ref "provider-artifacts-$$")
  profile_config="$TEST_TMP/artifact-profile.json"
  wrix_write_profile_config "$profile_config" "$image_ref" "$image_source" direct
  assert_profile_config_is_runtime_only "$profile_config"
  assert_image_is_runtime_only "$image_source"
}

if [[ "$#" -gt 0 ]]; then
  case "$1" in
    test_provider_artifacts_are_runtime_only) test_provider_artifacts_are_runtime_only ;;
    test_provider_probe_contract) test_provider_probe_contract ;;
    *) fail "unknown provider credential test function: $1" ;;
  esac
  exit 0
fi

wrix_require_live_sandbox_linux
wrix_make_ed25519_key "$DEPLOY_KEY" "provider-env-test"
test_provider_probe_contract
image_attr=$(wrix_agent_image_attr direct)
image_source=$(
  OPENAI_API_KEY="$OPENAI_CANARY" \
    ANTHROPIC_API_KEY="$ANTHROPIC_CANARY" \
    nix build --impure --no-link --print-out-paths --no-warn-dirty ".#$image_attr.source"
)
IMAGE_REF=$(wrix_live_image_ref "provider-env-$$")
wrix_remove_image_ref "$IMAGE_REF"
wrix_write_profile_config "$PROFILE_CONFIG" "$IMAGE_REF" "$image_source" direct
wrix_write_spawn_config "$SPAWN_CONFIG" "$WORKSPACE"
write_provider_probe "$WORKSPACE"

assert_profile_config_is_runtime_only "$PROFILE_CONFIG"
assert_image_is_runtime_only "$image_source"

LAUNCHER=$(wrix_build_live_launcher)
if ! HOME="$HOME_DIR" \
  XDG_CACHE_HOME="$XDG_CACHE_HOME" \
  WRIX_DEPLOY_KEY="$DEPLOY_KEY" \
  WRIX_GIT_SIGN=0 \
  OPENAI_API_KEY="$OPENAI_CANARY" \
  ANTHROPIC_API_KEY="$ANTHROPIC_CANARY" \
  wrix_run_spawn "$LAUNCHER" "$PROFILE_CONFIG" "$SPAWN_CONFIG" >"$OUT" 2>"$ERR"; then
  sed 's/^/  /' "$ERR" >&2
  fail "live launcher session failed"
fi
assert_provider_observation "$WORKSPACE/.wrix/provider-env.json"

printf 'PASS: provider credentials are runtime-only and reach the selected agent\n'
