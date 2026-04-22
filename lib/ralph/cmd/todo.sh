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
  _HOST_PRE_COUNT=$(bd list -l "spec-${_HOST_LABEL}" --json 2>/dev/null \
    | jq 'length' 2>/dev/null || echo 0)

  wrapix
  wrapix_exit=$?

  if [ $wrapix_exit -eq 0 ]; then
    # Pull beads synced by container's bd dolt push
    echo "Syncing beads from container..."
    bd dolt pull 2>/dev/null || echo "Warning: bd dolt pull failed (beads may not be synced)"

    # Host-side verification: did task count increase?
    _HOST_POST_COUNT=$(bd list -l "spec-${_HOST_LABEL}" --json 2>/dev/null \
      | jq 'length' 2>/dev/null || echo 0)

    if [ "$_HOST_POST_COUNT" -le "$_HOST_PRE_COUNT" ]; then
      _PREV_BASE_COMMIT=$(jq -r '.base_commit // "HEAD~1"' "$_HOST_STATE_FILE" 2>/dev/null || echo "HEAD~1")
      echo ""
      echo "ERROR: RALPH_COMPLETE but no new tasks detected after sync."
      echo "  Container dolt push likely failed — beads are lost."
      echo "  Check: bd list -l spec-${_HOST_LABEL}"
      echo "  To re-run: ralph todo --since ${_PREV_BASE_COMMIT}"
      echo ""
      echo "Resetting state file to allow re-run..."
      # Remove molecule and base_commit so tier 4 (new) can run again
      if [ -f "$_HOST_STATE_FILE" ]; then
        jq 'del(.molecule, .base_commit)' "$_HOST_STATE_FILE" > "$_HOST_STATE_FILE.tmp" \
          && mv "$_HOST_STATE_FILE.tmp" "$_HOST_STATE_FILE"
        echo "  Reset: $_HOST_STATE_FILE"
      fi
      exit 1
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
SPECS_README="$SPECS_DIR/README.md"

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
  README_INSTRUCTIONS="## README Update (required for cross-machine state recovery)

After creating the molecule, update \`specs/README.md\`:
- Find the row for this spec
- Update the Beads column with the molecule ID (epic ID)"
fi

# Check spec file exists
if [ ! -f "$SPEC_PATH" ]; then
  echo "Error: Spec file not found: $SPEC_PATH"
  echo "Run 'ralph plan' first to create the specification."
  exit 1
fi

# Check for uncommitted spec changes (git-tracked specs only)
if [ "$SPEC_HIDDEN" = "false" ] && git ls-files --error-unmatch "$SPEC_PATH" >/dev/null 2>&1; then
  if ! git diff --quiet -- "$SPEC_PATH" 2>/dev/null || ! git diff --cached --quiet -- "$SPEC_PATH" 2>/dev/null; then
    echo "Error: Uncommitted changes detected in $SPEC_PATH"
    echo ""
    echo "Spec changes must be committed before running ralph todo."
    echo "  git add $SPEC_PATH && git commit -m 'Update $LABEL spec'"
    exit 1
  fi
fi

# Use compute_spec_diff for four-tier detection
DIFF_ARGS=()
if [ -n "$SINCE_FLAG" ]; then
  DIFF_ARGS+=(--since "$SINCE_FLAG")
fi

DIFF_OUTPUT=$(compute_spec_diff "$STATE_FILE" "${DIFF_ARGS[@]+"${DIFF_ARGS[@]}"}")
DIFF_MODE=$(echo "$DIFF_OUTPUT" | head -1)
DIFF_CONTENT=$(echo "$DIFF_OUTPUT" | tail -n +2)

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

# Pin context from specs/README.md
PINNED_CONTEXT=""
if [ -f "$SPECS_README" ]; then
  PINNED_CONTEXT=$(cat "$SPECS_README")
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
    MOLECULE_ID=$(bd create --type=epic --title="$SPEC_TITLE" --labels="spec-$LABEL" --silent)
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

  # Clear implementation_notes from state file — they've been consumed
  if [ -f "$STATE_FILE" ] && jq -e '.implementation_notes' "$STATE_FILE" >/dev/null 2>&1; then
    jq 'del(.implementation_notes)' "$STATE_FILE" > "$STATE_FILE.tmp" && mv "$STATE_FILE.tmp" "$STATE_FILE"
    echo "Cleared implementation_notes from state file"
  fi

  # Commit the spec file
  if [ -f "$FINAL_SPEC_PATH" ]; then
    echo ""
    echo "Committing spec..."
    git add "$FINAL_SPEC_PATH" "$SPECS_README" 2>/dev/null || true
    if git diff --cached --quiet 2>/dev/null; then
      echo "  (no changes to commit)"
    else
      COMMIT_MSG="Add $LABEL specification"
      if [ "$UPDATE_MODE" = "true" ]; then
        COMMIT_MSG="Update $LABEL specification"
      fi
      git commit -m "$COMMIT_MSG" >/dev/null 2>&1 && echo "  Committed: $FINAL_SPEC_PATH" || echo "  (commit failed or nothing to commit)"
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

  # Store base_commit (HEAD) on successful completion
  if [ "$SPEC_HIDDEN" = "false" ]; then
    HEAD_COMMIT=$(git rev-parse HEAD)
    jq --arg bc "$HEAD_COMMIT" '.base_commit = $bc' "$STATE_FILE" > "$STATE_FILE.tmp" && mv "$STATE_FILE.tmp" "$STATE_FILE"
    echo ""
    echo "Stored base_commit: $HEAD_COMMIT"
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
  echo "  bd list -l spec-$LABEL"
  echo ""
  echo "To work through issues:"
  echo "  ralph run         # Work all issues automatically"
  echo "  ralph run --once  # Work one issue at a time"
else
  echo ""
  echo "Molecule creation did not complete. Review log: $LOG"
  echo "To retry: ralph todo"
fi
