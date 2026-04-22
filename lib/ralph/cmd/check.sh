#!/usr/bin/env bash
set -euo pipefail

# ralph check — Template validation and post-loop review
#
# Modes:
#   ralph check              Post-loop review of the active spec (default)
#   ralph check --spec <n>   Post-loop review of a named spec
#   ralph check -s <n>       Short form
#   ralph check -t           Validate all templates (no Claude invocation)
#
# -t and --spec/-s are mutually exclusive: template validation is a static
# check that runs under `nix flake check`; the review invokes Claude.
#
# Exit codes:
#   0 = success (or review passed)
#   1 = errors found (or review found issues)

# Load shared helpers
# shellcheck source=util.sh
source "$(dirname "$0")/util.sh"

RALPH_DIR="${RALPH_DIR:-.wrapix/ralph}"

#-----------------------------------------------------------------------------
# Flag parsing
#-----------------------------------------------------------------------------
DO_TEMPLATES=false
DO_SPEC=false
PRINT_HELP=false
SPEC_LABEL=""

while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help)
      PRINT_HELP=true
      shift
      ;;
    -t|--templates)
      DO_TEMPLATES=true
      shift
      ;;
    -s|--spec)
      DO_SPEC=true
      if [ -z "${2:-}" ]; then
        error "ralph check -s requires a <label> argument"
      fi
      SPEC_LABEL="$2"
      shift 2
      ;;
    --spec=*)
      DO_SPEC=true
      SPEC_LABEL="${1#--spec=}"
      shift
      ;;
    *)
      error "Unknown flag: $1 (see 'ralph check --help')"
      ;;
  esac
done

#-----------------------------------------------------------------------------
# Mutual exclusion: -t is a static check; -s invokes Claude
#-----------------------------------------------------------------------------
if [ "$DO_TEMPLATES" = "true" ] && [ "$DO_SPEC" = "true" ]; then
  error "ralph check -t and -s are mutually exclusive (see 'ralph check --help')"
fi

#-----------------------------------------------------------------------------
# --help → print usage, exit 0
#-----------------------------------------------------------------------------
if [ "$PRINT_HELP" = "true" ]; then
  echo "Usage: ralph check [flags]"
  echo ""
  echo "Modes:"
  echo "  (no flags)              Post-loop review of active spec (default)"
  echo "  -s, --spec <label>      Post-loop review of a named spec"
  echo "  -t, --templates         Validates all ralph templates (static, no Claude)"
  echo ""
  echo "-t and --spec/-s are mutually exclusive."
  echo ""
  echo "Template validation checks:"
  echo "  - Partial files exist"
  echo "  - Body files parse correctly"
  echo "  - No syntax errors in Nix expressions"
  echo "  - Dry-run render with dummy values"
  echo ""
  echo "Spec review spawns an independent reviewer agent that:"
  echo "  - Reads the spec and explores the codebase"
  echo "  - Assesses spec compliance, code quality, test adequacy"
  echo "  - Creates follow-up beads for actionable issues"
  echo "  - Pass/fail determined by whether new beads were created"
  echo ""
  echo "Environment:"
  echo "  RALPH_TEMPLATE_DIR  Template directory (from nix develop)"
  exit 0
fi

#-----------------------------------------------------------------------------
# No flags → default to spec review (resolve label from state/current)
#-----------------------------------------------------------------------------
if [ "$DO_TEMPLATES" = "false" ] && [ "$DO_SPEC" = "false" ]; then
  DO_SPEC=true
fi

