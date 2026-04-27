#!/usr/bin/env bash
set -euo pipefail

# ralph watch -s <label> [--panes <panes>]
# Observation agent that monitors running services and creates beads for detected issues.
#
# Architecture:
#   Host runs a poll loop (no LLM — just a timer) that spawns a fresh contained
#   Claude session each cycle. Each session reads watch state from
#   state/<label>.watch.md, captures new output, evaluates it, updates state file,
#   and exits. Short-lived sessions avoid context compaction — state is persisted
#   explicitly.
#
# Options:
#   -s/--spec <label>   Required. Spec label to monitor.
#   --panes <panes>     Optional. Tmux panes to observe (passed to watch template).

# Parse flags
SPEC_FLAG=""
PANES_FLAG=""

for arg in "$@"; do
  if [ "${_next_is_spec:-}" = "1" ]; then
    SPEC_FLAG="$arg"
    unset _next_is_spec
    continue
  fi
  if [ "${_next_is_panes:-}" = "1" ]; then
    PANES_FLAG="$arg"
    unset _next_is_panes
    continue
  fi
  case "$arg" in
    --spec|-s)
      _next_is_spec=1
      ;;
    --spec=*)
      SPEC_FLAG="${arg#--spec=}"
      ;;
    --panes)
      _next_is_panes=1
      ;;
    --panes=*)
      PANES_FLAG="${arg#--panes=}"
      ;;
    -h|--help)
      echo "Usage: ralph watch -s <label> [--panes <panes>]"
      echo ""
      echo "Monitor running services and create beads for detected issues."
      echo ""
      echo "Options:"
      echo "  -s, --spec <label>   Spec label to monitor (required)"
      echo "  --panes <panes>      Tmux panes to observe"
      echo ""
      echo "The watch agent polls on a configurable interval (watch.poll-interval"
      echo "in config.nix, default 30s). Each cycle spawns a fresh Claude session"
      echo "that reads watch state, observes logs/browser, and creates beads for"
      echo "detected issues."
      echo ""
      echo "Config (in config.nix):"
      echo "  watch.poll-interval   Seconds between cycles (default: 30)"
      echo "  watch.max-issues      Max beads before pausing (default: 10)"
      echo "  watch.ignore-patterns Noise filter patterns (default: [])"
      exit 0
      ;;
    *)
      echo "Unknown argument: $arg" >&2
      echo "Usage: ralph watch -s <label> [--panes <panes>]" >&2
      exit 1
      ;;
  esac
done
unset _next_is_spec
unset _next_is_panes

# Validate required -s flag
if [ -z "$SPEC_FLAG" ]; then
  echo "Error: -s/--spec <label> is required" >&2
  echo "Usage: ralph watch -s <label> [--panes <panes>]" >&2
  exit 1
fi

# Container detection: if not in container and wrapix is available, re-launch in container
if [ ! -f "${WRAPIX_CLAUDE_CONFIG:-/etc/wrapix/claude-config.json}" ] && command -v wrapix &>/dev/null; then
  export RALPH_MODE=1
  export RALPH_CMD=watch
  RALPH_ARGS_PARTS="--spec $SPEC_FLAG"
  if [ -n "$PANES_FLAG" ]; then
    RALPH_ARGS_PARTS="$RALPH_ARGS_PARTS --panes $PANES_FLAG"
  fi
  export RALPH_ARGS="$RALPH_ARGS_PARTS"
  wrapix
  wrapix_exit=$?
  bd dolt pull || echo "Warning: bd dolt pull failed (beads may not be synced)"
  exit $wrapix_exit
fi

# Load shared helpers
# shellcheck source=util.sh
source "$(dirname "$0")/util.sh"

# Warn early if scripts or templates are stale
check_ralph_staleness

# Pull latest beads state
debug "Pulling beads database..."
bd dolt commit >/dev/null 2>&1 || warn "bd dolt commit failed, continuing"
bd dolt pull >/dev/null 2>&1 || warn "bd dolt pull failed, continuing with local state"

RALPH_DIR="${RALPH_DIR:-.wrapix/ralph}"
CONFIG_FILE="$RALPH_DIR/config.nix"
SPECS_DIR="specs"
PINNED_CONTEXT_FILE=$(get_pinned_context_file)

