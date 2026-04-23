#!/usr/bin/env bash
set -euo pipefail

# ralph todo [--spec <name>|-s <name>] [--since <commit>]
# Converts spec to beads with task breakdown using four-tier detection:
#   Tier 1 (diff):   base_commit in state JSON → git diff (fast, precise)
#   Tier 2 (tasks):  no base_commit but molecule exists → LLM compares spec vs tasks
#   Tier 3 (README): no state file → discover molecule from README, reconstruct state, proceed as tier 2
#   Tier 4 (new):    no state file AND no molecule in README → full spec decomposition
#
# - Accepts --spec/-s flag to target a specific workflow
# - Accepts --since flag to force tier 1 from a specific commit
# - Errors on uncommitted spec changes (git-tracked specs only)
# - Stores HEAD as base_commit only on RALPH_COMPLETE
#
# SH-6 convention: jq/bd fallbacks below (e.g. `jq -r ... 2>/dev/null || echo 0`)
# are best-effort display/count lookups — failures fall back to safe defaults
# (empty string, "0", "HEAD~1") so the host-side verification and repin hook
# can still run.

# Parse --spec/-s and --since flags early (before container detection)
SPEC_FLAG=""
SINCE_FLAG=""
TODO_ARGS=()

for arg in "$@"; do
  if [ "${_next_is_spec:-}" = "1" ]; then
    SPEC_FLAG="$arg"
    unset _next_is_spec
    continue
  fi
  if [ "${_next_is_since:-}" = "1" ]; then
    SINCE_FLAG="$arg"
    unset _next_is_since
    continue
  fi
  case "$arg" in
    --spec|-s)
      _next_is_spec=1
      ;;
    --spec=*)
      SPEC_FLAG="${arg#--spec=}"
      ;;
    --since)
      _next_is_since=1
      ;;
    --since=*)
      SINCE_FLAG="${arg#--since=}"
      ;;
    *)
      TODO_ARGS+=("$arg")
      ;;
  esac
done
unset _next_is_spec _next_is_since

# Replace positional params with filtered args
set -- "${TODO_ARGS[@]+"${TODO_ARGS[@]}"}"

