#!/usr/bin/env bash
set -euo pipefail

# ralph status [--spec <name>|-s <name>] [--all] [--watch|-w]
# Show current workflow state using bd mol commands:
# - Current label and spec name
# - Molecule progress (completion %, rate, ETA)
# - Current position in DAG
# - Stale molecule warnings
#
# Modes:
#   ralph status                — show status for current spec (from state/current)
#   ralph status --spec <name>  — show status for a specific spec
#   ralph status -s <name>      — short form of --spec
#   ralph status --all          — summary of ALL active workflows
#
# --watch / -w: Auto-refreshing live view using tmux split panes.
#   Top pane: `watch -n5 ralph status` (molecule progress refresh)
#   Bottom pane: live tail of agent output if ralph run is active,
#                otherwise recent git log + last errors from ralph logs.
#   Requires tmux — errors with a clear message if $TMUX is not set.

# Source shared utilities
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=util.sh
source "$SCRIPT_DIR/util.sh"

RALPH_DIR="${RALPH_DIR:-.wrapix/ralph}"
SPECS_DIR="specs"

#-----------------------------------------------------------------------------
# Helpers
#-----------------------------------------------------------------------------

# Format a duration in seconds as a human-readable age string
# Usage: format_age <seconds>
# Output: "2h ago", "3d ago", "5m ago", etc.
format_age() {
  local seconds="$1"
  if [ "$seconds" -lt 60 ]; then
    echo "${seconds}s ago"
  elif [ "$seconds" -lt 3600 ]; then
    echo "$(( seconds / 60 ))m ago"
  elif [ "$seconds" -lt 86400 ]; then
    echo "$(( seconds / 3600 ))h ago"
  else
    echo "$(( seconds / 86400 ))d ago"
  fi
}

# Display beads with ralph:clarify label for the current spec label
# Usage: show_awaiting_items <bead_label>
# Output: Formatted section showing awaiting items, or nothing if none found
show_awaiting_items() {
  local bead_label="$1"

  # Query beads with both the spec label AND ralph:clarify label
  local awaiting_json
  awaiting_json=$(bd_json list --label "$bead_label" --label "ralph:clarify" --json --limit 50 2>/dev/null) || true

  # Check if we have any results
  if [ -z "$awaiting_json" ] || [ "$awaiting_json" = "[]" ] || ! echo "$awaiting_json" | jq -e 'length > 0' >/dev/null 2>&1; then
    return 0
  fi

  local count
  count=$(echo "$awaiting_json" | jq 'length')

  echo "Awaiting Input ($count):"

  local now
  now=$(date +%s)

  # Iterate over awaiting items
  # Description is placed last because IFS=$'\t' collapses adjacent empty
  # whitespace fields, and description is often empty in bd list output.
  echo "$awaiting_json" | jq -r '.[] | [.id, .title, (.notes // ""), .updated_at, (.description // "")] | @tsv' | while IFS=$'\t' read -r id title notes updated_at description; do
    # Prefer clarify note appended to description; fall back to legacy "Question: " in notes
    local question=""
    if [ -n "$description" ]; then
      question=$(extract_clarify_note "$description") || true
    fi
    if [ -z "$question" ] && [ -n "$notes" ]; then
      question=$(echo "$notes" | grep -oP 'Question:\s*\K.*' | head -1) || true
    fi

    # Calculate age from updated_at
    local age_str=""
    if [ -n "$updated_at" ] && [ "$updated_at" != "null" ]; then
      local updated_epoch
      updated_epoch=$(date -d "$updated_at" +%s 2>/dev/null) || true
      if [ -n "$updated_epoch" ]; then
        local age_seconds=$(( now - updated_epoch ))
        if [ "$age_seconds" -ge 0 ]; then
          age_str=$(format_age "$age_seconds")
        fi
      fi
    fi

    # Display: [awaiting] <id>  <title>
    printf '  [awaiting] %-14s %s\n' "$id" "$title"

    # Display question and age on the next line if available
    if [ -n "$question" ] && [ -n "$age_str" ]; then
      printf '               "%s" (%s)\n' "$question" "$age_str"
    elif [ -n "$question" ]; then
      printf '               "%s"\n' "$question"
    elif [ -n "$age_str" ]; then
      printf '               (%s)\n' "$age_str"
    fi
  done

  echo ""
}

