#!/usr/bin/env bash
# Mock claude binary for loom-agent tests.
#
# Reads stream-json on stdin, emits stream-json on stdout. The first
# argument selects a behavior mode used by the unit tests in
# loom-agent/src/claude/backend.rs:
#
#   steering       — emit one assistant turn, wait for a steer message on
#                    stdin, emit a second assistant turn that echoes the
#                    steer payload, then emit result/success and exit.
#
#   ignore-stdin   — emit result/success, then ignore SIGTERM and stdin
#                    close so the test exercises the SIGTERM → SIGKILL
#                    escalation in the shutdown watchdog.
#
# Modes are deliberately small — this binary is exercised via cargo test
# from the loom-agent crate; the surface is shaped to those tests, not to
# real claude semantics.
set -euo pipefail

MODE="${1:-default}"

# stream-json envelopes are NDJSON: one complete object per line. unbuffer
# stdout (stdbuf -oL) so the consumer reads each line as soon as it is
# written rather than waiting on the default block-buffered flush.
exec 1> >(stdbuf -oL cat)

emit() {
    printf '%s\n' "$1"
}

emit_assistant_text() {
    local text="$1"
    # Escape backslashes and double-quotes so the embedded text survives
    # round-tripping through bash → JSON → serde.
    text="${text//\\/\\\\}"
    text="${text//\"/\\\"}"
    emit "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"${text}\"}]}}"
}

emit_result_success() {
    emit '{"type":"result","subtype":"success","total_cost_usd":0.0,"duration_ms":1,"num_turns":1,"is_error":false}'
}

case "$MODE" in
    steering)
        # Read first user message (initial prompt). We don't parse it —
        # the test only cares that the mock emits its turn after seeing
        # any input.
        IFS= read -r _initial
        emit_assistant_text "first turn response"

        # Wait for the driver's steer message. The test passes a string
        # containing "STEERED_TEXT"; we emit it back so the test can
        # assert the second turn was triggered by the steer.
        IFS= read -r steer_line
        # Pull the content field out via a sloppy regex. jq is not a hard
        # dependency in the wrapix tests env, but stream-json messages
        # use a stable shape so a substring match is sufficient.
        if [[ "$steer_line" == *STEERED_TEXT* ]]; then
            emit_assistant_text "ack STEERED_TEXT"
        else
            emit_assistant_text "ack unknown steer: $steer_line"
        fi

        emit_result_success
        exit 0
        ;;
    ignore-stdin)
        # Read once so the test's prompt() call doesn't block.
        IFS= read -r _initial || true
        emit_result_success

        # Trap SIGTERM and SIGPIPE so the test's shutdown watchdog must
        # escalate to SIGKILL. SIGKILL is uncatchable.
        trap '' TERM PIPE
        # Loop forever — kernel reaps us via SIGKILL.
        while true; do
            sleep 0.1
        done
        ;;
    *)
        echo "mock-claude: unknown mode: $MODE" >&2
        exit 2
        ;;
esac
