#!/usr/bin/env bash
# ralph logs - Error-focused log viewer
#
# Usage:
#   ralph logs              # Find most recent error for current spec
#   ralph logs -n 50        # Show 50 lines of context
#   ralph logs --all        # Show full log without error filtering
#   ralph logs --spec <name>  # Show logs for named spec
#   ralph logs -s <name>      # Short form
#   ralph logs <logfile>    # Analyze specific log file
#
# Error patterns scanned:
#   - is_error: true in JSON entries
#   - exit_code != 0 in hook responses
#   - Text patterns: error:, Error:, ERROR, failed, Failed, FAILED, stack traces
set -euo pipefail

# Source shared utilities (provides resolve_spec_label, debug, error, etc.)
# shellcheck source=util.sh
source "$(dirname "${BASH_SOURCE[0]}")/util.sh"

RALPH_DIR="${RALPH_DIR:-.wrapix/ralph}"
CONTEXT_LINES=20
SHOW_ALL=false
LOG_FILE=""
SPEC_FLAG=""

# Parse arguments
while [[ $# -gt 0 ]]; do
  case $1 in
    --spec|-s)
      if [[ -z "${2:-}" ]]; then
        echo "Error: $1 requires a spec name argument" >&2
        exit 1
      fi
      SPEC_FLAG="$2"
      shift 2
      ;;
    --spec=*)
      SPEC_FLAG="${1#--spec=}"
      shift
      ;;
    -n)
      if [[ -z "${2:-}" ]]; then
        echo "Error: -n requires a count argument" >&2
        exit 1
      fi
      CONTEXT_LINES="$2"
      shift 2
      ;;
    --all|-a)
      SHOW_ALL=true
      shift
      ;;
    -h|--help)
      echo "Usage: ralph logs [OPTIONS] [LOGFILE]"
      echo ""
      echo "Error-focused log viewer. Finds errors and shows context."
      echo ""
      echo "Options:"
      echo "  --spec, -s <name>  Show logs for named spec (default: current spec)"
      echo "  -n <count>         Show <count> lines of context before error (default: 20)"
      echo "  --all, -a          Show full log without error filtering"
      echo "  -h, --help         Show this help message"
      echo ""
      echo "Without LOGFILE, finds the most recent work-*.log for the resolved spec."
      exit 0
      ;;
    -*)
      echo "Unknown option: $1" >&2
      exit 1
      ;;
    *)
      LOG_FILE="$1"
      shift
      ;;
  esac
done

