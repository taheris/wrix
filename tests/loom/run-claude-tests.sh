#!/usr/bin/env bash
# Loom container smoke for the claude backend — exercises the parallel of
# tests/loom/run-tests.sh but for stream-json claude instead of pi RPC.
#
# What this exercises (host↔container plumbing):
#   1. wrapix spawn --spawn-config / --stdio sets WRAPIX_STDIO=1 in container env.
#   2. Entrypoint sees WRAPIX_AGENT=claude + WRAPIX_STDIO=1 and execs the
#      stream-json branch (claude --print --input-format stream-json
#      --output-format stream-json). The launcher's per-bead PATH override
#      resolves `claude` to mock-claude inside the container.
#   3. Mock-claude happy-path emits system/init → assistant text → result —
#      the entrypoint plumbing carries each line back to the harness.
#
# Without the claude branch (and the launcher's WRAPIX_STDIO=1), this test
# fails: interactive claude reads stdin as user keystrokes and never emits
# stream-json events. That's the bug class wx-z91yn.
#
# Linux-only: Darwin's `nix run .#test-loom-claude` resolves to a no-op stub.
set -euo pipefail

if [ "$(uname -s)" = "Darwin" ]; then
    echo "claude container smoke not available on Darwin" >&2
    exit 0
fi

: "${WRAPIX_LOOM_MOCK_CLAUDE_SCRIPT:?must be set by the Nix wrapper to mock-claude/claude.sh}"
: "${WRAPIX_LOOM_WRAPIX_BIN:?must be set by the Nix wrapper to a wrapix launcher path}"
: "${WRAPIX_LOOM_TEST_IMAGE_REF:?must be set by the Nix wrapper to the test image tag}"
: "${WRAPIX_LOOM_TEST_IMAGE_SOURCE:?must be set by the Nix wrapper to the streamLayeredImage path}"

if ! command -v podman >/dev/null 2>&1; then
    echo "test-loom-claude: podman not on PATH" >&2
    exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "test-loom-claude: jq not on PATH" >&2
    exit 1
fi

START_EPOCH=$(date +%s)
TEST_DIR=""

cleanup() {
    local exit_code=$?
    if [ -n "$TEST_DIR" ] && [ -d "$TEST_DIR" ]; then
        rm -rf "$TEST_DIR"
    fi
    exit "$exit_code"
}
trap cleanup EXIT

TEST_DIR="$(mktemp -d -t loom-claude-smoke-XXXXXX)"
WORKSPACE="$TEST_DIR/workspace"
mkdir -p "$WORKSPACE"
cd "$TEST_DIR"
export HOME="$TEST_DIR/home"
mkdir -p "$HOME"

git init -q "$WORKSPACE" -b main
git -C "$WORKSPACE" \
    -c user.email=loom-claude-smoke@wrapix.local \
    -c user.name=loom-claude-smoke \
    -c commit.gpgsign=false \
    -c tag.gpgsign=false \
    commit -q --allow-empty -m "loom-claude-smoke init"

# mock-claude shim. The entrypoint resolves `claude` through PATH; the
# spawn-config below prepends /workspace/.wrapix/loom-test-bin to PATH so
# the entrypoint picks up this shim instead of the image's real claude
# binary. The shim discards the entrypoint's argv (`--print
# --input-format stream-json --output-format stream-json`) and dispatches
# mock-claude by MOCK_CLAUDE_SCENARIO env.
LOOM_TEST_BIN="$WORKSPACE/.wrapix/loom-test-bin"
mkdir -p "$LOOM_TEST_BIN"
cp "$WRAPIX_LOOM_MOCK_CLAUDE_SCRIPT" "$LOOM_TEST_BIN/mock-claude.sh"
chmod +x "$LOOM_TEST_BIN/mock-claude.sh"

cat > "$LOOM_TEST_BIN/claude" <<'MOCKCLAUDE'
#!/usr/bin/env bash
exec bash /workspace/.wrapix/loom-test-bin/mock-claude.sh "${MOCK_CLAUDE_SCENARIO:-happy-path}"
MOCKCLAUDE
chmod +x "$LOOM_TEST_BIN/claude"

mkdir -p "$WORKSPACE/.wrapix/loom"