# Helper to indent each line of output
indent() {
  while IFS= read -r line; do
    printf '  %s\n' "$line"
  done
}

# Generate a visual progress bar
# Usage: progress_bar <completed> <total> [<width>]
# Example: progress_bar 4 10 10 => "[####------] 40% (4/10)"
progress_bar() {
  local completed="${1:-0}"
  local total="${2:-0}"
  local width="${3:-10}"

  # Handle edge cases
  if [ "$total" -eq 0 ]; then
    printf "[%s] 0%% (0/0)" "$(printf '%*s' "$width" '' | tr ' ' '-')"
    return
  fi

  # Calculate percentage and filled width
  local percent=$((completed * 100 / total))
  local filled=$((completed * width / total))
  local empty=$((width - filled))

  # Build the bar
  local bar_filled bar_empty
  bar_filled=$(printf '%*s' "$filled" '' | tr ' ' '#')
  bar_empty=$(printf '%*s' "$empty" '' | tr ' ' '-')

  printf "[%s%s] %d%% (%d/%d)" "$bar_filled" "$bar_empty" "$percent" "$completed" "$total"
}

# Determine workflow phase from state file
# Usage: get_workflow_phase <label>
# Output: one of "planning", "todo", "running", "done"
get_workflow_phase() {
  local label="$1"
  local state_file="$RALPH_DIR/state/${label}.json"
  local molecule=""

  if [ -f "$state_file" ]; then
    molecule=$(jq -r '.molecule // empty' "$state_file" 2>/dev/null || true)
  fi

  if [ -z "$molecule" ]; then
    # No molecule yet — check if spec exists
    local spec_path
    spec_path=$(jq -r '.spec_path // ""' "$state_file" 2>/dev/null || echo "")
    if [ -z "$spec_path" ]; then
      # Fallback: derive from label
      spec_path="$SPECS_DIR/$label.md"
    fi
    if [ -f "$spec_path" ]; then
      echo "todo"
    else
      echo "planning"
    fi
    return
  fi

  # Has molecule — check progress
  local progress_json
  progress_json=$(bd_json mol progress "$molecule" --json 2>/dev/null) || true
  if [ -n "$progress_json" ] && echo "$progress_json" | jq empty 2>/dev/null; then
    local completed total
    completed=$(echo "$progress_json" | jq -r '.completed // 0')
    total=$(echo "$progress_json" | jq -r '.total // 0')
    if [ "$total" -gt 0 ] && [ "$completed" -eq "$total" ]; then
      echo "done"
    else
      echo "running"
    fi
  else
    echo "running"
  fi
}

# Get progress stats for a workflow
# Usage: get_workflow_progress <label>
# Output: "completed total" (space-separated)
get_workflow_progress() {
  local label="$1"
  local state_file="$RALPH_DIR/state/${label}.json"
  local molecule=""

  if [ -f "$state_file" ]; then
    molecule=$(jq -r '.molecule // empty' "$state_file" 2>/dev/null || true)
  fi

  if [ -z "$molecule" ]; then
    echo "0 0"
    return
  fi

  local progress_json
  progress_json=$(bd_json mol progress "$molecule" --json 2>/dev/null) || true
  if [ -n "$progress_json" ] && echo "$progress_json" | jq empty 2>/dev/null; then
    local completed total
    completed=$(echo "$progress_json" | jq -r '.completed // 0')
    total=$(echo "$progress_json" | jq -r '.total // 0')
    echo "$completed $total"
  else
    echo "0 0"
  fi
}