# Resolve spec label
FEATURE_NAME=$(resolve_spec_label "$SPEC_FLAG")
MOLECULE_ID=""

# Read state from per-label state file
STATE_FILE="$RALPH_DIR/state/${FEATURE_NAME}.json"
if [ -f "$STATE_FILE" ]; then
  # best-effort: malformed/missing field -> empty MOLECULE_ID, caller handles
  MOLECULE_ID=$(jq -r '.molecule // empty' "$STATE_FILE" || true)
fi

if [ -z "$MOLECULE_ID" ]; then
  error "No molecule ID found for '$FEATURE_NAME'. Run 'ralph todo' first."
fi

require_file "$CONFIG_FILE" "Ralph config"

# Load config
debug "Loading config from $CONFIG_FILE"
CONFIG=$(nix eval --json --file "$CONFIG_FILE") || error "Failed to evaluate config: $CONFIG_FILE"
if ! validate_json "$CONFIG" "Config"; then
  error "Config file did not produce valid JSON"
fi

# Read watch settings from config
POLL_INTERVAL=$(echo "$CONFIG" | jq -r '.watch."poll-interval" // 30')
MAX_ISSUES=$(echo "$CONFIG" | jq -r '.watch."max-issues" // 10')
IGNORE_PATTERNS=$(echo "$CONFIG" | jq -r '.watch."ignore-patterns" // []')

# Load model override for watch phase
MODEL_WATCH=$(resolve_model "watch" "$CONFIG")
if [ -n "$MODEL_WATCH" ]; then
  debug "Model override for watch phase: $MODEL_WATCH"
fi

debug "Watch config: poll-interval=$POLL_INTERVAL max-issues=$MAX_ISSUES"

# Compute spec path
SPEC_HIDDEN="false"
if spec_is_hidden "$STATE_FILE"; then
  SPEC_HIDDEN="true"
fi

SPEC_PATH=""
if [ "$SPEC_HIDDEN" = "true" ]; then
  SPEC_PATH="$RALPH_DIR/state/$FEATURE_NAME.md"
else
  SPEC_PATH="$SPECS_DIR/$FEATURE_NAME.md"
fi

# Write initial watch state file if it doesn't exist
WATCH_STATE_FILE="$RALPH_DIR/state/${FEATURE_NAME}.watch.md"
if [ ! -f "$WATCH_STATE_FILE" ]; then
  mkdir -p "$(dirname "$WATCH_STATE_FILE")"
  cat > "$WATCH_STATE_FILE" << EOF
# Watch State: $FEATURE_NAME

## Session Info
- Spec: $FEATURE_NAME
- Molecule: $MOLECULE_ID
- Started: $(date -u +%Y-%m-%dT%H:%M:%SZ)
- Panes: ${PANES_FLAG:-all}

## Ignore Patterns
$(echo "$IGNORE_PATTERNS" | { jq -r '.[] // empty' || true; } | sed 's/^/- /' || echo "- (none)")

## Known Issues
(none yet)

## Observations
(first session pending)
EOF
  debug "Created initial watch state: $WATCH_STATE_FILE"
fi

# Graceful shutdown handling
RUNNING=true
cleanup() {
  echo ""
  echo "Watch interrupted. Stopping..."
  RUNNING=false
}
trap cleanup SIGINT SIGTERM

echo "Ralph watch starting..."
echo "  Spec: $FEATURE_NAME"
echo "  Molecule: $MOLECULE_ID"
echo "  Poll interval: ${POLL_INTERVAL}s"
echo "  Max issues: $MAX_ISSUES"
if [ -n "$PANES_FLAG" ]; then
  echo "  Panes: $PANES_FLAG"
fi
echo ""

#-----------------------------------------------------------------------------
# Poll Loop
#-----------------------------------------------------------------------------
cycle=0