#-----------------------------------------------------------------------------
# Template validation (-t)
#-----------------------------------------------------------------------------
run_template_validation() {
  # Template directory: use RALPH_TEMPLATE_DIR if set and exists
  local TEMPLATE_DIR=""
  if [ -n "${RALPH_TEMPLATE_DIR:-}" ] && [ -d "$RALPH_TEMPLATE_DIR" ]; then
    TEMPLATE_DIR="$RALPH_TEMPLATE_DIR"
  fi

  # Track errors
  local ERRORS=()

  # Validate RALPH_TEMPLATE_DIR is set
  if [ -z "$TEMPLATE_DIR" ]; then
    error "RALPH_TEMPLATE_DIR not set or directory doesn't exist.

To fix this, do one of the following:
  - Run 'ralph sync' to fetch templates from GitHub
  - Set RALPH_TEMPLATE_DIR to point to an existing template directory

Current value: ${RALPH_TEMPLATE_DIR:-<not set>}"
  fi

  echo "Checking templates in: $TEMPLATE_DIR"
  echo ""

  #---------------------------------------------------------------------------
  # Check 1: Partial files exist
  #---------------------------------------------------------------------------
  echo "Checking partials..."

  local PARTIAL_DIR="$TEMPLATE_DIR/partial"
  local EXPECTED_PARTIALS=("context-pinning.md" "exit-signals.md" "spec-header.md")

  for partial in "${EXPECTED_PARTIALS[@]}"; do
    local partial_path="$PARTIAL_DIR/$partial"
    if [ -f "$partial_path" ]; then
      echo "  ✓ $partial"
    else
      echo "  ✗ $partial (missing)"
      ERRORS+=("Missing partial: $partial_path")
    fi
  done

  #---------------------------------------------------------------------------
  # Check 2: Body files exist and are readable
  #---------------------------------------------------------------------------
  echo ""
  echo "Checking body files..."

  local BODY_FILES=("plan-new.md" "plan-update.md" "todo-new.md" "todo-update.md" "run.md" "check.md" "watch.md")

  for body in "${BODY_FILES[@]}"; do
    local body_path="$TEMPLATE_DIR/$body"
    if [ -f "$body_path" ]; then
      # Check it's readable
      if head -1 "$body_path" >/dev/null 2>&1; then
        echo "  ✓ $body"
      else
        echo "  ✗ $body (unreadable)"
        ERRORS+=("Unreadable body file: $body_path")
      fi
    else
      echo "  ✗ $body (missing)"
      ERRORS+=("Missing body file: $body_path")
    fi
  done

  #---------------------------------------------------------------------------
  # Check 3: Nix expressions are valid
  #---------------------------------------------------------------------------
  echo ""
  echo "Checking Nix expressions..."

  local NIX_FILE="$TEMPLATE_DIR/default.nix"

  if [ -f "$NIX_FILE" ]; then
    # Try to parse the Nix file
    if nix-instantiate --parse "$NIX_FILE" >/dev/null 2>&1; then
      echo "  ✓ default.nix (syntax valid)"
    else
      echo "  ✗ default.nix (syntax error)"
      local parse_error
      parse_error=$(nix-instantiate --parse "$NIX_FILE" 2>&1 || true)
      ERRORS+=("Nix syntax error in $NIX_FILE: $parse_error")
    fi

    # Try to evaluate the template module using nix eval with flake
    if nix eval --impure --expr "let lib = (builtins.getFlake \"nixpkgs\").lib; t = import $NIX_FILE { inherit lib; }; in t.validateTemplates" >/dev/null 2>&1; then
      echo "  ✓ default.nix (evaluation valid)"
    else
      echo "  ✗ default.nix (evaluation error)"
      local eval_error
      eval_error=$(nix eval --impure --expr "let lib = (builtins.getFlake \"nixpkgs\").lib; t = import $NIX_FILE { inherit lib; }; in t.validateTemplates" 2>&1 || true)
      ERRORS+=("Nix evaluation error: $eval_error")
    fi
  else
    echo "  ✗ default.nix (missing)"
    ERRORS+=("Missing Nix file: $NIX_FILE")
  fi

  # Check config.nix if present
  local CONFIG_FILE="$TEMPLATE_DIR/config.nix"
  if [ -f "$CONFIG_FILE" ]; then
    if nix-instantiate --parse "$CONFIG_FILE" >/dev/null 2>&1; then
      echo "  ✓ config.nix (syntax valid)"
    else
      echo "  ✗ config.nix (syntax error)"
      local parse_error
      parse_error=$(nix-instantiate --parse "$CONFIG_FILE" 2>&1 || true)
      ERRORS+=("Nix syntax error in $CONFIG_FILE: $parse_error")
    fi
  fi

  #---------------------------------------------------------------------------
  # Check 4: Partial references are valid
  #---------------------------------------------------------------------------
  echo ""
  echo "Checking partial references..."

  for body in "${BODY_FILES[@]}"; do
    local body_path="$TEMPLATE_DIR/$body"
    [ -f "$body_path" ] || continue

    local refs
    refs=$(grep -oE '\{\{> [a-z-]+\}\}' "$body_path" 2>/dev/null | sed 's/{{> //;s/}}//' || true)

    if [ -n "$refs" ]; then
      for ref in $refs; do
        local partial_path="$PARTIAL_DIR/${ref}.md"
        if [ -f "$partial_path" ]; then
          echo "  ✓ $body → {{> $ref}}"
        else
          echo "  ✗ $body → {{> $ref}} (partial missing)"
          ERRORS+=("$body references missing partial: $ref")
        fi
      done
    fi
  done

  #---------------------------------------------------------------------------
  # Check 5: Dry-run render with dummy values
  #---------------------------------------------------------------------------
  echo ""
  echo "Checking template rendering..."

  local TEMPLATES_TO_CHECK=("plan-new" "plan-update" "todo-new" "todo-update" "run" "check" "watch")

  # shellcheck disable=SC2016  # Single quotes are intentional for Nix expression building
  for template in "${TEMPLATES_TO_CHECK[@]}"; do
    local render_expr='
let
  lib = (builtins.getFlake "nixpkgs").lib;
  templateModule = import '"$NIX_FILE"' { inherit lib; };
  templates = templateModule.templates;
  variableDefs = templateModule.variableDefinitions;
  template = templates."'"$template"'";

  # Generate a dummy value for a variable based on its metadata
  makeDummy = name: def:
    let
      source = def.source or "unknown";
      lowerName = lib.toLower name;
    in
    if source == "args" then "dummy-${lowerName}"
    else if source == "state" then "dummy-state-${lowerName}"
    else if source == "computed" then
      if name == "SPEC_PATH" then "specs/dummy.md"
      else if name == "CURRENT_FILE" then ".wrapix/ralph/state/current.json"
      else if name == "NEW_REQUIREMENTS_PATH" then ".wrapix/ralph/state/dummy-feature.md"
      else if name == "MOLECULE_PROGRESS" then "50% (5/10)"
      else "dummy-computed-${lowerName}"
    else if source == "file" then "# Dummy content for ${name}"
    else if source == "beads" then "dummy-beads-${lowerName}"
    else if source == "config" then "dummy-config-${lowerName}"
    else "dummy-${lowerName}";

  allDummyVars = builtins.mapAttrs makeDummy variableDefs;
  templateVars = lib.filterAttrs (k: v: builtins.elem k template.variables) allDummyVars;
  rendered = template.render templateVars;

in builtins.stringLength rendered
'

    if nix eval --impure --expr "$render_expr" >/dev/null 2>&1; then
      echo "  ✓ $template (renders successfully)"
    else
      echo "  ✗ $template (render failed)"
      local render_error
      render_error=$(nix eval --impure --expr "$render_expr" 2>&1 || true)
      ERRORS+=("Template $template render failed: $render_error")
    fi
  done

  #---------------------------------------------------------------------------
  # Check 6: Variable placeholders in body files match declarations
  #---------------------------------------------------------------------------
  echo ""
  echo "Checking variable declarations..."

  local VAR_CHECK_NIX
  VAR_CHECK_NIX=$(mktemp)
  # shellcheck disable=SC2064
  trap "rm -f $VAR_CHECK_NIX" EXIT

  cat > "$VAR_CHECK_NIX" << NIXEOF
let
  lib = (builtins.getFlake "nixpkgs").lib;
  nixFile = builtins.getEnv "RALPH_CHECK_NIX_FILE";
  templates = (import nixFile { inherit lib; }).templates;

  # Extract {{VAR}} patterns from content (excluding {{> partial}})
  extractVars = content:
    let
      parts = builtins.split "[{][{]([A-Z_]+)[}][}]" content;
      matches = builtins.filter (p: builtins.isList p) parts;
    in
    map builtins.head matches;

  # Extract {{> partial-name}} references from content
  extractPartialRefs = content:
    let
      parts = builtins.split "[{][{]> ([a-z-]+)[}][}]" content;
      matches = builtins.filter (p: builtins.isList p) parts;
    in
    map builtins.head matches;

  results = builtins.mapAttrs (name: template:
    let
      declared = template.variables;
      usedInBody = extractVars template.content;
      # Only scan partials actually referenced by this template's body
      referencedPartials = extractPartialRefs template.content;
      referencedPartialContents = builtins.filter (x: x != null)
        (map (ref: template.partials.${ref} or null) referencedPartials);
      partialVars = builtins.concatLists (map extractVars referencedPartialContents);
      allUsed = usedInBody ++ partialVars;
      undeclared = builtins.filter (v: !(builtins.elem v declared)) allUsed;
    in
    { inherit declared undeclared; usedInBody = usedInBody; }
  ) templates;
in results
NIXEOF

  local var_check
  if var_check=$(RALPH_CHECK_NIX_FILE="$NIX_FILE" nix eval --impure --json --file "$VAR_CHECK_NIX" 2>/dev/null); then
    for template in plan-new plan-update todo-new todo-update step; do
      local undeclared
      undeclared=$(echo "$var_check" | jq -r ".\"$template\".undeclared | length" 2>/dev/null)
      if [ "$undeclared" = "0" ]; then
        echo "  ✓ $template (all variables declared)"
      else
        local undeclared_list
        undeclared_list=$(echo "$var_check" | jq -r ".\"$template\".undeclared | join(\", \")" 2>/dev/null)
        echo "  ✗ $template (undeclared variables: $undeclared_list)"
        ERRORS+=("Template $template uses undeclared variables: $undeclared_list")
      fi
    done
  else
    echo "  ⚠ Variable check skipped (evaluation failed)"
  fi

  #---------------------------------------------------------------------------
  # Summary
  #---------------------------------------------------------------------------
  echo ""
  echo "─────────────────────────────────────"

  if [ ${#ERRORS[@]} -eq 0 ]; then
    echo "✓ All template checks passed"
    return 0
  else
    echo "✗ ${#ERRORS[@]} error(s) found:"
    echo ""
    for err in "${ERRORS[@]}"; do
      echo "  • $err"
    done
    return 1
  fi
}

#-----------------------------------------------------------------------------
# Spec review (default / -s <label>)
#
# Split across two execution contexts:
#   - Host side (outside container): resolve label, pre-count beads, launch
#     wrapix, bd dolt pull after exit, post-count beads, compare.
#   - Container side (inside wrapix): render check.md, run Claude reviewer,
#     bd dolt push after RALPH_COMPLETE so the host can pull new beads.
#-----------------------------------------------------------------------------
run_spec_review() {
  local label_arg="${1:-}"

  # -----------------------------------------------------------------------
  # HOST SIDE: launch container, handle bead sync, compute verdict
  # -----------------------------------------------------------------------
  if [ ! -f /etc/wrapix/claude-config.json ] && command -v wrapix &>/dev/null; then
    # Resolve label on host so state file is validated before launching
    local host_label
    host_label=$(resolve_spec_label "$label_arg")

    echo "Ralph check: post-loop review for '$host_label'"
    echo ""

    # Pre-invocation bead count using spec label (survives container bead sync)
    local beads_before
    beads_before=$(bd list -l "spec-${host_label}" --json 2>/dev/null \
      | jq 'length' 2>/dev/null || echo 0)
    debug "Host pre-count: $beads_before bead(s) with label spec-${host_label}"

    # Re-launch in container with explicit label so container-side resolves
    # the same spec (avoids state/current drift during a long review).
    export RALPH_MODE=1
    export RALPH_CMD=check
    export RALPH_ARGS="-s $host_label"

    wrapix
    local wrapix_exit=$?

    # Container bead sync protocol: pull before re-counting on host side
    echo "Syncing beads from container..."
    bd dolt pull 2>/dev/null || echo "Warning: bd dolt pull failed (beads may not be synced)"

    if [ $wrapix_exit -ne 0 ]; then
      echo ""
      echo "Review did not complete (container exited $wrapix_exit)."
      return $wrapix_exit
    fi

    # Post-sync bead count
    local beads_after_json beads_after
    beads_after_json=$(bd list -l "spec-${host_label}" --json 2>/dev/null || echo "[]")
    beads_after=$(echo "$beads_after_json" | jq 'length' 2>/dev/null || echo 0)
    local new_beads=$((beads_after - beads_before))
    debug "Host post-count: $beads_after bead(s) (new: $new_beads)"

    echo ""
    echo "─────────────────────────────────────"
    if [ "$new_beads" -le 0 ]; then
      echo "✓ Review PASSED — no new beads created"
      notify_event "Ralph" "Review passed for $host_label"
      return 0
    else
      echo "✗ Review found $new_beads new bead(s)"
      echo ""
      echo "Follow-up beads:"
      echo "$beads_after_json" \
        | jq -r --argjson n "$beads_before" '.[$n:] | .[] | "  - \(.id): \(.title)"' \
          2>/dev/null || true
      notify_event "Ralph" "Review found $new_beads issue(s) for $host_label"
      return 1
    fi
  fi

  # -----------------------------------------------------------------------
  # CONTAINER SIDE: render check.md, run reviewer, push beads to Dolt
  # -----------------------------------------------------------------------
  local label
  label=$(resolve_spec_label "$label_arg")

  local state_file="$RALPH_DIR/state/${label}.json"

  local molecule_id base_commit spec_path
  molecule_id=$(jq -r '.molecule // empty' "$state_file")
  base_commit=$(jq -r '.base_commit // empty' "$state_file")
  spec_path=$(jq -r '.spec_path // empty' "$state_file")

  if [ -z "$molecule_id" ]; then
    error "No molecule ID found in $state_file — run 'ralph todo' first."
  fi

  if [ -z "$base_commit" ]; then
    warn "No base_commit in state file — reviewer will not have a diff baseline."
    base_commit="(unknown)"
  fi

  if [ -z "$spec_path" ]; then
    spec_path="specs/${label}.md"
  fi

  echo "Ralph check: post-loop review for '$label'"
  echo "  Spec: $spec_path"
  echo "  Molecule: $molecule_id"
  echo "  Base commit: $base_commit"
  echo ""

  # BEADS_SUMMARY: titles + status for the molecule's beads
  local beads_json beads_summary
  beads_json=$(bd_json list --parent "$molecule_id" --json 2>/dev/null || echo "[]")
  if echo "$beads_json" | jq -e 'type == "array" and length > 0' >/dev/null 2>&1; then
    beads_summary=$(echo "$beads_json" | jq -r '.[] | "- \(.id): \(.title) [\(.status)]"')
  else
    beads_summary="(no beads found)"
  fi

  echo "Beads summary:"
  echo "$beads_summary"
  echo ""

  # Read companion manifests
  local companions=""
  if [ -f "$state_file" ]; then
    companions=$(read_manifests "$state_file")
  fi

  # Pin context
  local pinned_context=""
  if [ -f "specs/README.md" ]; then
    pinned_context=$(cat "specs/README.md")
  fi

  # Load config for model override and output settings
  local config_file="$RALPH_DIR/config.nix"
  local config="{}"
  if [ -f "$config_file" ]; then
    config=$(nix eval --json --file "$config_file" 2>/dev/null || echo "{}")
  fi

  local model_check
  model_check=$(resolve_model "check" "$config")

  # Render check.md template
  local review_prompt
  review_prompt=$(render_template check \
    "SPEC_PATH=$spec_path" \
    "LABEL=$label" \
    "BEADS_SUMMARY=$beads_summary" \
    "BASE_COMMIT=$base_commit" \
    "MOLECULE_ID=$molecule_id" \
    "COMPANIONS=$companions" \
    "PINNED_CONTEXT=$pinned_context" \
    "EXIT_SIGNALS=")

  mkdir -p "$RALPH_DIR/logs"
  local log="$RALPH_DIR/logs/check-${label}.log"

  echo "=== Starting review (fresh context) ==="
  echo ""

  # Run the reviewer agent
  export CHECK_PROMPT="$review_prompt"
  run_claude_stream "CHECK_PROMPT" "$log" "$config" "$model_check"

  # Require RALPH_COMPLETE before pushing beads
  if ! jq -e '[.[] | select(.type == "result") | .result | contains("RALPH_COMPLETE")] | any' -s "$log" >/dev/null 2>&1; then
    echo ""
    echo "Review did not complete (no RALPH_COMPLETE signal)."

    if jq -e '[.[] | select(.type == "result") | .result | contains("RALPH_CLARIFY")] | any' -s "$log" >/dev/null 2>&1; then
      local clarify_text
      clarify_text=$(jq -r 'select(.type == "result") | .result' "$log" \
        | grep -oP 'RALPH_CLARIFY:\s*\K.*' | head -1)
      echo "Reviewer needs clarification: ${clarify_text:-<see log>}"
    fi
    return 1
  fi

  # Container bead sync protocol: commit + push so the host can pull any
  # fix-up or ralph:clarify beads the reviewer created.
  echo ""
  echo "Pushing beads to Dolt remote..."
  bd dolt commit >/dev/null 2>&1 || true
  if ! bd dolt push 2>&1; then
    echo "Warning: bd dolt push failed — beads may not reach host"
  fi

  return 0
}

#-----------------------------------------------------------------------------
# Main execution
#-----------------------------------------------------------------------------
TEMPLATE_EXIT=0
SPEC_EXIT=0

if [ "$DO_TEMPLATES" = "true" ]; then
  run_template_validation || TEMPLATE_EXIT=$?
fi

if [ "$DO_SPEC" = "true" ]; then
  run_spec_review "$SPEC_LABEL" || SPEC_EXIT=$?
fi

# Exit with failure if either mode failed
if [ "$TEMPLATE_EXIT" -ne 0 ] || [ "$SPEC_EXIT" -ne 0 ]; then
  exit 1
fi

exit 0
