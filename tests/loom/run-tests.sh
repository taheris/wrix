#!/usr/bin/env bash
# Loom container smoke harness — single happy-path scenario per
# specs/loom-tests.md Functional #5.
#
# What this exercises (host↔container plumbing):
#   1. wrapix run-bead --spawn-config / --stdio argv branching reaches the
#      Linux launcher, mounts the workspace, and runs entrypoint.sh.
#   2. Entrypoint sees WRAPIX_AGENT=pi and execs `pi --mode rpc`. The
#      launcher's per-bead PATH override resolves `pi` to mock-pi inside
#      the container, so MOCK_PI_SCENARIO=happy-path drives the full
#      single-turn lifecycle (probe → prompt → message_delta → agent_end).
#   3. Container exits clean. The harness then closes the bead via real
#      `bd` (per NFR #6 — no mocked bd) and asserts it reports closed.
#
# Out of scope: workflow logic (loom run --once orchestration, retry/clarify,
# parallel dispatch). Those live in cargo nextest integration tests against
# in-process shims; the smoke is the only test that pays for a real container.
#
# Linux-only: Darwin's `nix run .#test-loom` resolves to a no-op stub that
# prints a skip message — the smoke depends on podman, which is not part of
# the Darwin runtime. (NFR #7.)

set -euo pipefail

#-----------------------------------------------------------------------------
# Darwin skip — must short-circuit before any Linux-only setup.
#-----------------------------------------------------------------------------
if [ "$(uname -s)" = "Darwin" ]; then
    echo "container smoke not available on Darwin (no podman dependency on macOS)" >&2
    exit 0
fi

#-----------------------------------------------------------------------------
# Required inputs from the Nix wrapper (tests/loom/default.nix).
#-----------------------------------------------------------------------------
: "${WRAPIX_LOOM_MOCK_PI_SCRIPT:?must be set by the Nix wrapper to mock-pi/pi.sh}"
: "${WRAPIX_LOOM_WRAPIX_BIN:?must be set by the Nix wrapper to a wrapix launcher path}"
: "${WRAPIX_LOOM_TEST_IMAGE_REF:?must be set by the Nix wrapper to the test image tag}"

if ! command -v podman >/dev/null 2>&1; then
    echo "test-loom: podman not on PATH; smoke requires podman" >&2
    exit 1
fi
if ! command -v bd >/dev/null 2>&1; then
    echo "test-loom: bd not on PATH; smoke requires real bd (per NFR #6)" >&2
    exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "test-loom: jq not on PATH" >&2
    exit 1
fi

START_EPOCH=$(date +%s)
TEST_DIR=""

# bd's behaviour is steered by BEADS_*/BD_* env vars. In a wrapix devShell
# the shellHook points BEADS_DOLT_SERVER_SOCKET at the host workspace's
# dolt socket; left in place, every bd command inside the smoke would
# hit that socket instead of our isolated tempdir DB. Strip the inherited
# env so bd falls back to its embedded mode against $TEST_DIR/.beads/.
for _v in ${!BEADS_@} ${!BD_@}; do
    unset "$_v"
done

cleanup() {
    local exit_code=$?
    if [ -n "$TEST_DIR" ] && [ -d "$TEST_DIR" ]; then
        # Stop any per-workspace dolt server bd may have started for the
        # tempdir, then delete the tree. Failure to stop is non-fatal —
        # the dolt server is per-workspace and the workspace is gone anyway.
        bd dolt stop --workspace "$TEST_DIR" >/dev/null 2>&1 || true
        rm -rf "$TEST_DIR"
    fi
    exit "$exit_code"
}
trap cleanup EXIT

#-----------------------------------------------------------------------------
# Tempdir layout: bd lives at $TEST_DIR/.beads/ (host-side state, accessible
# to the harness for create/close), and wrapix's workspace is the empty
# subdir $TEST_DIR/workspace. Splitting them keeps bd's dolt-backend
# config out of the container's `/workspace` mount — the wrapix entrypoint
# refuses to run when it finds `.beads/config.yaml` declaring the dolt
# backend without a reachable socket. The smoke doesn't need bd inside
# the container (mock-pi doesn't touch it) so the simplest fix is to keep
# `.beads/` outside the wrapix workspace tree.
#
# Real bd, real Dolt — per NFR #6. tempfile::tempdir-style isolation per NFR #3.
#-----------------------------------------------------------------------------
TEST_DIR="$(mktemp -d -t loom-smoke-XXXXXX)"
WORKSPACE="$TEST_DIR/workspace"
mkdir -p "$WORKSPACE"

