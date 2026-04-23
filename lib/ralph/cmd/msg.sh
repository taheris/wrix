#!/usr/bin/env bash
set -euo pipefail

# ralph msg — Human interface for resolving ralph:clarify beads.
#
# Modes (see specs/ralph-review.md §Clarify resolution):
#   ralph msg                          List outstanding clarifies (host)
#   ralph msg -s <label>               List filtered to a single spec (host)
#   ralph msg -c                       Interactive Drafter session (container)
#   ralph msg -c -s <label>            Interactive, filtered to a single spec
#   ralph msg -i <id>                  View by bead ID (host)
#   ralph msg -i <id> "answer"         Fast-reply, verbatim answer (host)
#   ralph msg -i <id> -d               Dismiss (host)

RALPH_DIR="${RALPH_DIR:-.wrapix/ralph}"

# shellcheck source=util.sh
source "$(dirname "$0")/util.sh"

# Parse flags
SPEC_FILTER=""
BEAD_ID=""
DISMISS=false
CHAT=false
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
    -c|--chat)
      CHAT=true
      shift
      ;;
    -d|--dismiss)
      DISMISS=true
      shift
      ;;
    -h|--help)
      echo "Usage: ralph msg [flags] [answer]"
      echo ""
      echo "Human interface for resolving ralph:clarify beads."
      echo ""
      echo "Modes:"
      echo "  ralph msg                    List outstanding clarifies (host)"
      echo "  ralph msg -s <label>         List filtered to a single spec (host)"
      echo "  ralph msg -c                 Interactive Drafter session (container)"
      echo "  ralph msg -c -s <label>      Interactive, filtered to a single spec"
      echo "  ralph msg -i <id>            View specific clarify (host)"
      echo "  ralph msg -i <id> \"answer\"   Reply to a clarify (host)"
      echo "  ralph msg -i <id> -d         Dismiss without answering (host)"
      echo ""
      echo "Flags:"
      echo "  -c, --chat           Launch interactive Drafter (container, Claude)"
      echo "  -s, --spec <label>   Filter by spec label (default: state/current)"
      echo "  -i, --id <id>        Target specific clarify by bead ID"
      echo "  -d, --dismiss        Dismiss without answering"
      echo "  -h, --help           Show this help"
      exit 0
      ;;
    -*)
      error "Unknown flag: $1 (see ralph msg --help)"
      ;;
    *)
      ANSWER="$1"
      shift
      ;;
  esac
done

#-----------------------------------------------------------------------------
# Determine source label from bead labels
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
# Extract spec label from bead labels (spec:<label>)
#-----------------------------------------------------------------------------
get_spec_from_labels() {
  local labels_json="$1"
  # best-effort: malformed labels -> em-dash placeholder, caller treats as "no spec"
  echo "$labels_json" | { jq -r '[.[] | select(startswith("spec:"))] | first // "—"' || echo "—"; } | sed 's/^spec://'
}

#-----------------------------------------------------------------------------
# Reset the run ↔ check iteration counter for the bead's spec (if any).
#-----------------------------------------------------------------------------
reset_iteration_for_bead() {
  local bead_id="$1"

  local labels_json
  # best-effort: bd unavailable -> empty labels, caller treats as "no spec"
  labels_json=$(bd_json show "$bead_id" --json | jq -c '.[0].labels // []' || echo '[]')

  local bead_spec
  bead_spec=$(get_spec_from_labels "$labels_json")

  if [ -z "$bead_spec" ] || [ "$bead_spec" = "—" ]; then
    return 0
  fi

  reset_iteration_count "$RALPH_DIR/state/${bead_spec}.json"
}