# Show detailed status for a single workflow
# Usage: show_single_status <label>
show_single_status() {
  local label="$1"
  local state_file="$RALPH_DIR/state/${label}.json"
  local molecule=""

  if [ -f "$state_file" ]; then
    molecule=$(jq -r '.molecule // empty' "$state_file" 2>/dev/null || true)
  fi

  # Header
  echo "Ralph Status: $label"
  echo "==============================="

  # Molecule ID
  if [ -n "$molecule" ]; then
    echo "Molecule: $molecule"
  else
    echo "Molecule: (not set)"
  fi

  # Spec location (derive from spec_path in state JSON)
  local spec_path
  spec_path=$(jq -r '.spec_path // ""' "$state_file" 2>/dev/null || echo "")
  if [ -z "$spec_path" ]; then
    spec_path="$SPECS_DIR/$label.md"
  fi
  if [[ "$spec_path" == *"/state/"* ]]; then
    echo "Spec: $spec_path (hidden)"
  else
    echo "Spec: $spec_path"
  fi

  echo ""

  # If molecule is set, use bd mol commands for progress tracking
  if [ -n "$molecule" ]; then
    # Progress section - use JSON for reliable parsing
    echo "Progress:"
    local progress_json
    progress_json=$(bd_json mol progress "$molecule" --json 2>/dev/null) || true
    if [ -n "$progress_json" ] && echo "$progress_json" | jq empty 2>/dev/null; then
      # Extract stats from JSON
      local completed total
      completed=$(echo "$progress_json" | jq -r '.completed // 0')
      total=$(echo "$progress_json" | jq -r '.total // 0')

      # Display visual progress bar
      echo "  $(progress_bar "$completed" "$total")"
    else
      # Fallback to label-based counting when molecule commands fail
      echo "  (molecule progress unavailable, using label counts)"
      show_label_progress "$label"
    fi

    echo ""

    # Current position in DAG - use the formatted text output
    echo "Current Position:"
    local current_output
    if current_output=$(bd mol current "$molecule" 2>&1); then
      # Skip the header lines and just show the task list (already indented by bd mol current)
      echo "$current_output" | grep -E '^\s*\[(done|current|ready|blocked|pending)\]' || echo "  (no position markers found)"
    else
      # Fallback: show next ready task
      local bead_label="spec-$label"
      local next_issue
      next_issue=$(bd list --label "$bead_label" --ready --sort priority --limit 1 --json 2>/dev/null | jq -r '.[0].id // empty') || true
      if [ -n "$next_issue" ]; then
        local next_title
        next_title=$(bd show "$next_issue" --json 2>/dev/null | jq -r '.[0].title // empty') || next_title=""
        echo "  Next ready: $next_issue - $next_title"
      else
        local in_progress
        in_progress=$(bd list --label "$bead_label" --status=in_progress --limit 1 --json 2>/dev/null | jq -r '.[0].id // empty') || true
        if [ -n "$in_progress" ]; then
          local in_progress_title
          in_progress_title=$(bd show "$in_progress" --json 2>/dev/null | jq -r '.[0].title // empty') || in_progress_title=""
          echo "  In progress: $in_progress - $in_progress_title"
        else
          echo "  (no tasks ready or in progress)"
        fi
      fi
    fi

    echo ""

    # Show awaiting input items
    show_awaiting_items "spec-$label"

    # Check for stale molecules (hygiene warnings)
    echo "Warnings:"
    local stale_output
    if stale_output=$(bd mol stale --quiet 2>&1) && [ -n "$stale_output" ]; then
      echo "$stale_output" | indent
    else
      echo "  (none)"
    fi
    echo ""
  else
    # Fallback: no molecule set, use legacy label-based counting
    local bead_label="spec-$label"
    echo "Beads Progress ($bead_label):"
    echo "  (No molecule set - using label-based counting)"
    echo ""

    # Count by status
    local open_count in_progress_count closed_count ready_count total
    open_count=$(bd list --label "$bead_label" --status=open 2>/dev/null | wc -l) || open_count=0
    in_progress_count=$(bd list --label "$bead_label" --status=in_progress 2>/dev/null | wc -l) || in_progress_count=0
    closed_count=$(bd list --label "$bead_label" --status=closed 2>/dev/null | wc -l) || closed_count=0
    ready_count=$(bd list --label "$bead_label" --ready 2>/dev/null | wc -l) || ready_count=0

    total=$((open_count + in_progress_count + closed_count))

    echo "  Open:        $open_count"
    echo "  In Progress: $in_progress_count"
    echo "  Closed:      $closed_count"
    echo "  Ready:       $ready_count"
    echo "  Total:       $total"

    if [ "$total" -gt 0 ]; then
      local percent=$((closed_count * 100 / total))
      echo "  Progress:    $percent% complete"
    fi

    echo ""

    # Next ready task
    local next_issue
    next_issue=$(bd list --label "$bead_label" --ready --sort priority --limit 1 --json 2>/dev/null | jq -r '.[0].id // empty') || true
    if [ -n "$next_issue" ]; then
      echo "Next Ready Task:"
      bd show "$next_issue" 2>/dev/null | head -5 || echo "  $next_issue"
    else
      if [ "$total" -gt 0 ] && [ "$closed_count" -eq "$total" ]; then
        echo "All tasks complete!"
      elif [ "$in_progress_count" -gt 0 ]; then
        echo "No ready tasks. $in_progress_count task(s) in progress."
      else
        echo "No tasks found for label: $bead_label"
      fi
    fi

    echo ""

    # Show awaiting input items
    show_awaiting_items "$bead_label"

    echo "Run 'ralph todo' to create a molecule for this spec."
  fi

  echo ""
  echo "Commands:"
  echo "  ralph plan   - Create/continue spec interview"
  echo "  ralph todo   - Convert spec to beads"
  echo "  ralph run    - Work all tasks (or --once for single task)"
}