while [ "$RUNNING" = "true" ]; do
  ((++cycle))

  # Check bead count: count source:watch labeled beads in the molecule
  WATCH_BEAD_COUNT=0
  # best-effort: bd list failure -> empty array, downstream uses default count 0
  watch_beads_json=$(bd_json list --label "source:watch" --parent "$MOLECULE_ID" --json || echo "[]")
  if echo "$watch_beads_json" | jq -e 'type == "array"' >/dev/null 2>&1; then
    WATCH_BEAD_COUNT=$(echo "$watch_beads_json" | jq 'length')
  fi
  debug "Watch beads created so far: $WATCH_BEAD_COUNT / $MAX_ISSUES"

  if [ "$WATCH_BEAD_COUNT" -ge "$MAX_ISSUES" ]; then
    echo "Max issues reached ($WATCH_BEAD_COUNT >= $MAX_ISSUES). Pausing watch."
    notify_event "Ralph" "Watch paused: max issues reached for $FEATURE_NAME"
    break
  fi

  echo "=== Watch cycle $cycle ($(date -u +%H:%M:%S)) ==="

  # Record bead count before this cycle for new-issue detection
  beads_before_cycle="$WATCH_BEAD_COUNT"

  # Pin context
  pinned_context=""
  if [ -f "$PINNED_CONTEXT_FILE" ]; then
    pinned_context=$(cat "$PINNED_CONTEXT_FILE")
  fi

  # Read companion paths
  companions=""
  if [ -f "$STATE_FILE" ]; then
    companions=$(list_companion_paths "$STATE_FILE")
  fi

  # Render watch template
  watch_prompt=$(render_template watch \
    "SPEC_PATH=$SPEC_PATH" \
    "LABEL=$FEATURE_NAME" \
    "MOLECULE_ID=$MOLECULE_ID" \
    "COMPANION_PATHS=$companions" \
    "PINNED_CONTEXT=$pinned_context" \
    "EXIT_SIGNALS=")

  mkdir -p "$RALPH_DIR/logs"
  log="$RALPH_DIR/logs/watch-${FEATURE_NAME}-${cycle}.log"

  # Run claude with FRESH CONTEXT (new process each cycle)
  run_claude_stream "$watch_prompt" "$log" "$CONFIG" "$MODEL_WATCH"

  # Check result
  if jq -e '[.[] | select(.type == "result") | .result | contains("RALPH_COMPLETE")] | any' -s "$log" >/dev/null 2>&1; then
    echo "  Observation cycle $cycle complete."
  elif jq -e '[.[] | select(.type == "result") | .result | contains("RALPH_CLARIFY")] | any' -s "$log" >/dev/null 2>&1; then
    echo "  Watch agent needs clarification. Pausing."
    break
  elif jq -e '[.[] | select(.type == "result") | .result | contains("RALPH_BLOCKED")] | any' -s "$log" >/dev/null 2>&1; then
    echo "  Watch agent is blocked. Pausing."
    break
  else
    warn "Watch cycle $cycle did not complete cleanly. Continuing..."
  fi

  # Check for newly created watch beads and notify
  # best-effort: bd list failure -> empty array, caller falls back to prior count
  new_watch_json=$(bd_json list --label "source:watch" --parent "$MOLECULE_ID" --json || echo "[]")
  new_watch_count=0
  if echo "$new_watch_json" | jq -e 'type == "array"' >/dev/null 2>&1; then
    new_watch_count=$(echo "$new_watch_json" | jq 'length')
  fi
  if [ "$new_watch_count" -gt "$beads_before_cycle" ]; then
    # Notify for each new bead
    # best-effort: jq failure -> no notifications, acceptable (WATCH_BEAD_COUNT still updates)
    echo "$new_watch_json" | { jq -r ".[$beads_before_cycle:] | .[].title // empty" || true; } | while IFS= read -r new_title; do
      [ -z "$new_title" ] && continue
      notify_event "Ralph" "New issue detected: $new_title"
    done
    WATCH_BEAD_COUNT="$new_watch_count"
  fi

  # Push beads (incremental sync)
  # best-effort: transient network/dolt errors -> retry next cycle, final sync at end
  bd dolt commit >/dev/null || true
  bd dolt push || true

  # Check if we should continue
  if [ "$RUNNING" != "true" ]; then
    break
  fi

  echo "  Sleeping ${POLL_INTERVAL}s..."
  # Use a loop with short sleeps so SIGINT is responsive
  elapsed=0
  while [ "$elapsed" -lt "$POLL_INTERVAL" ] && [ "$RUNNING" = "true" ]; do
    sleep 1
    elapsed=$((elapsed + 1))
  done
done

# Final sync
echo "Pushing beads to Dolt remote..."
beads-push || echo "Warning: beads-push failed"

echo ""
echo "Watch stopped after $cycle cycle(s)."