#-----------------------------------------------------------------------------
# Print one-line resume hint after clearing a clarify.
#-----------------------------------------------------------------------------
print_resume_hint() {
  local bead_id="$1"

  local labels_json
  # best-effort: bd unavailable -> empty labels, caller treats as "no spec"
  labels_json=$(bd_json show "$bead_id" --json | jq -c '.[0].labels // []' || echo '[]')

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
# Fetch clarify beads for the resolved spec, sorted by creation time ascending.
# Usage: get_sorted_clarify_beads [spec_filter]
# Output: JSON array
#-----------------------------------------------------------------------------
get_sorted_clarify_beads() {
  local spec_filter="${1:-}"
  local json
  json=$(list_clarify_beads "$spec_filter")
  echo "$json" | jq 'sort_by(.created_at // .created // "")' 2>/dev/null || echo "$json"
}

#-----------------------------------------------------------------------------
# Derive SUMMARY for list display. Uses the Options Format Contract summary
# when present; falls back to bead title.
# Usage: get_summary_for_bead <description> <title>
#-----------------------------------------------------------------------------
get_summary_for_bead() {
  local description="$1"
  local title="${2:-}"

  local summary=""
  # parse_options_format is defined in util.sh
  summary=$(parse_options_format "$description" | jq -r '.summary // ""' 2>/dev/null || echo "")

  if [ -n "$summary" ]; then
    echo "$summary"
  else
    echo "$title"
  fi
}

#-----------------------------------------------------------------------------
# Render the outstanding-clarify list table.
# Usage: render_clarify_list <questions_json> <spec_label>
#-----------------------------------------------------------------------------
render_clarify_list() {
  local questions_json="$1"
  local spec_label="${2:-}"

  local count
  count=$(echo "$questions_json" | jq 'length' 2>/dev/null || echo 0)

  if [ -n "$spec_label" ]; then
    echo "Outstanding clarifies for ${spec_label} (${count}):"
  else
    echo "Outstanding clarifies (${count}):"
  fi
  echo ""

  printf " %2s  %-14s  %s\n" "#" "ID" "SUMMARY"
  printf " %2s  %-14s  %s\n" "--" "--------------" "----------------------------------------"

  local idx=0
  local line
  while IFS= read -r line; do
    idx=$((idx + 1))
    local bead_id description title summary
    bead_id=$(echo "$line" | jq -r '.id // "—"')
    description=$(echo "$line" | jq -r '.description // ""')
    title=$(echo "$line" | jq -r '.title // ""')
    summary=$(get_summary_for_bead "$description" "$title")

    if [ ${#summary} -gt 60 ]; then
      summary="${summary:0:57}..."
    fi

    printf " %2d  %-14s  %s\n" "$idx" "$bead_id" "$summary"
  done < <(echo "$questions_json" | jq -c '.[]')

  echo ""
  echo "Reply:"
  echo "  ralph msg -c                  # interactive triage + walk (container, Claude)"
  echo "  ralph msg -i <id>             # view a clarify"
  echo "  ralph msg -i <id> \"answer\"    # fast-reply verbatim"
  echo "  ralph msg -i <id> -d          # dismiss"
}

#-----------------------------------------------------------------------------
# Build the CLARIFY_BEADS markdown block for the msg.md template.
# One entry per bead: ID, title, description excerpt, ## Options header, and
# enumerated options. The LLM uses this to present the triage summary and walk.
# Usage: build_clarify_beads_block <questions_json>
#-----------------------------------------------------------------------------
build_clarify_beads_block() {
  local questions_json="$1"
  local block=""

  local line
  while IFS= read -r line; do
    local bead_id title description parsed options_count summary
    bead_id=$(echo "$line" | jq -r '.id // "—"')
    title=$(echo "$line" | jq -r '.title // ""')
    description=$(echo "$line" | jq -r '.description // ""')

    parsed=$(parse_options_format "$description")
    summary=$(echo "$parsed" | jq -r '.summary // ""')
    options_count=$(echo "$parsed" | jq -r '.options | length')

    block="${block}### ${bead_id} — ${title}"$'\n\n'

    if [ -n "$summary" ]; then
      block="${block}## Options — ${summary}"$'\n\n'
    elif [ "$options_count" -gt 0 ]; then
      block="${block}## Options"$'\n\n'
    fi

    if [ "$options_count" -gt 0 ]; then
      local opts_json
      opts_json=$(echo "$parsed" | jq -c '.options[]')
      local opt
      while IFS= read -r opt; do
        local n otitle obody
        n=$(echo "$opt" | jq -r '.n')
        otitle=$(echo "$opt" | jq -r '.title // ""')
        obody=$(echo "$opt" | jq -r '.body // ""')
        if [ -n "$otitle" ]; then
          block="${block}#### Option ${n} — ${otitle}"$'\n\n'
        else
          block="${block}#### Option ${n}"$'\n\n'
        fi
        if [ -n "$obody" ]; then
          block="${block}${obody}"$'\n\n'
        fi
      done <<< "$opts_json"
    else
      # No options parsed — include the raw description so the LLM can read it.
      if [ -n "$description" ]; then
        block="${block}${description}"$'\n\n'
      fi
    fi

    block="${block}"$'\n'
  done < <(echo "$questions_json" | jq -c '.[]')

  printf '%s' "$block"
}

#-----------------------------------------------------------------------------
# Mode: Show specific question by bead ID
#-----------------------------------------------------------------------------
if [ -n "$BEAD_ID" ] && [ -z "$ANSWER" ] && [ "$DISMISS" = "false" ] && [ "$CHAT" = "false" ]; then
  bd show "$BEAD_ID"
  exit 0
fi

#-----------------------------------------------------------------------------
# Mode: Reply to a question (positional answer)
#-----------------------------------------------------------------------------
if [ -n "$BEAD_ID" ] && [ -n "$ANSWER" ]; then
  debug "Replying to $BEAD_ID with answer"

  bd update "$BEAD_ID" --append-notes "Answer: $ANSWER" || error "Failed to store answer for $BEAD_ID"

  remove_clarify_label "$BEAD_ID"
  reset_iteration_for_bead "$BEAD_ID"

  print_resume_hint "$BEAD_ID"
  exit 0
fi

#-----------------------------------------------------------------------------
# Mode: Dismiss a question
#-----------------------------------------------------------------------------
if [ -n "$BEAD_ID" ] && [ "$DISMISS" = "true" ]; then
  debug "Dismissing $BEAD_ID"

  bd update "$BEAD_ID" --append-notes "Dismissed: Agent should work around this question." || error "Failed to store dismissal for $BEAD_ID"

  remove_clarify_label "$BEAD_ID"
  reset_iteration_for_bead "$BEAD_ID"

  echo "Dismissed $BEAD_ID. The agent will proceed without an answer on its next iteration."
  exit 0
fi

#-----------------------------------------------------------------------------
# Mode: Interactive Drafter session (-c / --chat)
#
# Split across two execution contexts (same pattern as ralph check):
#   - Host side (wrapix available, outside container): resolve label, install
#     re-pin hook, launch wrapix with base profile, bd dolt pull after exit
#   - Container side (inside wrapix or dev without wrapix): render msg.md with
#     CLARIFY_BEADS, run Claude, bd dolt push after RALPH_COMPLETE
#-----------------------------------------------------------------------------
if [ "$CHAT" = "true" ]; then
  # HOST SIDE: re-launch in container
  if [ ! -f /etc/wrapix/claude-config.json ] && command -v wrapix &>/dev/null; then
    host_label=$(resolve_spec_label "$SPEC_FILTER")

    echo "Ralph msg: interactive Drafter session for '$host_label'"
    echo ""

    # Pre-check: if no outstanding clarifies, don't bother launching the container.
    host_questions=$(get_sorted_clarify_beads "$host_label")
    host_count=$(echo "$host_questions" | jq 'length' 2>/dev/null || echo 0)
    if [ "$host_count" -eq 0 ]; then
      echo "No outstanding clarifies for '$host_label'."
      exit 0
    fi

    render_clarify_list "$host_questions" "$host_label"
    echo ""

    export RALPH_MODE=1
    export RALPH_CMD=msg
    export RALPH_ARGS="-c -s $host_label"

    # Compaction re-pin hook: label/spec/molecule for orientation
    host_state_file="$RALPH_DIR/state/${host_label}.json"
    host_spec="specs/${host_label}.md"
    [ -f "$RALPH_DIR/state/${host_label}.md" ] && host_spec="$RALPH_DIR/state/${host_label}.md"
    host_molecule=""
    if [ -f "$host_state_file" ]; then
      host_molecule=$(jq -r '.molecule // empty' "$host_state_file" 2>/dev/null || true)
    fi
    repin_content=$(build_repin_content "$host_label" msg \
      "spec=$host_spec" \
      "mode=interactive" \
      "molecule=$host_molecule")
    install_repin_hook "$host_label" "$repin_content"
    # shellcheck disable=SC2064
    trap "rm -rf '$RALPH_DIR/runtime/$host_label'" EXIT

    wrapix
    wrapix_exit=$?

    echo "Syncing beads from container..."
    bd dolt pull || echo "Warning: bd dolt pull failed (beads may not be synced)"

    exit $wrapix_exit
  fi

  # CONTAINER SIDE: render msg.md, run Claude, push beads to Dolt
  label=$(resolve_spec_label "$SPEC_FILTER")

  questions_json=$(get_sorted_clarify_beads "$label")
  question_count=$(echo "$questions_json" | jq 'length' 2>/dev/null || echo 0)

  if [ "$question_count" -eq 0 ]; then
    echo "No outstanding clarifies for '$label'."
    exit 0
  fi

  state_file="$RALPH_DIR/state/${label}.json"
  spec_path="specs/${label}.md"
  if [ -f "$state_file" ]; then
    maybe_spec=$(jq -r '.spec_path // empty' "$state_file" 2>/dev/null || true)
    [ -n "$maybe_spec" ] && spec_path="$maybe_spec"
  fi

  clarify_beads=$(build_clarify_beads_block "$questions_json")

  companions=""
  if [ -f "$state_file" ]; then
    companions=$(read_manifests "$state_file")
  fi

  pinned_context_file=$(get_pinned_context_file)
  pinned_context=""
  if [ -f "$pinned_context_file" ]; then
    pinned_context=$(cat "$pinned_context_file")
  fi

  config_file="$RALPH_DIR/config.nix"
  config="{}"
  if [ -f "$config_file" ]; then
    config=$(nix eval --json --file "$config_file" 2>/dev/null || echo "{}")
  fi

  model_msg=$(resolve_model "msg" "$config")

  msg_prompt=$(render_template msg \
    "SPEC_PATH=$spec_path" \
    "LABEL=$label" \
    "CLARIFY_BEADS=$clarify_beads" \
    "COMPANIONS=$companions" \
    "PINNED_CONTEXT=$pinned_context" \
    "EXIT_SIGNALS=")

  mkdir -p "$RALPH_DIR/logs"
  log="$RALPH_DIR/logs/msg-${label}.log"

  echo "=== Interactive Drafter session ==="
  echo ""

  run_claude_stream "$msg_prompt" "$log" "$config" "$model_msg"

  # Container bead sync: push so host can pull any note/label changes.
  echo ""
  echo "Pushing beads to Dolt remote..."
  # best-effort: nothing to commit -> dolt returns non-zero; push below still runs
  bd dolt commit >/dev/null || true
  if ! bd dolt push 2>&1; then
    echo "Warning: bd dolt push failed — beads may not reach host"
  fi

  # Emit resume hints for every bead whose ralph:clarify label was cleared in
  # this session so the user sees the exact shell command on their way out.
  remaining=$(list_clarify_beads "$label" | jq -c '[.[].id]')
  before_ids=$(echo "$questions_json" | jq -c '[.[].id]')
  cleared_ids=$(jq -cn --argjson a "$before_ids" --argjson b "$remaining" '$a - $b')
  echo "$cleared_ids" | jq -r '.[]' | while IFS= read -r cleared; do
    [ -z "$cleared" ] && continue
    print_resume_hint "$cleared"
  done

  exit 0
fi

#-----------------------------------------------------------------------------
# Mode: List outstanding questions (default, host-side)
#-----------------------------------------------------------------------------

# Resolve spec label: --spec/-s if given, else state/current. Unlike the
# container modes we tolerate a missing state/current here so `ralph msg`
# in a fresh checkout prints a helpful "No outstanding questions" instead
# of erroring out.
resolved_label=""
if [ -n "$SPEC_FILTER" ]; then
  resolved_label="$SPEC_FILTER"
else
  current_file="$RALPH_DIR/state/current"
  if [ -f "$current_file" ]; then
    resolved_label=$(<"$current_file")
    resolved_label="${resolved_label#"${resolved_label%%[![:space:]]*}"}"
    resolved_label="${resolved_label%"${resolved_label##*[![:space:]]}"}"
  fi
fi

QUESTIONS_JSON=$(get_sorted_clarify_beads "$resolved_label") || {
  echo "No outstanding questions."
  exit 0
}

QUESTION_COUNT=$(echo "$QUESTIONS_JSON" | jq 'length' 2>/dev/null || echo 0)

if [ "$QUESTION_COUNT" -eq 0 ]; then
  if [ -n "$resolved_label" ]; then
    echo "No outstanding clarifies for '$resolved_label'."
  else
    echo "No outstanding questions."
  fi
  exit 0
fi

render_clarify_list "$QUESTIONS_JSON" "$resolved_label"