# Helper function for label-based progress (fallback when molecule commands fail)
show_label_progress() {
  local label="$1"
  local bead_label="spec-$label"

  # Count by status
  local open_count in_progress_count closed_count ready_count total
  open_count=$(bd list --label "$bead_label" --status=open 2>/dev/null | wc -l) || open_count=0
  in_progress_count=$(bd list --label "$bead_label" --status=in_progress 2>/dev/null | wc -l) || in_progress_count=0
  closed_count=$(bd list --label "$bead_label" --status=closed 2>/dev/null | wc -l) || closed_count=0
  ready_count=$(bd list --label "$bead_label" --ready 2>/dev/null | wc -l) || ready_count=0
  total=$((open_count + in_progress_count + closed_count))

  echo "  Open:        $open_count"
  echo "  In Progress: $in_progress_count"
  echo "  Closed:      $closed_count"
  echo "  Ready:       $ready_count"
  echo "  Total:       $total"

  if [ "$total" -gt 0 ]; then
    local percent=$((closed_count * 100 / total))
    echo "  Progress:    $percent% complete"
  fi
}

# Show summary of all active workflows
# Scans state/*.json files and displays each workflow's label, phase, and progress
show_all_status() {
  local state_dir="$RALPH_DIR/state"

  if [ ! -d "$state_dir" ]; then
    echo "No workflows found. Run 'ralph plan <label>' to start."
    return
  fi

  # Find all state JSON files (exclude non-workflow files)
  local found_any=false
  echo "Active Workflows:"

  for state_file in "$state_dir"/*.json; do
    # Skip if glob didn't match
    [ -f "$state_file" ] || continue

    local label
    label=$(jq -r '.label // empty' "$state_file" 2>/dev/null || true)

    # Skip files without a label (not workflow state files)
    [ -z "$label" ] && continue

    found_any=true

    local phase completed total
    phase=$(get_workflow_phase "$label")
    read -r completed total <<< "$(get_workflow_progress "$label")"

    # Format: label  phase  [progress bar] N% (X/Y)
    printf '  %-20s %-10s %s\n' "$label" "$phase" "$(progress_bar "$completed" "$total")"
  done

  if [ "$found_any" = "false" ]; then
    echo "  (no active workflows)"
    echo ""
    echo "Run 'ralph plan <label>' to start a new feature."
  fi
}

#-----------------------------------------------------------------------------
# Flag parsing
#-----------------------------------------------------------------------------
WATCH_MODE=false
ALL_MODE=false
SPEC_FLAG=""

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
    --all|-a)
      ALL_MODE=true
      shift
      ;;
    --watch|-w)
      WATCH_MODE=true
      shift
      ;;
    -h|--help)
      echo "Usage: ralph status [OPTIONS]"
      echo ""
      echo "Show current workflow state."
      echo ""
      echo "Options:"
      echo "  --spec, -s <name>  Show status for named spec (default: current spec)"
      echo "  --all, -a          Summary of all active workflows"
      echo "  --watch, -w        Auto-refreshing live view (requires tmux)"
      echo "  -h, --help         Show this help message"
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      exit 1
      ;;
  esac
done

#-----------------------------------------------------------------------------
# Watch mode: tmux split layout
#-----------------------------------------------------------------------------
if [ "$WATCH_MODE" = "true" ]; then
  # Require tmux
  if [ -z "${TMUX:-}" ]; then
    echo "Error: --watch requires a tmux session." >&2
    echo "Start tmux first, then run 'ralph status --watch'." >&2
    exit 1
  fi

  # Find the most recent active log file (from ralph run)
  LOGS_DIR="$RALPH_DIR/logs"
  ACTIVE_LOG=""
  if [ -d "$LOGS_DIR" ]; then
    ACTIVE_LOG=$(find "$LOGS_DIR" -maxdepth 1 -name "work-*.log" -type f \
      -printf '%T@\t%p\n' 2>/dev/null \
      | sort -rn \
      | head -1 \
      | cut -f2) || true
  fi

  # Build bottom pane command
  if [ -n "$ACTIVE_LOG" ]; then
    # Active log found — tail it for live agent output
    BOTTOM_CMD="echo '=== Agent Output: $(basename "$ACTIVE_LOG") ===' && tail -f '$ACTIVE_LOG' | jq -r 'if .type == \"assistant\" then .message.content // .content // \"\" elif .type == \"result\" then \"--- result: \" + (.subtype // \"\") + \" ---\\n\" + (.result // \"\") else empty end' 2>/dev/null || tail -f '$ACTIVE_LOG'"
  else
    # No active log — show recent git log + last errors
    BOTTOM_CMD="echo '=== Recent Activity ===' && echo '' && git log --oneline -15 2>/dev/null || echo '(no git history)' && echo '' && echo '=== Last Errors ===' && ralph logs 2>/dev/null || echo '(no ralph logs found)'"
  fi

  # Create tmux split layout
  # Top pane: auto-refreshing ralph status
  # Bottom pane: agent output or recent activity
  tmux split-window -v -p 40 "$BOTTOM_CMD"
  tmux select-pane -t 0
  exec watch -n5 ralph status
fi

#-----------------------------------------------------------------------------
# Check if ralph is initialized
#-----------------------------------------------------------------------------
if [ ! -d "$RALPH_DIR" ]; then
  echo "Ralph not initialized. Run 'ralph plan <label>' first."
  exit 0
fi

#-----------------------------------------------------------------------------
# --all mode: summary of all active workflows
#-----------------------------------------------------------------------------
if [ "$ALL_MODE" = "true" ]; then
  show_all_status
  exit 0
fi

#-----------------------------------------------------------------------------
# Single workflow mode (default or --spec)
#-----------------------------------------------------------------------------

# Resolve the spec label using --spec flag or state/current
# For backwards compatibility: if resolve_spec_label fails (no state/current
# and no --spec flag), try falling back to the legacy current.json
LABEL=""
if [ -n "$SPEC_FLAG" ]; then
  # Explicit --spec flag: use resolve_spec_label (errors on missing state file)
  LABEL=$(resolve_spec_label "$SPEC_FLAG")
else
  # No --spec flag: try resolve_spec_label first, fall back to legacy current.json
  LABEL=$(resolve_spec_label "" 2>/dev/null) || true

  if [ -z "$LABEL" ]; then
    # Legacy fallback: try reading from current.json
    CURRENT_FILE="$RALPH_DIR/state/current.json"
    if [ -f "$CURRENT_FILE" ]; then
      LABEL=$(jq -r '.label // empty' "$CURRENT_FILE" 2>/dev/null || true)
    fi
  fi
fi

if [ -z "$LABEL" ]; then
  echo "Ralph Status"
  echo "============"
  echo ""
  echo "Label: (not set)"
  echo ""
  echo "Run 'ralph plan <label>' to start a new feature."
  exit 0
fi

# Show detailed status for the resolved label
show_single_status "$LABEL"