# SpawnConfig — see loom-driver/src/agent/backend.rs::SpawnConfig.
#
# env:
#   WRAPIX_AGENT=claude         — entrypoint dispatch
#   MOCK_CLAUDE_SCENARIO=happy-path — mock-claude shim mode
#   PATH=...                    — prepends mock-claude shim ahead of real claude
#
# WRAPIX_STDIO=1 is NOT set here — the launcher sets it itself when --stdio
# is passed, which is precisely the contract this smoke validates.
# Preload the test image — see run-tests.sh for the rationale (the
# launcher's first-run load path is flaky for agent-suffixed `:latest`
# images, and the smoke is meant to validate dispatch, not load logic).
if ! podman image exists "$WRAPIX_LOOM_TEST_IMAGE_REF"; then
    LOAD_OUT="$("$WRAPIX_LOOM_TEST_IMAGE_SOURCE" | podman load)"
    LOADED_IMAGE="$(printf '%s\n' "$LOAD_OUT" | sed -n 's/^Loaded image: //p' | head -n1)"
    if [ -z "$LOADED_IMAGE" ]; then
        echo "test-loom-claude: podman load did not report a loaded image:" >&2
        printf '%s\n' "$LOAD_OUT" >&2
        exit 1
    fi
    if [ "$LOADED_IMAGE" != "$WRAPIX_LOOM_TEST_IMAGE_REF" ]; then
        podman tag "$LOADED_IMAGE" "$WRAPIX_LOOM_TEST_IMAGE_REF"
    fi
fi

SPAWN_CONFIG="$WORKSPACE/.wrapix/loom/spawn-config.json"
jq -n \
    --arg image_ref "$WRAPIX_LOOM_TEST_IMAGE_REF" \
    --arg workspace "$WORKSPACE" \
    '{
        image_ref: $image_ref,
        image_source: "",
        workspace: $workspace,
        env: [
            ["WRAPIX_AGENT", "claude"],
            ["MOCK_CLAUDE_SCENARIO", "happy-path"],
            ["PATH", "/workspace/.wrapix/loom-test-bin:/usr/bin:/bin"]
        ],
        initial_prompt: "smoke",
        agent_args: [],
        repin: { orientation: "", pinned_context: "", partial_bodies: [] }
    }' > "$SPAWN_CONFIG"

echo "test-loom-claude: invoking wrapix spawn (image=$WRAPIX_LOOM_TEST_IMAGE_REF)"

# Feed mock-claude happy-path one stdin line (the prompt). It reads the
# line, emits system/assistant/result, then exits.
CONTAINER_RC=0
{
    printf '%s\n' '{"type":"user","message":{"role":"user","content":"hello"}}'
} | "$WRAPIX_LOOM_WRAPIX_BIN" spawn \
    --spawn-config "$SPAWN_CONFIG" \
    --stdio \
    > "$WORKSPACE/container.stdout" \
    2> "$WORKSPACE/container.stderr" \
    || CONTAINER_RC=$?

if [ "$CONTAINER_RC" -ne 0 ]; then
    echo "test-loom-claude: container exited with code $CONTAINER_RC (expected 0)" >&2
    echo "--- stdout ---" >&2
    sed 's/^/  /' "$WORKSPACE/container.stdout" >&2 || true
    echo "--- stderr ---" >&2
    sed 's/^/  /' "$WORKSPACE/container.stderr" >&2 || true
    exit 1
fi

# Mock-claude's happy-path emits system/init, an assistant text block,
# and a result/success line. All three must reach the host or the
# entrypoint dispatched the wrong branch (e.g., interactive TTY claude
# instead of stream-json).
for marker in '"type":"system"' '"type":"assistant"' '"type":"result"'; do
    if ! grep -qF "$marker" "$WORKSPACE/container.stdout"; then
        echo "test-loom-claude: missing $marker in container stdout" >&2
        sed 's/^/  /' "$WORKSPACE/container.stdout" >&2
        echo "--- stderr ---" >&2
        sed 's/^/  /' "$WORKSPACE/container.stderr" >&2 || true
        exit 1
    fi
done

END_EPOCH=$(date +%s)
ELAPSED=$((END_EPOCH - START_EPOCH))
if [ "$ELAPSED" -gt 30 ]; then
    echo "test-loom-claude: smoke took ${ELAPSED}s (>30s budget)" >&2
    exit 1
fi

echo "test-loom-claude: PASS — stream-json round-trip in ${ELAPSED}s"
