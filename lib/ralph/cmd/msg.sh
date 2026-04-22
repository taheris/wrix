#!/usr/bin/env bash
set -euo pipefail

# ralph msg — Async human communication for agent questions
#
# Usage:
#   ralph msg                          # List all outstanding questions
#   ralph msg -s <label>               # List for specific spec
#   ralph msg -i <id>                  # Show specific question
#   ralph msg -i <id> "answer"         # Reply (answer as positional)
#   ralph msg -i <id> -d               # Dismiss without answering

RALPH_DIR="${RALPH_DIR:-.wrapix/ralph}"

# Load shared helpers
# shellcheck source=util.sh
source "$(dirname "$0")/util.sh"

# Parse flags
SPEC_FILTER=""
BEAD_ID=""
DISMISS=false
ANSWER=""

while [ $# -gt 0 ]; do
  case "$1" in
    -s|--spec)
      if [ -z "${2:-}" ]; then
        error "Flag $1 requires a spec label argument"
      fi
      SPEC_FILTER="$2"
      shift 2
      ;;
    --spec=*)
      SPEC_FILTER="${1#--spec=}"
      shift
      ;;
    -i|--id)
      if [ -z "${2:-}" ]; then
        error "Flag $1 requires a bead ID argument"
      fi
      BEAD_ID="$2"
      shift 2
      ;;
    --id=*)
      BEAD_ID="${1#--id=}"
      shift
      ;;
    -d|--dismiss)
      DISMISS=true
      shift
      ;;
    -h|--help)
      echo "Usage: ralph msg [flags] [answer]"
      echo ""
      echo "Async human communication for agent questions."
      echo ""
      echo "Modes:"
      echo "  ralph msg                    List all outstanding questions"
      echo "  ralph msg -s <label>         List for specific spec"
      echo "  ralph msg -i <id>            Show specific question"
      echo "  ralph msg -i <id> \"answer\"   Reply to a question"
      echo "  ralph msg -i <id> -d         Dismiss without answering"
      echo ""
      echo "Flags:"
      echo "  -s, --spec <label>   Filter by spec label"
      echo "  -i, --id <id>        Target specific question"
      echo "  -d, --dismiss        Dismiss without answering"
      echo "  -h, --help           Show this help"
      exit 0
      ;;
    -*)
      error "Unknown flag: $1 (see ralph msg --help)"
      ;;
    *)
      # Positional argument — treat as answer text
      ANSWER="$1"
      shift
      ;;
  esac
done

#-----------------------------------------------------------------------------
# Determine source label from bead labels
# Looks for source:worker, source:reviewer, source:watcher
#-----------------------------------------------------------------------------
get_source_label() {
  local labels_json="$1"
  local source="unknown"

  if echo "$labels_json" | jq -e 'map(select(. == "source:worker")) | length > 0' >/dev/null 2>&1; then
    source="worker"
  elif echo "$labels_json" | jq -e 'map(select(. == "source:reviewer")) | length > 0' >/dev/null 2>&1; then
    source="reviewer"
  elif echo "$labels_json" | jq -e 'map(select(. == "source:watcher")) | length > 0' >/dev/null 2>&1; then
    source="watcher"
  fi

  echo "$source"
}

#-----------------------------------------------------------------------------
# Extract spec label from bead labels (spec-<label>)
#-----------------------------------------------------------------------------
get_spec_from_labels() {
  local labels_json="$1"
  echo "$labels_json" | jq -r '[.[] | select(startswith("spec-"))] | first // "—"' 2>/dev/null | sed 's/^spec-//'
}

#-----------------------------------------------------------------------------
# Resolve the question text for a clarify bead.
# Prefer the clarify note appended to the description (current convention);
# fall back to a legacy "Question: ..." line in notes for older beads.
#-----------------------------------------------------------------------------
get_question_for_bead() {
  local description="$1"
  local notes="$2"

  local question
  question=$(extract_clarify_note "$description")
  if [ -n "$question" ]; then
    echo "$question"
    return 0
  fi

  question=$(echo "$notes" | grep -oP '^Question:\s*\K.*' | tail -1 || true)
  if [ -z "$question" ]; then
    question="$notes"
  fi
  echo "$question"
}

#-----------------------------------------------------------------------------
# Reset the run ↔ check iteration counter for the bead's spec (if any).
# Called after a clarify is cleared so the next ralph run starts fresh.
#-----------------------------------------------------------------------------
reset_iteration_for_bead() {
  local bead_id="$1"

  local labels_json
  labels_json=$(bd_json show "$bead_id" --json 2>/dev/null | jq -c '.[0].labels // []' 2>/dev/null || echo '[]')

  local bead_spec
  bead_spec=$(get_spec_from_labels "$labels_json")

  if [ -z "$bead_spec" ] || [ "$bead_spec" = "—" ]; then
    return 0
  fi

  reset_iteration_count "$RALPH_DIR/state/${bead_spec}.json"
}