# Find most recent log if not specified
if [[ -z "$LOG_FILE" ]]; then
  LOGS_DIR="$RALPH_DIR/logs"
  if [[ ! -d "$LOGS_DIR" ]]; then
    echo "No logs directory found: $LOGS_DIR" >&2
    exit 1
  fi

  # Resolve spec label to filter logs by workflow
  LABEL=$(resolve_spec_label "$SPEC_FLAG")
  BEAD_LABEL="spec:$LABEL"
  debug "logs: resolved label=$LABEL, bead_label=$BEAD_LABEL"

  # Get issue IDs for this spec's workflow
  ISSUE_IDS=$(bd list --label "$BEAD_LABEL" --json 2>/dev/null \
    | jq -r '.[].id // empty' 2>/dev/null) || true

  if [[ -n "$ISSUE_IDS" ]]; then
    # Build a find pattern matching work-<issue_id>.log for each issue
    FIND_ARGS=()
    first=true
    while IFS= read -r issue_id; do
      [[ -z "$issue_id" ]] && continue
      if [[ "$first" == "true" ]]; then
        FIND_ARGS+=(-name "work-${issue_id}.log")
        first=false
      else
        FIND_ARGS+=(-o -name "work-${issue_id}.log")
      fi
    done <<< "$ISSUE_IDS"

    if [[ ${#FIND_ARGS[@]} -gt 0 ]]; then
      LOG_FILE=$(find "$LOGS_DIR" -maxdepth 1 -type f \( "${FIND_ARGS[@]}" \) \
        -printf '%T@\t%p\n' 2>/dev/null \
        | sort -rn \
        | head -1 \
        | cut -f2) || true
    fi
  fi

  # Fallback: if no spec-specific logs found, try most recent work-*.log
  if [[ -z "$LOG_FILE" ]]; then
    LOG_FILE=$(find "$LOGS_DIR" -maxdepth 1 -name "work-*.log" -type f \
      -printf '%T@\t%p\n' 2>/dev/null \
      | sort -rn \
      | head -1 \
      | cut -f2) || true
  fi

  if [[ -z "$LOG_FILE" ]]; then
    echo "No work logs found in $LOGS_DIR" >&2
    exit 1
  fi
fi

# Verify log file exists
if [[ ! -f "$LOG_FILE" ]]; then
  echo "Log file not found: $LOG_FILE" >&2
  exit 1
fi

echo "=== $(basename "$LOG_FILE") ==="
echo ""

# Show all mode - just dump the log nicely formatted
if [[ "$SHOW_ALL" == "true" ]]; then
  jq -r '
    if .type == "assistant" then
      "─── assistant ───\n" + (.message.content // .content // "(no content)")
    elif .type == "user" then
      "─── user ───\n" + (.message.content // .content // "(no content)")
    elif .type == "result" then
      "─── result (" + (.subtype // "unknown") + ") ───\n" + (.result // "(no result)")
    elif .type == "system" then
      "─── system: " + (.subtype // "unknown") + " ───" +
      (if .exit_code then " [exit: " + (.exit_code | tostring) + "]" else "" end) +
      (if .output then "\n" + .output else "" end)
    else
      "─── " + (.type // "unknown") + " ───"
    end
  ' "$LOG_FILE" 2>/dev/null || cat "$LOG_FILE"
  exit 0
fi

# Error scanning mode - find first error and show context

# Find the first error line using grep for fast initial scan
# IMPORTANT: Do NOT use jq -s (slurp) or per-line jq - both cause OOM in containers
# Strategy: grep for potential errors (streaming), then validate with targeted jq
find_error_line() {
  local file="$1"

  # Step 1: Use grep to find lines with JSON error indicators (streaming, memory-safe)
  # Check for is_error:true, non-zero exit_code, or subtype:error
  local json_error_line
  json_error_line=$(grep -n -m1 -E '"is_error"\s*:\s*true|"exit_code"\s*:\s*[1-9]|"subtype"\s*:\s*"error"' "$file" 2>/dev/null | cut -d: -f1) || true

  # Step 2: Use grep to find lines with error text patterns (streaming, memory-safe)
  local text_error_line
  text_error_line=$(grep -n -m1 -iE \
    'error:|failed|exception|panic:|stack trace|traceback' \
    "$file" 2>/dev/null | cut -d: -f1) || true

  # Filter out false positives (0 errors, RALPH_COMPLETE, etc)
  if [[ -n "$text_error_line" ]]; then
    local line_content
    line_content=$(sed -n "${text_error_line}p" "$file")
    if echo "$line_content" | grep -qE '0 errors|no errors|errors: 0|RALPH_COMPLETE'; then
      # False positive - continue searching from this line
      local remaining
      remaining=$(tail -n "+$((text_error_line + 1))" "$file" | grep -n -m1 -iE \
        'error:|failed|exception|panic:|stack trace|traceback' 2>/dev/null | cut -d: -f1) || true
      if [[ -n "$remaining" ]]; then
        text_error_line=$((text_error_line + remaining))
        # Re-check for false positives
        line_content=$(sed -n "${text_error_line}p" "$file")
        if echo "$line_content" | grep -qE '0 errors|no errors|errors: 0|RALPH_COMPLETE'; then
          text_error_line=""
        fi
      else
        text_error_line=""
      fi
    fi
  fi

  # Return the earliest error found
  if [[ -n "$json_error_line" && -n "$text_error_line" ]]; then
    if [[ "$json_error_line" -lt "$text_error_line" ]]; then
      echo "$json_error_line"
    else
      echo "$text_error_line"
    fi
  elif [[ -n "$json_error_line" ]]; then
    echo "$json_error_line"
  elif [[ -n "$text_error_line" ]]; then
    echo "$text_error_line"
  else
    echo "0"
  fi
}

# Find the error line
ERROR_LINE=$(find_error_line "$LOG_FILE")

if [[ "$ERROR_LINE" -eq 0 ]]; then
  echo "No errors found in log."
  echo ""
  echo "Last result:"
  jq -r 'select(.type == "result") |
    "Status: " + (.subtype // "unknown") +
    (if .result then "\n" + .result else "" end)
  ' "$LOG_FILE" 2>/dev/null | tail -30
  exit 0
fi

# Calculate context range
START_LINE=$((ERROR_LINE - CONTEXT_LINES))
if [[ $START_LINE -lt 1 ]]; then
  START_LINE=1
fi

echo "Error found at line $ERROR_LINE (showing $CONTEXT_LINES lines of context)"
echo ""

# Show context leading up to and including the error
sed -n "${START_LINE},${ERROR_LINE}p" "$LOG_FILE" | jq -r '
  if .type == "assistant" then
    "─── assistant ───\n" + (.message.content // .content // "(no content)")
  elif .type == "user" then
    "─── user ───\n" + (.message.content // .content // "(no content)")
  elif .type == "result" then
    "─── result (" + (.subtype // "unknown") + ") ───\n" + (.result // "(no result)")
  elif .type == "system" then
    "─── system: " + (.subtype // "unknown") + " ───" +
    (if .exit_code then " [exit: " + (.exit_code | tostring) + "]" else "" end) +
    (if .output then "\n" + .output else "" end)
  else
    "─── " + (.type // "unknown") + " ───"
  end
' 2>/dev/null || sed -n "${START_LINE},${ERROR_LINE}p" "$LOG_FILE"

# Highlight what triggered the error detection
echo ""
echo "═══ Error detected at line $ERROR_LINE ═══"
sed -n "${ERROR_LINE}p" "$LOG_FILE" | jq -C '.' 2>/dev/null || sed -n "${ERROR_LINE}p" "$LOG_FILE"
