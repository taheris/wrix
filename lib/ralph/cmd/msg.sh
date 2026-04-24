#!/usr/bin/env bash
set -euo pipefail

# ralph msg — Human interface for resolving ralph:clarify beads.
#
# Modes (see specs/ralph-review.md §Clarify resolution):
#   ralph msg                          List outstanding clarifies (host)
#   ralph msg -s <label>               List filtered to a single spec (host)
#   ralph msg -c                       Interactive Drafter session (container)
#   ralph msg -c -s <label>            Interactive, filtered to a single spec
#   ralph msg -n <N>                   View clarify #N (host)
#   ralph msg -i <id>                  View by bead ID (host)
#   ralph msg -n <N> -a <choice>       Fast-reply (int = option lookup; else verbatim)
#   ralph msg -i <id> -a <choice>      Fast-reply by bead ID (host)
#   ralph msg -n <N> -d                Dismiss (host)
#   ralph msg -i <id> -d               Dismiss by bead ID (host)

RALPH_DIR="${RALPH_DIR:-.wrapix/ralph}"

# shellcheck source=util.sh
source "$(dirname "$0")/util.sh"

# Parse flags
SPEC_FILTER=""
BEAD_ID=""
NUM_TARGET=""
DISMISS=false
CHAT=false
ANSWER=""
ANSWER_SET=false

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
    -n|--num)
      if [ -z "${2:-}" ]; then
        error "Flag $1 requires an integer index argument"
      fi
      NUM_TARGET="$2"
      shift 2
      ;;
    --num=*)
      NUM_TARGET="${1#--num=}"
      shift
      ;;
    -a|--answer)
      if [ $# -lt 2 ]; then
        error "Flag $1 requires a choice argument"
      fi
      ANSWER="$2"
      ANSWER_SET=true
      shift 2
      ;;
    --answer=*)
      ANSWER="${1#--answer=}"
      ANSWER_SET=true
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
      cat <<'HELP'
Usage: ralph msg [flags]

Human interface for resolving ralph:clarify beads.

Modes:
  ralph msg                         List outstanding clarifies (host)
  ralph msg -s <label>              List filtered to a single spec (host)
  ralph msg -c                      Interactive Drafter session (container)
  ralph msg -c -s <label>           Interactive, filtered to a single spec
  ralph msg -n <N>                  View clarify #N (host)
  ralph msg -i <id>                 View by bead ID (host)
  ralph msg -n <N> -a <choice>      Fast-reply: int = option lookup, else verbatim
  ralph msg -i <id> -a <choice>     Fast-reply by bead ID (host)
  ralph msg -n <N> -d               Dismiss (host)
  ralph msg -i <id> -d              Dismiss by bead ID (host)

Flags:
  -c, --chat               Launch interactive Drafter (container, Claude)
  -s, --spec <label>       Filter by spec label (default: state/current)
  -n, --num <N>            Target clarify by 1-based sequential index
  -i, --id <id>            Target clarify by bead ID
  -a, --answer <choice>    Store answer and clear label (int = option lookup)
  -d, --dismiss            Dismiss without answering
  -h, --help               Show this help
HELP
      exit 0
      ;;
    -*)
      error "Unknown flag: $1 (see ralph msg --help)"
      ;;
    *)
      error "Unexpected argument: $1 (use -a <choice> to supply an answer; see ralph msg --help)"
      ;;
  esac
done

if [ -n "$BEAD_ID" ] && [ -n "$NUM_TARGET" ]; then
  error "Use either -n <N> or -i <id>, not both"
fi

if [ "$ANSWER_SET" = "true" ] && [ "$DISMISS" = "true" ]; then
  error "Use either -a <choice> or -d, not both"
fi

if [ "$ANSWER_SET" = "true" ] && [ -z "$BEAD_ID" ] && [ -z "$NUM_TARGET" ]; then
  error "-a requires -n <N> or -i <id>"
fi

if [ "$DISMISS" = "true" ] && [ -z "$BEAD_ID" ] && [ -z "$NUM_TARGET" ]; then
  error "-d requires -n <N> or -i <id>"