#-----------------------------------------------------------------------------
# Print one-line resume hint after clearing a clarify.
# When the bead's spec label matches state/current (or current is unset), the
# hint is `ralph run`; otherwise it appends `-s <label>` so the user resumes
# the right workflow.
#-----------------------------------------------------------------------------
print_resume_hint() {
  local bead_id="$1"

  local labels_json
  labels_json=$(bd_json show "$bead_id" --json 2>/dev/null | jq -c '.[0].labels // []' 2>/dev/null || echo '[]')

  local bead_spec
  bead_spec=$(get_spec_from_labels "$labels_json")

  local current_spec=""
  local current_file="$RALPH_DIR/state/current"
  if [ -f "$current_file" ]; then
    current_spec=$(<"$current_file")
    current_spec="${current_spec#"${current_spec%%[![:space:]]*}"}"
    current_spec="${current_spec%"${current_spec##*[![:space:]]}"}"
  fi

  if [ -n "$bead_spec" ] && [ "$bead_spec" != "—" ] && [ "$bead_spec" != "$current_spec" ]; then
    echo "Clarify cleared on $bead_id. Resume with: ralph run -s $bead_spec"
  else
    echo "Clarify cleared on $bead_id. Resume with: ralph run"
  fi
}

#-----------------------------------------------------------------------------
# Mode: Show specific question
#-----------------------------------------------------------------------------
if [ -n "$BEAD_ID" ] && [ -z "$ANSWER" ] && [ "$DISMISS" = "false" ]; then
  # Show mode: display the specific question
  bd show "$BEAD_ID"
  exit 0
fi

#-----------------------------------------------------------------------------
# Mode: Reply to a question
#-----------------------------------------------------------------------------
if [ -n "$BEAD_ID" ] && [ -n "$ANSWER" ]; then
  debug "Replying to $BEAD_ID with answer"

  # Store the answer in notes
  bd update "$BEAD_ID" --append-notes "Answer: $ANSWER" || error "Failed to store answer for $BEAD_ID"

  # Remove ralph:clarify label
  remove_clarify_label "$BEAD_ID"

  # Reset the auto-iteration counter so ralph run starts the clock fresh.
  reset_iteration_for_bead "$BEAD_ID"

  print_resume_hint "$BEAD_ID"
  exit 0
fi

#-----------------------------------------------------------------------------
# Mode: Dismiss a question
#-----------------------------------------------------------------------------
if [ -n "$BEAD_ID" ] && [ "$DISMISS" = "true" ]; then
  debug "Dismissing $BEAD_ID"

  # Store dismissal note
  bd update "$BEAD_ID" --append-notes "Dismissed: Agent should work around this question." || error "Failed to store dismissal for $BEAD_ID"

  # Remove ralph:clarify label
  remove_clarify_label "$BEAD_ID"

  # Reset the auto-iteration counter so ralph run starts the clock fresh.
  reset_iteration_for_bead "$BEAD_ID"

  echo "Dismissed $BEAD_ID. The agent will proceed without an answer on its next iteration."
  exit 0
fi

#-----------------------------------------------------------------------------
# Mode: List outstanding questions
#-----------------------------------------------------------------------------

# Query beads with ralph:clarify label (optionally filtered by spec)
QUESTIONS_JSON=$(list_clarify_beads "$SPEC_FILTER") || {
  echo "No outstanding questions."
  exit 0
}

# Check if we got any results
QUESTION_COUNT=$(echo "$QUESTIONS_JSON" | jq 'if type == "array" then length else 0 end' 2>/dev/null || echo "0")

if [ "$QUESTION_COUNT" -eq 0 ]; then
  echo "No outstanding questions."
  exit 0
fi

# Print header
printf "%-14s  %-16s  %-10s  %s\n" "ID" "SPEC" "SOURCE" "QUESTION"
printf "%-14s  %-16s  %-10s  %s\n" "--------------" "----------------" "----------" "----------------------------------------"

# Print each question
echo "$QUESTIONS_JSON" | jq -c '.[]' | while IFS= read -r item; do
  local_id=$(echo "$item" | jq -r '.id // "—"')
  local_labels=$(echo "$item" | jq -c '.labels // []')
  local_notes=$(echo "$item" | jq -r '.notes // ""')
  local_description=$(echo "$item" | jq -r '.description // ""')

  local_spec=$(get_spec_from_labels "$local_labels")
  local_source=$(get_source_label "$local_labels")
  local_question=$(get_question_for_bead "$local_description" "$local_notes")

  # Truncate question for table display
  if [ ${#local_question} -gt 60 ]; then
    local_question="${local_question:0:57}..."
  fi

  printf "%-14s  %-16s  %-10s  %s\n" "$local_id" "$local_spec" "$local_source" "$local_question"
done
