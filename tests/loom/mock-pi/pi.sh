#!/usr/bin/env bash
# Mock pi binary for loom-agent tests.
#
# Reads NDJSON commands on stdin, emits NDJSON responses+events on stdout.
# The first argument selects a behavior mode used by the unit tests in
# loom-agent/src/pi/backend.rs:
#
#   probe-ok                  — respond to get_commands with the full
#                               required command set; then echo a prompt
#                               as a message_delta + agent_end.
#   probe-missing-set-model   — respond to get_commands omitting
#                               'set_model' so the driver fails fast.
#   echo-prompt               — like probe-ok but the echoed message_delta
#                               contains the literal prompt payload so the
#                               test can assert the wire shape.
#   steering                  — probe ok; on prompt emit a first turn +
#                               turn_end; on the next stdin line (steer),
#                               echo its payload back as a message_delta +
#                               turn_end, then agent_end.
#   compaction                — probe ok; on prompt emit compaction_start;
#                               on the next stdin line (re-pin steer),
#                               echo it back as a message_delta + emit
#                               compaction_end + agent_end.
#   set-model                 — probe ok; on the next command (expected
#                               set_model) respond ok and echo the
#                               provider/modelId pair into a later
#                               message_delta after the prompt.
#
# Modes are deliberately small — this binary is exercised via cargo test
# from the loom-agent crate; the surface is shaped to those tests, not to
# real pi semantics.
set -euo pipefail

MODE="${1:-default}"

# pi RPC framing is NDJSON: one complete object per line. Unbuffer stdout
# so the consumer sees each line as soon as it is written.
exec 1> >(stdbuf -oL cat)

emit() {
    printf '%s\n' "$1"
}

# Pull a string field out of a JSON line via sed. Good enough for this
# mock — the protocol values we care about don't contain escaped quotes.
extract_field() {
    local field="$1" line="$2"
    sed -n "s/.*\"${field}\":\"\\([^\"]*\\)\".*/\\1/p" <<<"$line"
}

emit_response_ok() {
    local id="$1" command="$2" data="${3:-null}"
    emit "{\"type\":\"response\",\"id\":\"${id}\",\"command\":\"${command}\",\"success\":true,\"data\":${data}}"
}

emit_response_err() {
    local id="$1" command="$2" error="$3"
    emit "{\"type\":\"response\",\"id\":\"${id}\",\"command\":\"${command}\",\"success\":false,\"error\":\"${error}\"}"
}

emit_message_delta() {
    local text="$1"
    text="${text//\\/\\\\}"
    text="${text//\"/\\\"}"
    emit "{\"type\":\"message_update\",\"assistantMessageEvent\":{\"type\":\"text_delta\",\"text\":\"${text}\"}}"
}

emit_turn_end() {
    emit '{"type":"turn_end","message":{},"toolResults":[]}'
}

emit_agent_end() {
    emit '{"type":"agent_end","messages":[]}'
}

emit_compaction_start() {
    emit '{"type":"compaction_start","reason":"threshold"}'
}

emit_compaction_end() {
    emit '{"type":"compaction_end","aborted":false,"reason":"threshold","willRetry":false}'
}

# Read the first command (must be get_commands) and either echo a full
# command set, or omit set_model when the first arg is "1".
handle_probe() {
    local missing_set_model="${1:-0}"
    local probe_line probe_id data
    IFS= read -r probe_line
    probe_id="$(extract_field id "$probe_line")"
    if [ -z "$probe_id" ]; then
        echo "mock-pi: probe missing id field" >&2
        exit 2
    fi
    if [ "$missing_set_model" = "1" ]; then
        data='["prompt","steer","abort"]'
    else
        data='["prompt","steer","abort","set_model","compact","get_session_stats"]'
    fi
    emit_response_ok "$probe_id" "get_commands" "$data"
}

run_probe_ok() {
    handle_probe 0
    local _prompt
    IFS= read -r _prompt
    emit_message_delta "ack"
    emit_agent_end
}

run_echo_prompt() {
    handle_probe 0
    local prompt_line message
    IFS= read -r prompt_line
    message="$(extract_field message "$prompt_line")"
    emit_message_delta "echo: ${message}"
    emit_agent_end
}

run_steering() {
    handle_probe 0
    local _prompt steer_line steer_msg
    IFS= read -r _prompt
    emit_message_delta "first turn response"
    emit_turn_end

    IFS= read -r steer_line
    steer_msg="$(extract_field message "$steer_line")"
    emit_message_delta "ack ${steer_msg}"
    emit_turn_end
    emit_agent_end
}

run_compaction() {
    handle_probe 0
    local _prompt steer_line repin_msg
    IFS= read -r _prompt
    emit_compaction_start

    IFS= read -r steer_line
    repin_msg="$(extract_field message "$steer_line")"
    emit_message_delta "repin: ${repin_msg}"
    emit_compaction_end
    emit_agent_end
}

run_set_model() {
    handle_probe 0
    local set_model_line sm_id sm_type provider model_id _prompt
    IFS= read -r set_model_line
    sm_id="$(extract_field id "$set_model_line")"
    sm_type="$(extract_field type "$set_model_line")"
    provider="$(extract_field provider "$set_model_line")"
    model_id="$(extract_field modelId "$set_model_line")"
    if [ "$sm_type" != "set_model" ]; then
        emit_response_err "${sm_id:-unknown}" "${sm_type:-unknown}" "expected set_model"
        return
    fi
    emit_response_ok "$sm_id" "set_model"

    IFS= read -r _prompt
    emit_message_delta "model:${provider}:${model_id}"
    emit_agent_end
}

case "$MODE" in
    probe-ok)
        run_probe_ok
        ;;
    probe-missing-set-model)
        handle_probe 1
        ;;
    echo-prompt)
        run_echo_prompt
        ;;
    steering)
        run_steering
        ;;
    compaction)
        run_compaction
        ;;
    set-model)
        run_set_model
        ;;
    *)
        echo "mock-pi: unknown mode: $MODE" >&2
        exit 2
        ;;
esac