cd "$TEST_DIR"

# Each test run gets its own HOME so dolt's global config doesn't bleed
# between runs and the bd embedded server has somewhere to stash state.
export HOME="$TEST_DIR/home"
mkdir -p "$HOME"

dolt config --global --add user.email "loom-smoke@wrapix.local" >/dev/null 2>&1 || true
dolt config --global --add user.name "loom-smoke" >/dev/null 2>&1 || true

# A tracked workspace is the precondition for `bd init` in non-interactive
# mode — it writes config under .beads/. Disable commit signing on the
# fly: a developer's global ~/.gitconfig may set commit.gpgsign=true and
# point at a key that has no secret material under our overridden $HOME.
git init -q -b main
git -c user.email=loom-smoke@wrapix.local \
    -c user.name=loom-smoke \
    -c commit.gpgsign=false \
    -c tag.gpgsign=false \
    commit -q --allow-empty -m "loom-smoke init"

bd init --prefix loomsmoke --skip-hooks --skip-agents --non-interactive --quiet </dev/null

#-----------------------------------------------------------------------------
# One ready bead labelled profile:base — the unit the smoke "completes".
#-----------------------------------------------------------------------------
BEAD_ID="$(
    bd create \
        --title="loom smoke happy-path" \
        --description="Container smoke: mock-pi happy-path completes one turn." \
        --type=task \
        --priority=2 \
        --labels="profile:base" \
        --json \
    | jq -r '.id'
)"

if [ -z "$BEAD_ID" ] || [ "$BEAD_ID" = "null" ]; then
    echo "test-loom: failed to create test bead" >&2
    exit 1
fi
echo "test-loom: created bead $BEAD_ID"

#-----------------------------------------------------------------------------
# mock-pi shim at a known in-workspace path.
#
# `pi --mode rpc` from the entrypoint resolves through PATH; the SpawnConfig
# below prepends /workspace/.wrapix/loom-test-bin to PATH, so the entrypoint
# picks up this shim instead of the image's real pi binary. The shim
# discards the `--mode rpc` argv from the entrypoint and dispatches mock-pi
# by MOCK_PI_SCENARIO env (matches the issue's argv contract).
#-----------------------------------------------------------------------------
LOOM_TEST_BIN="$WORKSPACE/.wrapix/loom-test-bin"
mkdir -p "$LOOM_TEST_BIN"

# Stage mock-pi inside the workspace tree. The container only sees what's
# under /workspace and what's baked into the image; the host Nix store
# path that $WRAPIX_LOOM_MOCK_PI_SCRIPT points at is on the host filesystem,
# not the container's. Copying the mock-pi body into the bind-mounted
# workspace makes it visible to the entrypoint.
cp "$WRAPIX_LOOM_MOCK_PI_SCRIPT" "$LOOM_TEST_BIN/mock-pi.sh"
chmod +x "$LOOM_TEST_BIN/mock-pi.sh"

# `pi` shim: discards CLI flags (the entrypoint passes `pi --mode rpc`)
# and dispatches by MOCK_PI_SCENARIO env. The shim path inside the
# container is /workspace/.wrapix/loom-test-bin/pi.
cat > "$LOOM_TEST_BIN/pi" <<'MOCKPI'
#!/usr/bin/env bash
exec /usr/bin/env bash /workspace/.wrapix/loom-test-bin/mock-pi.sh "${MOCK_PI_SCENARIO:-happy-path}"
MOCKPI
chmod +x "$LOOM_TEST_BIN/pi"

# Loom writes initial-prompt and re-pin files into the workspace before
# invoking wrapix run-bead. Mock-pi happy-path doesn't read them — it just
# echoes a single message_delta — but creating empty placeholders matches
# the contract so a future change to mock-pi (or the entrypoint) doesn't
# break on missing inputs.
mkdir -p "$WORKSPACE/.wrapix/loom"
: > "$WORKSPACE/.wrapix/loom/initial-prompt.md"