fi

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
  echo "  ralph msg -n <N>              # view clarify #N"
  echo "  ralph msg -n <N> -a <int>     # fast-reply: pick option <int>"
  echo "  ralph msg -n <N> -a \"text\"    # fast-reply: verbatim answer"
  echo "  ralph msg -n <N> -d           # dismiss"
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
# Resolve the target bead ID from -n <N> (1-based index into the sorted
# clarify list for the resolved spec filter) or -i <id>. Sets TARGET_ID and
# TARGET_INDEX. TARGET_INDEX is the 1-based sequential position if known,
# else empty.
#-----------------------------------------------------------------------------
resolve_target_bead() {
  local num="$1"
  local id="$2"
  local spec="$3"

  TARGET_ID=""
  TARGET_INDEX=""

  local json
  json=$(get_sorted_clarify_beads "$spec")
  local count
  count=$(echo "$json" | jq 'length' 2>/dev/null || echo 0)

  if [ -n "$num" ]; then
    if ! [[ "$num" =~ ^[0-9]+$ ]]; then
      error "Invalid index '$num' for -n (expected a positive integer)"
    fi
    if [ "$num" -lt 1 ] || [ "$num" -gt "$count" ]; then
      error "No clarify at index $num ($count outstanding)"
    fi
    TARGET_ID=$(echo "$json" | jq -r --argjson idx "$((num - 1))" '.[$idx].id')
    TARGET_INDEX="$num"
    return 0
  fi

  if [ -n "$id" ]; then
    TARGET_ID="$id"
    local pos
    pos=$(echo "$json" | jq -r --arg id "$id" \
      'to_entries | map(select(.value.id == $id)) | first | (.key + 1) // empty')
    TARGET_INDEX="$pos"
    return 0
  fi
}

#-----------------------------------------------------------------------------
# Render view output for a single clarify: header, summary, body (description
# preamble), enumerated options, reply hints. Host-side, no container.
# Usage: render_view <bead_id> [index]
#-----------------------------------------------------------------------------
render_view() {
  local bead_id="$1"
  local index="${2:-}"

  local bead_json description title summary
  bead_json=$(bd_json show "$bead_id" --json)
  description=$(echo "$bead_json" | jq -r '.[0].description // ""')
  title=$(echo "$bead_json" | jq -r '.[0].title // ""')
  summary=$(get_summary_for_bead "$description" "$title")

  if [ -n "$index" ]; then
    echo "Clarify #${index} — ${bead_id}"
  else
    echo "Clarify — ${bead_id}"
  fi
  echo "Summary: ${summary}"
  echo ""

  local body
  body=$(printf '%s\n' "$description" \
    | awk 'BEGIN{p=1} /^##[[:space:]]+Options([[:space:]]|$)/{p=0} p' \
    | awk 'BEGIN{seen=0; buf=""} { if (NF) { if (buf != "") buf = buf "\n"; buf = buf $0; seen=1 } else if (seen) { buf = buf "\n" } } END{ sub(/\n+$/, "", buf); if (buf != "") print buf }')
  if [ -n "$body" ]; then
    echo "$body"
    echo ""
  fi

  local parsed options_count hdr_summary
  parsed=$(parse_options_format "$description")
  options_count=$(echo "$parsed" | jq -r '.options | length')

  if [ "$options_count" -gt 0 ]; then
    hdr_summary=$(echo "$parsed" | jq -r '.summary // ""')
    if [ -n "$hdr_summary" ]; then
      echo "## Options — ${hdr_summary}"
    else
      echo "## Options"
    fi
    echo ""

    local opt
    while IFS= read -r opt; do
      local n otitle obody
      n=$(echo "$opt" | jq -r '.n')
      otitle=$(echo "$opt" | jq -r '.title // ""')
      obody=$(echo "$opt" | jq -r '.body // ""')
      if [ -n "$otitle" ]; then
        echo "[${n}] ${otitle}"
      else
        echo "[${n}]"
      fi
      if [ -n "$obody" ]; then
        echo "$obody" | sed 's/^/    /'
      fi
      echo ""
    done < <(echo "$parsed" | jq -c '.options[]')
  fi

  local target_ref
  if [ -n "$index" ]; then
    target_ref="-n ${index}"
  else
    target_ref="-i ${bead_id}"
  fi
  echo "Reply:"
  echo "  ralph msg ${target_ref} -a <int>     # pick an option"
  echo "  ralph msg ${target_ref} -a \"text\"    # custom answer"
  echo "  ralph msg ${target_ref} -d           # dismiss"
}