# Container detection: if not in container and wrapix is available, re-launch in container
# /etc/wrapix/claude-config.json only exists inside containers (baked into image)
if [ ! -f /etc/wrapix/claude-config.json ] && command -v wrapix &>/dev/null; then
  export RALPH_MODE=1
  export RALPH_CMD=todo
  # Preserve flags in args for container re-exec
  RALPH_ARGS=""
  if [ -n "$SPEC_FLAG" ]; then
    RALPH_ARGS="--spec $SPEC_FLAG"
  fi
  if [ -n "$SINCE_FLAG" ]; then
    RALPH_ARGS="$RALPH_ARGS --since $SINCE_FLAG"
  fi
  RALPH_ARGS="$RALPH_ARGS ${*:-}"
  export RALPH_ARGS

  # Resolve label for host-side verification
  _HOST_RALPH_DIR="${RALPH_DIR:-.wrapix/ralph}"
  _HOST_LABEL="$SPEC_FLAG"
  if [ -z "$_HOST_LABEL" ] && [ -f "$_HOST_RALPH_DIR/state/current" ]; then
    _HOST_LABEL=$(<"$_HOST_RALPH_DIR/state/current")
    _HOST_LABEL="${_HOST_LABEL#"${_HOST_LABEL%%[![:space:]]*}"}"
    _HOST_LABEL="${_HOST_LABEL%"${_HOST_LABEL##*[![:space:]]}"}"
  fi
  _HOST_STATE_FILE="$_HOST_RALPH_DIR/state/${_HOST_LABEL}.json"
  # Count beads by label (works reliably after dolt pull, unlike --children)
  _HOST_PRE_COUNT=$(bd list -l "spec:${_HOST_LABEL}" --json 2>/dev/null \
    | jq 'length' 2>/dev/null || echo 0)

  # Compaction re-pin hook: label/spec/molecule/companions/exit-signals
  if [ -n "$_HOST_LABEL" ]; then
    # shellcheck source=util.sh
    source "$(dirname "$0")/util.sh"

    _host_spec="specs/${_HOST_LABEL}.md"
    [ -f "$_HOST_RALPH_DIR/state/${_HOST_LABEL}.md" ] && _host_spec="$_HOST_RALPH_DIR/state/${_HOST_LABEL}.md"
    _host_molecule=""
    _host_companions=""
    if [ -f "$_HOST_STATE_FILE" ]; then
      _host_molecule=$(jq -r '.molecule // empty' "$_HOST_STATE_FILE" 2>/dev/null || true)
      _host_companions=$(jq -r '(.companions // []) | join(",")' "$_HOST_STATE_FILE" 2>/dev/null || true)
    fi
    _repin_content=$(build_repin_content "$_HOST_LABEL" todo \
      "spec=$_host_spec" \
      "molecule=$_host_molecule" \
      "companions=$_host_companions")
    install_repin_hook "$_HOST_LABEL" "$_repin_content"
    # shellcheck disable=SC2064
    trap "rm -rf '$_HOST_RALPH_DIR/runtime/$_HOST_LABEL'" EXIT
  fi

  wrapix
  wrapix_exit=$?

  if [ $wrapix_exit -eq 0 ]; then
    # Pull beads synced by container's bd dolt push
    echo "Syncing beads from container..."
    bd dolt pull || echo "Warning: bd dolt pull failed (beads may not be synced)"

    # Host-side verification: did task count increase?
    _HOST_POST_COUNT=$(bd list -l "spec:${_HOST_LABEL}" --json 2>/dev/null \
      | jq 'length' 2>/dev/null || echo 0)

    if [ "$_HOST_POST_COUNT" -le "$_HOST_PRE_COUNT" ]; then
      _PREV_BASE_COMMIT=$(jq -r '.base_commit // "HEAD~1"' "$_HOST_STATE_FILE" 2>/dev/null || echo "HEAD~1")
      echo "" >&2
      echo "Warning: RALPH_COMPLETE but no new tasks detected after sync." >&2
      echo "  If the container dolt push failed above, tasks may not have synced." >&2
      echo "  This may be expected under anchor-driven multi-spec planning" >&2
      echo "  when only sibling specs received tasks." >&2
      echo "  Check: bd list -l spec:${_HOST_LABEL}" >&2
      echo "  To re-run: ralph todo --since ${_PREV_BASE_COMMIT}" >&2
    fi
  fi
  exit $wrapix_exit
fi

# Load shared helpers
# shellcheck source=util.sh
source "$(dirname "$0")/util.sh"

# Warn early if scripts or templates are stale
check_ralph_staleness

RALPH_DIR="${RALPH_DIR:-.wrapix/ralph}"

CONFIG_FILE="$RALPH_DIR/config.nix"
SPECS_DIR="specs"

if [ ! -f "$CONFIG_FILE" ]; then
  echo "Error: No ralph config found at $CONFIG_FILE"
  echo "Run 'ralph plan <label>' first."
  exit 1
fi

# Resolve the spec label using --spec flag or state/current
LABEL=$(resolve_spec_label "$SPEC_FLAG")

# Read state from per-label state file: state/<label>.json
STATE_FILE="$RALPH_DIR/state/${LABEL}.json"

# Derive hidden from spec_path (no .hidden field in state JSON)
if spec_is_hidden "$STATE_FILE"; then
  SPEC_HIDDEN="true"
else
  SPEC_HIDDEN="false"
fi

# Load config for stream filter
CONFIG=$(nix eval --json --file "$CONFIG_FILE")

# Compute spec path and README instructions based on hidden flag
if [ "$SPEC_HIDDEN" = "true" ]; then
  SPEC_PATH="$RALPH_DIR/state/$LABEL.md"
  README_INSTRUCTIONS=""
else
  SPEC_PATH="$SPECS_DIR/$LABEL.md"
  PINNED_CONTEXT_FILE=$(get_pinned_context_file)
  README_INSTRUCTIONS="## Spec Index Update (required for cross-machine state recovery)

After creating the molecule, update \`$PINNED_CONTEXT_FILE\`:
- Find the row for this spec
- Update the Beads column with the molecule ID (epic ID)"
fi

# Check spec file exists
if [ ! -f "$SPEC_PATH" ]; then
  echo "Error: Spec file not found: $SPEC_PATH"
  echo "Run 'ralph plan' first to create the specification."
  exit 1
fi

# Check for uncommitted spec changes (git-tracked specs only).
# Guard covers the anchor plus every sibling in the tier 1 candidate set
# (spec req 21: anchor-driven multi-spec planning).
check_uncommitted_spec() {
  local path="$1"
  if ! git ls-files --error-unmatch "$path" >/dev/null 2>&1; then
    return 0
  fi
  if ! git diff --quiet -- "$path" || ! git diff --cached --quiet -- "$path"; then
    echo "Error: Uncommitted changes detected in $path"
    echo ""
    echo "Spec changes must be committed before running ralph todo."
    echo "  git add $path && git commit -m 'Update spec'"
    exit 1
  fi
}

if [ "$SPEC_HIDDEN" = "false" ]; then
  check_uncommitted_spec "$SPEC_PATH"

  # Widen to tier 1 candidate set: every spec changed since anchor's cursor
  # (override with --since applies to the anchor only — siblings keep their
  # own state/<s>.base_commit).
  ANCHOR_BASE_FOR_GUARD=""
  if [ -f "$STATE_FILE" ]; then
    ANCHOR_BASE_FOR_GUARD=$(jq -r '.base_commit // ""' "$STATE_FILE")
  fi
  if [ -n "$SINCE_FLAG" ] \
    && git rev-parse --verify "${SINCE_FLAG}^{commit}" >/dev/null 2>&1; then
    ANCHOR_BASE_FOR_GUARD="$SINCE_FLAG"
  fi
  if [ -n "$ANCHOR_BASE_FOR_GUARD" ] \
    && git rev-parse --verify "${ANCHOR_BASE_FOR_GUARD}^{commit}" >/dev/null 2>&1 \
    && git merge-base --is-ancestor "$ANCHOR_BASE_FOR_GUARD" HEAD >/dev/null 2>&1; then
    while IFS= read -r _cand_path; do
      [ -z "$_cand_path" ] && continue
      [ "$_cand_path" = "$SPEC_PATH" ] && continue
      check_uncommitted_spec "$_cand_path"
    done < <(git diff "$ANCHOR_BASE_FOR_GUARD" HEAD --name-only -- specs/)
    unset _cand_path
  fi
  unset ANCHOR_BASE_FOR_GUARD
fi

# Use compute_spec_diff for four-tier detection
DIFF_ARGS=()
if [ -n "$SINCE_FLAG" ]; then
  DIFF_ARGS+=(--since "$SINCE_FLAG")
fi

DIFF_OUTPUT=$(compute_spec_diff "$STATE_FILE" "${DIFF_ARGS[@]+"${DIFF_ARGS[@]}"}")
DIFF_MODE=$(echo "$DIFF_OUTPUT" | head -1)
DIFF_CONTENT=$(echo "$DIFF_OUTPUT" | tail -n +2)

# Pre-count tasks per tier-1 fan-out candidate so RALPH_COMPLETE can detect
# which specs received tasks this session (spec req 21).
FANOUT_SPECS=()
declare -A FANOUT_PRE_COUNTS=()
if [ "$DIFF_MODE" = "diff" ] && [ "$SPEC_HIDDEN" = "false" ]; then
  while IFS= read -r _fo_line; do
    if [[ "$_fo_line" =~ ^===[[:space:]]+(specs/[^[:space:]]+\.md)[[:space:]]+===$ ]]; then
      _fo_label=$(basename "${BASH_REMATCH[1]}" .md)
      FANOUT_SPECS+=("$_fo_label")
      FANOUT_PRE_COUNTS["$_fo_label"]=$(bd list -l "spec:$_fo_label" --json 2>/dev/null \
        | jq 'length' 2>/dev/null || echo 0)
    fi
  done <<< "$DIFF_CONTENT"
  unset _fo_line _fo_label
fi

SPEC_CONTENT=""
if [ -f "$SPEC_PATH" ]; then
  SPEC_CONTENT=$(cat "$SPEC_PATH")
fi

SPEC_DIFF=""
EXISTING_TASKS=""
EXISTING_SPEC=""

case "$DIFF_MODE" in
  diff)
    if [ -z "$(echo "$DIFF_CONTENT" | tr -d '[:space:]')" ]; then
      echo "No spec changes since last task creation."
      echo "  base_commit: $(jq -r '.base_commit // "unknown"' "$STATE_FILE")"
      echo ""
      echo "To force re-evaluation, run:"
      echo "  ralph plan -u $LABEL  # then ralph todo"
      exit 0
    fi
    TEMPLATE_NAME="todo-update"
    SPEC_DIFF="$DIFF_CONTENT"
    ;;
  tasks)
    # Tier 2 has no diff; Claude needs the full spec to reconcile against tasks.
    TEMPLATE_NAME="todo-update"
    EXISTING_TASKS="$DIFF_CONTENT"
    EXISTING_SPEC="## Existing Specification

The main spec file (\`$SPEC_PATH\`) contains the full current specification:

\`\`\`markdown
$SPEC_CONTENT
\`\`\`"
    ;;
  new)
    # Tier 4: full decomposition
    TEMPLATE_NAME="todo-new"
    ;;
  *)
    echo "Error: unexpected diff mode: $DIFF_MODE"
    exit 1
    ;;
esac

UPDATE_MODE="false"
if [ "$TEMPLATE_NAME" = "todo-update" ]; then
  UPDATE_MODE="true"
fi

mkdir -p "$RALPH_DIR/logs"

# Pin context from the configured pinnedContext file
PINNED_CONTEXT_FILE="${PINNED_CONTEXT_FILE:-$(get_pinned_context_file)}"
PINNED_CONTEXT=""
if [ -f "$PINNED_CONTEXT_FILE" ]; then
  PINNED_CONTEXT=$(cat "$PINNED_CONTEXT_FILE")
fi

# Extract title from spec file (first heading)
SPEC_TITLE=$(grep -m 1 '^#' "$SPEC_PATH" | sed 's/^#* *//' || echo "$LABEL")

# Get molecule ID from per-label state file (for update mode)
MOLECULE_ID=$(jq -r '.molecule // empty' "$STATE_FILE")

echo "Ralph Todo: Converting spec to molecule..."
echo "  Label: $LABEL"
echo "  Spec: $SPEC_PATH"
echo "  Title: $SPEC_TITLE"
if [ "$UPDATE_MODE" = "true" ]; then
  if [ -n "$MOLECULE_ID" ]; then
    echo "  Mode: UPDATE (bonding new tasks to existing molecule)"
    echo "  Molecule: $MOLECULE_ID"
    echo "  Detection: tier ${DIFF_MODE/#diff/1 (git diff)}${DIFF_MODE/#tasks/2 (molecule-based)}"
  else
    echo "  Mode: UPDATE (creating molecule for existing spec)"
    echo "  Creating epic..."
    MOLECULE_ID=$(bd create --type=epic --title="$SPEC_TITLE" --labels="spec:$LABEL" --silent)
    jq --arg mol "$MOLECULE_ID" '.molecule = $mol' "$STATE_FILE" > "$STATE_FILE.tmp" && mv "$STATE_FILE.tmp" "$STATE_FILE"
    echo "  Molecule: $MOLECULE_ID"
  fi
else
  echo "  Mode: NEW (creating molecule from scratch)"
fi
echo ""

# Read companion manifests
COMPANIONS=$(read_manifests "$STATE_FILE")

# Read implementation notes from state file and format as markdown bullet list
IMPLEMENTATION_NOTES=""
if [ -f "$STATE_FILE" ]; then
  _notes_json=$(jq -r '.implementation_notes // empty' "$STATE_FILE" 2>/dev/null || true)
  if [ -n "$_notes_json" ] && [ "$_notes_json" != "null" ]; then
    _notes_list=$(echo "$_notes_json" | jq -r '.[] // empty' 2>/dev/null || true)
    if [ -n "$_notes_list" ]; then
      IMPLEMENTATION_NOTES="## Implementation Notes

The following implementation hints were gathered during planning. Use them as guidance:

"
      while IFS= read -r _note; do
        IMPLEMENTATION_NOTES="${IMPLEMENTATION_NOTES}- ${_note}
"
      done <<< "$_notes_list"
    fi
  fi
  unset _notes_json _notes_list _note
fi

# Render template using centralized render_template function
if [ "$UPDATE_MODE" = "true" ]; then
  # Compute molecule progress
  MOLECULE_PROGRESS=""
  if [ -n "$MOLECULE_ID" ]; then
    PROGRESS_OUTPUT=$(bd mol progress "$MOLECULE_ID" 2>/dev/null || true)
    if [ -n "$PROGRESS_OUTPUT" ]; then
      MOLECULE_PROGRESS="$PROGRESS_OUTPUT"
    fi
  fi

  PROMPT_CONTENT=$(render_template "$TEMPLATE_NAME" \
    "LABEL=$LABEL" \
    "SPEC_PATH=$SPEC_PATH" \
    "EXISTING_SPEC=$EXISTING_SPEC" \
    "MOLECULE_ID=$MOLECULE_ID" \
    "MOLECULE_PROGRESS=$MOLECULE_PROGRESS" \
    "SPEC_DIFF=$SPEC_DIFF" \
    "EXISTING_TASKS=$EXISTING_TASKS" \
    "COMPANIONS=$COMPANIONS" \
    "PINNED_CONTEXT=$PINNED_CONTEXT" \
    "README_INSTRUCTIONS=$README_INSTRUCTIONS" \
    "IMPLEMENTATION_NOTES=$IMPLEMENTATION_NOTES" \
    "EXIT_SIGNALS=")
else
  PROMPT_CONTENT=$(render_template "$TEMPLATE_NAME" \
    "LABEL=$LABEL" \
    "SPEC_PATH=$SPEC_PATH" \
    "SPEC_CONTENT=$SPEC_CONTENT" \
    "COMPANIONS=$COMPANIONS" \
    "CURRENT_FILE=$STATE_FILE" \
    "PINNED_CONTEXT=$PINNED_CONTEXT" \
    "README_INSTRUCTIONS=$README_INSTRUCTIONS" \
    "IMPLEMENTATION_NOTES=$IMPLEMENTATION_NOTES" \
    "EXIT_SIGNALS=")
fi

LOG="$RALPH_DIR/logs/todo-$(date +%Y%m%d-%H%M%S).log"

echo "=== Creating Task Breakdown ==="
echo ""
# Use stream-json for real-time output display with configurable visibility
export PROMPT_CONTENT
run_claude_stream "PROMPT_CONTENT" "$LOG" "$CONFIG"

# Check for completion by examining the result in the JSON log
if jq -e '[.[] | select(.type == "result") | .result | contains("RALPH_COMPLETE")] | any' -s "$LOG" >/dev/null 2>&1; then
  echo ""
  echo "Molecule creation complete!"

  FINAL_SPEC_PATH="$SPECS_DIR/$LABEL.md"

  # Strip Implementation Notes section from spec if present
  if [ -f "$SPEC_PATH" ]; then
    SPEC_CONTENT=$(cat "$SPEC_PATH")
    FINAL_CONTENT=$(strip_implementation_notes "$SPEC_CONTENT")

    if [ "$SPEC_CONTENT" != "$FINAL_CONTENT" ]; then
      echo ""
      echo "Stripping Implementation Notes from $FINAL_SPEC_PATH..."
      echo "$FINAL_CONTENT" > "$FINAL_SPEC_PATH"
    fi
  fi

  # Commit the spec file
  if [ -f "$FINAL_SPEC_PATH" ]; then
    echo ""
    echo "Committing spec..."
    git add "$FINAL_SPEC_PATH"
    if [ -f "$PINNED_CONTEXT_FILE" ]; then
      git add "$PINNED_CONTEXT_FILE"
    fi
    if git diff --cached --quiet; then
      echo "  (no changes to commit)"
    else
      COMMIT_MSG="Add $LABEL specification"
      if [ "$UPDATE_MODE" = "true" ]; then
        COMMIT_MSG="Update $LABEL specification"
      fi
      if git commit -m "$COMMIT_MSG" >/dev/null; then
        echo "  Committed: $FINAL_SPEC_PATH"
      else
        echo "  (commit failed or nothing to commit)"
      fi
    fi
  fi

  # Commit working set then push to Dolt remote so host can pull them
  # bd dolt push only pushes committed data; without commit, working set
  # changes are lost to subsequent dolt clone (e.g., ralph run container)
  echo ""
  echo "Pushing beads to Dolt remote..."
  if ! bd dolt commit 2>&1; then
    echo "ERROR: bd dolt commit failed — beads will NOT sync to host"
    echo "  Tasks were created locally but cannot be pushed."
    echo "  Re-run: ralph todo -s $LABEL"
    exit 1
  fi
  if ! bd dolt push 2>&1; then
    echo "ERROR: bd dolt push failed — beads will NOT sync to host"
    echo "  Tasks were committed locally but push to remote failed."
    echo "  Re-run: ralph todo -s $LABEL"
    exit 1
  fi

  # Per-spec cursor fan-out (spec req 21).
  HEAD_COMMIT=$(git rev-parse HEAD)
  ANCHOR_RECEIVED_TASKS=1
  if [ "$DIFF_MODE" = "diff" ] && [ "$SPEC_HIDDEN" = "false" ]; then
    _anchor_in_fanout=0
    for _fo_label in "${FANOUT_SPECS[@]+"${FANOUT_SPECS[@]}"}"; do
      _fo_pre="${FANOUT_PRE_COUNTS[$_fo_label]:-0}"
      _fo_post=$(bd list -l "spec:$_fo_label" --json 2>/dev/null \
        | jq 'length' 2>/dev/null || echo 0)
      if [ "$_fo_label" = "$LABEL" ]; then
        _anchor_in_fanout=1
        if [ "$_fo_post" -le "$_fo_pre" ]; then
          ANCHOR_RECEIVED_TASKS=0
        fi
        continue
      fi
      if [ "$_fo_post" -le "$_fo_pre" ]; then
        continue
      fi
      _fo_state="$RALPH_DIR/state/${_fo_label}.json"
      advance_spec_cursor "$_fo_state" "$_fo_label" "specs/${_fo_label}.md" "$HEAD_COMMIT"
      echo "Advanced sibling $_fo_label base_commit: $HEAD_COMMIT"
    done
    if [ "$_anchor_in_fanout" = "0" ]; then
      ANCHOR_RECEIVED_TASKS=0
    fi
    unset _fo_label _fo_pre _fo_post _fo_state _anchor_in_fanout
  fi

  # Anchor always clears implementation_notes; base_commit advances only when
  # the anchor itself received tasks (same rule as siblings).
  if [ -f "$STATE_FILE" ]; then
    if [ "$SPEC_HIDDEN" = "false" ] && [ "$ANCHOR_RECEIVED_TASKS" = "1" ]; then
      jq --arg bc "$HEAD_COMMIT" 'del(.implementation_notes) | .base_commit = $bc' \
        "$STATE_FILE" > "$STATE_FILE.tmp" && mv "$STATE_FILE.tmp" "$STATE_FILE"
      echo ""
      echo "Stored base_commit: $HEAD_COMMIT"
    else
      jq 'del(.implementation_notes)' \
        "$STATE_FILE" > "$STATE_FILE.tmp" && mv "$STATE_FILE.tmp" "$STATE_FILE"
    fi
  fi

  # Display the molecule ID if available
  STORED_MOLECULE=$(jq -r '.molecule // empty' "$STATE_FILE" 2>/dev/null || true)
  if [ -n "$STORED_MOLECULE" ]; then
    echo ""
    echo "Molecule ID: $STORED_MOLECULE"
    echo ""
    echo "To view molecule progress:"
    echo "  bd mol progress $STORED_MOLECULE"
  fi

  echo ""
  echo "To list created issues:"
  echo "  bd list -l spec:$LABEL"
  echo ""
  echo "To work through issues:"
  echo "  ralph run         # Work all issues automatically"
  echo "  ralph run --once  # Work one issue at a time"
else
  echo ""
  echo "Molecule creation did not complete. Review log: $LOG"
  echo "To retry: ralph todo"
fi