#-----------------------------------------------------------------------------
# SpawnConfig — the on-disk shape `wrapix run-bead --spawn-config` consumes.
# See loom-core/src/agent/backend.rs::SpawnConfig.
#
# env is an explicit allowlist:
#   WRAPIX_AGENT=pi             — entrypoint branches into `pi --mode rpc`
#   MOCK_PI_SCENARIO=happy-path — mock-pi shim selects single-turn lifecycle
#   PATH=...                    — prepends mock-pi shim ahead of pi-mono
#-----------------------------------------------------------------------------
SPAWN_CONFIG="$WORKSPACE/.wrapix/loom/spawn-config.json"
jq -n \
    --arg image "$WRAPIX_LOOM_TEST_IMAGE_REF" \
    --arg workspace "$WORKSPACE" \
    '{
        image: $image,
        workspace: $workspace,
        env: [
            ["WRAPIX_AGENT", "pi"],
            ["MOCK_PI_SCENARIO", "happy-path"],
            ["PATH", "/workspace/.wrapix/loom-test-bin:/usr/bin:/bin"]
        ],
        initial_prompt: "smoke",
        agent_args: [],
        repin: { orientation: "", pinned_context: "", partial_bodies: [] }
    }' > "$SPAWN_CONFIG"

#-----------------------------------------------------------------------------
# Drive the container: feed mock-pi happy-path the two stdin lines it
# expects (probe + prompt). agent_end on stdout terminates the script;
# the container exits clean.
#-----------------------------------------------------------------------------
echo "test-loom: invoking wrapix run-bead (image=$WRAPIX_LOOM_TEST_IMAGE_REF)"

CONTAINER_RC=0
{
    printf '%s\n' '{"type":"get_commands","id":"smoke-probe"}'
    printf '%s\n' '{"type":"prompt","id":"smoke-prompt","message":"hello"}'
} | "$WRAPIX_LOOM_WRAPIX_BIN" run-bead \
    --spawn-config "$SPAWN_CONFIG" \
    --stdio \
    > "$WORKSPACE/container.stdout" \
    2> "$WORKSPACE/container.stderr" \
    || CONTAINER_RC=$?

if [ "$CONTAINER_RC" -ne 0 ]; then
    echo "test-loom: container exited with code $CONTAINER_RC (expected 0)" >&2
    echo "--- stdout ---" >&2
    sed 's/^/  /' "$WORKSPACE/container.stdout" >&2 || true
    echo "--- stderr ---" >&2
    sed 's/^/  /' "$WORKSPACE/container.stderr" >&2 || true
    exit 1
fi

# Mock-pi's happy-path emits a message_delta then agent_end. Both lines
# travel back over the same stdio the launcher wired through; if either
# is missing, the container plumbing dropped data.
if ! grep -q '"type":"message_update"' "$WORKSPACE/container.stdout"; then
    echo "test-loom: missing message_update in container stdout" >&2
    sed 's/^/  /' "$WORKSPACE/container.stdout" >&2
    exit 1
fi
if ! grep -q '"type":"agent_end"' "$WORKSPACE/container.stdout"; then
    echo "test-loom: missing agent_end in container stdout" >&2
    sed 's/^/  /' "$WORKSPACE/container.stdout" >&2
    exit 1
fi

#-----------------------------------------------------------------------------
# Bead-close half of the assertion. mock-pi has no notion of bd; the host
# closes the bead after a clean container exit, mirroring what loom would
# do once the workflow layer wires agent_end → bd close.
#-----------------------------------------------------------------------------
bd close "$BEAD_ID" --reason="loom smoke happy-path complete"

# `bd show --json` returns a one-element array, not a single object.
STATUS="$(bd show "$BEAD_ID" --json 2>/dev/null | jq -r '.[0].status' 2>/dev/null || echo "unknown")"
if [ "$STATUS" != "closed" ]; then
    echo "test-loom: bead $BEAD_ID status is '$STATUS' (expected 'closed')" >&2
    exit 1
fi

#-----------------------------------------------------------------------------
# Wall-clock budget per Functional #5: <30s. Enforce it locally so a
# regression in container startup or image load time fails the smoke,
# rather than silently doubling CI time.
#-----------------------------------------------------------------------------
END_EPOCH=$(date +%s)
ELAPSED=$((END_EPOCH - START_EPOCH))
if [ "$ELAPSED" -gt 30 ]; then
    echo "test-loom: smoke took ${ELAPSED}s (>30s budget per spec Functional #5)" >&2
    exit 1
fi

echo "test-loom: PASS — bead $BEAD_ID closed, container exited clean in ${ELAPSED}s"