#-----------------------------------------------------------------------------
# Fast-reply: store an answer and clear ralph:clarify. Integer <choice>
# triggers Options Format lookup; anything else is stored verbatim.
# Usage: do_fast_reply <bead_id> <choice>
#-----------------------------------------------------------------------------
do_fast_reply() {
  local bead_id="$1"
  local choice="$2"
  local note

  if [[ "$choice" =~ ^[0-9]+$ ]]; then
    local parsed matched
    parsed=$(parse_options_format_from_bead "$bead_id")
    matched=$(echo "$parsed" | jq -c --argjson n "$choice" \
      '.options[] | select(.n == $n)' 2>/dev/null || true)
    if [ -z "$matched" ] || [ "$matched" = "null" ]; then
      local available
      available=$(echo "$parsed" | jq -r '.options | map(.n) | join(", ")')
      [ -z "$available" ] && available="(none)"
      echo "Option ${choice} not found in ${bead_id}. Available options: ${available}" >&2
      echo "Use -a \"text\" for a free-form answer." >&2
      exit 1
    fi
    local otitle obody
    otitle=$(echo "$matched" | jq -r '.title // ""')
    obody=$(echo "$matched" | jq -r '.body // ""')
    note="Chose option ${choice} — ${otitle}: ${obody}"
  else
    note="$choice"
  fi

  bd update "$bead_id" --append-notes "$note" || error "Failed to store answer for $bead_id"
  remove_clarify_label "$bead_id"
  reset_iteration_for_bead "$bead_id"
  print_resume_hint "$bead_id"
}

#-----------------------------------------------------------------------------
# Dismiss: remove ralph:clarify with a work-around note. Host-side.
# Usage: do_dismiss <bead_id>
#-----------------------------------------------------------------------------
do_dismiss() {
  local bead_id="$1"
  bd update "$bead_id" --append-notes "Dismissed: Agent should work around this question." \
    || error "Failed to store dismissal for $bead_id"
  remove_clarify_label "$bead_id"
  reset_iteration_for_bead "$bead_id"
  print_resume_hint "$bead_id"
}

#-----------------------------------------------------------------------------
# Mode: View, fast-reply, or dismiss a single clarify (host-side).
#-----------------------------------------------------------------------------
if { [ -n "$BEAD_ID" ] || [ -n "$NUM_TARGET" ]; } && [ "$CHAT" = "false" ]; then
  # Resolve spec filter for index lookup. Same rules as list mode: explicit
  # --spec, else state/current, else unfiltered.
  resolved_filter=""
  if [ -n "$SPEC_FILTER" ]; then
    resolved_filter="$SPEC_FILTER"
  else
    current_file="$RALPH_DIR/state/current"
    if [ -f "$current_file" ]; then
      resolved_filter=$(<"$current_file")
      resolved_filter="${resolved_filter#"${resolved_filter%%[![:space:]]*}"}"
      resolved_filter="${resolved_filter%"${resolved_filter##*[![:space:]]}"}"
    fi
  fi

  resolve_target_bead "$NUM_TARGET" "$BEAD_ID" "$resolved_filter"

  if [ "$ANSWER_SET" = "true" ]; then
    do_fast_reply "$TARGET_ID" "$ANSWER"
    exit 0
  fi

  if [ "$DISMISS" = "true" ]; then
    do_dismiss "$TARGET_ID"
    exit 0
  fi

  render_view "$TARGET_ID" "$TARGET_INDEX"
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
  if [ ! -f "${WRAPIX_CLAUDE_CONFIG:-/etc/wrapix/claude-config.json}" ] && command -v wrapix &>/dev/null; then
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
    reset_iteration_for_bead "$cleared"
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
