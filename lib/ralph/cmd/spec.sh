#!/usr/bin/env bash
set -euo pipefail

# ralph spec [--verbose] [--verify/-v] [--judge/-j] [--all/-a] [--spec/-s NAME]
# Query spec annotations across all spec files.
#
# Default (no flags): fast annotation index — counts [verify], [judge],
# and unannotated criteria per spec file. No test execution, no LLM calls.
#
# --verbose:       expand to per-criterion detail showing each criterion and its
#                  annotation type. No short flag.
# --verify / -v:   run all [verify] shell tests (all specs by default, or --spec NAME).
# --judge / -j:    run all [judge] LLM evaluations (all specs by default, or --spec NAME).
# --all / -a:      run both --verify and --judge.
# --spec / -s NAME: filter to a single spec file (specs/NAME.md).

# Load shared helpers
# shellcheck source=util.sh
source "$(dirname "$0")/util.sh"

SPECS_DIR="specs"

#-----------------------------------------------------------------------------
# Helper: look up molecule ID for a spec from specs/README.md
#-----------------------------------------------------------------------------
lookup_molecule_id() {
  local spec_name="$1"  # e.g. "notifications" (without .md)
  local readme="$SPECS_DIR/README.md"

  if [ ! -f "$readme" ]; then
    echo ""
    return
  fi

  # Parse the spec table in README.md for rows matching the spec filename
  # Table format: | [spec.md](./spec.md) | code | beads-id | purpose |
  local molecule_id=""
  while IFS= read -r line; do
    # Match table rows containing the spec filename as a link
    if [[ "$line" == *"${spec_name}.md"* ]] && [[ "$line" == *"|"* ]]; then
      # Extract the Beads column (3rd data column, 4th pipe-separated field)
      # Format: | Spec | Code | Beads | Purpose |
      molecule_id=$(echo "$line" | awk -F'|' '{print $4}' | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
      # Clean up: remove markdown formatting, dashes used as empty markers
      if [ "$molecule_id" = "—" ] || [ "$molecule_id" = "-" ] || [ -z "$molecule_id" ]; then
        molecule_id=""
      fi
      break
    fi
  done < "$readme"

  echo "$molecule_id"
}

#-----------------------------------------------------------------------------
# Helper: run a [verify] test
#-----------------------------------------------------------------------------
run_verify_test() {
  local criterion="$1"
  local file_path="$2"
  local function_name="$3"

  if [ ! -f "$file_path" ]; then
    echo "  [FAIL] $criterion"
    echo "         $file_path not found"
    ((failed++)) || true
    has_failure=true
    return
  fi

  local exit_code test_output
  if [ -n "$function_name" ]; then
    test_output=$("$file_path" "$function_name" 2>&1) && exit_code=0 || exit_code=$?
  else
    test_output=$("$file_path" 2>&1) && exit_code=0 || exit_code=$?
  fi

  if [ "$exit_code" -eq 0 ]; then
    echo "  [PASS] $criterion"
    echo "         $file_path${function_name:+::$function_name} (exit 0)"
    ((passed++)) || true
  elif [ "$exit_code" -eq 77 ]; then
    echo "  [SKIP] $criterion"
    echo "         $file_path${function_name:+::$function_name} (exit 77 — skipped)"
    ((skipped++)) || true
  elif [ "$exit_code" -eq 78 ]; then
    echo "  [SKIP] $criterion"
    echo "         $file_path${function_name:+::$function_name} (exit 78 — not implemented)"
    ((skipped++)) || true
  else
    echo "  [FAIL] $criterion"
    echo "         $file_path${function_name:+::$function_name} (exit $exit_code)"
    ((failed++)) || true
    has_failure=true
    # Always show tail of output on failure (helps diagnose missing binaries etc.)
    if [ "$VERBOSE" != "true" ] && [ -n "$test_output" ]; then
      echo "$test_output" | tail -5 | while IFS= read -r line; do
        echo "         | $line"
      done
    fi
  fi

  # Show captured output in verbose mode
  if [ "$VERBOSE" = "true" ] && [ -n "$test_output" ]; then
    echo "$test_output" | while IFS= read -r line; do
      echo "         | $line"
    done
  fi
}

#-----------------------------------------------------------------------------
# Helper: run a [verify:wrapix] test inside a wrapix container
#-----------------------------------------------------------------------------

# Cached sandbox-mcp build state: "" = untried, "ok" = built, "skip" = unavailable
_WRAPIX_BUILD_STATE=""
_WRAPIX_BUILD_MSG=""

ensure_wrapix_build() {
  if [ -n "$_WRAPIX_BUILD_STATE" ]; then
    return 0
  fi

  local build_output build_exit
  build_output=$(nix build .#sandbox-mcp --no-link 2>&1) && build_exit=0 || build_exit=$?

  if [ "$build_exit" -eq 0 ]; then
    _WRAPIX_BUILD_STATE="ok"
  else
    _WRAPIX_BUILD_STATE="skip"
    _WRAPIX_BUILD_MSG=$(echo "$build_output" | tail -3)
  fi
}

run_verify_wrapix_test() {
  local criterion="$1"
  local file_path="$2"
  local function_name="$3"

  if [ ! -f "$file_path" ]; then
    echo "  [FAIL] $criterion"
    echo "         $file_path not found"
    ((failed++)) || true
    has_failure=true
    return
  fi

  # Build the sandbox-mcp sandbox (cached across invocations)
  ensure_wrapix_build

  if [ "$_WRAPIX_BUILD_STATE" = "skip" ]; then
    echo "  [FAIL] $criterion"
    echo "         Failed to build sandbox-mcp container"
    if [ -n "$_WRAPIX_BUILD_MSG" ]; then
      echo "$_WRAPIX_BUILD_MSG" | while IFS= read -r line; do
        echo "         | $line"
      done
    fi
    ((failed++)) || true
    has_failure=true
    return
  fi

  # Run the test inside the container (skip krun microVM for spec tests)
  local exit_code test_output
  local container_file_path="/workspace/$file_path"
  local project_dir
  project_dir="$(pwd)"

  if [ -n "$function_name" ]; then
    test_output=$(nix run .#sandbox-mcp -- "$project_dir" bash -c "source $container_file_path && $function_name" 2>&1) && exit_code=0 || exit_code=$?
  else
    test_output=$(nix run .#sandbox-mcp -- "$project_dir" "$container_file_path" 2>&1) && exit_code=0 || exit_code=$?
  fi

  if [ "$exit_code" -eq 0 ]; then
    echo "  [PASS] $criterion"
    echo "         $file_path${function_name:+::$function_name} (exit 0, container)"
    ((passed++)) || true
  elif [ "$exit_code" -eq 77 ]; then
    echo "  [SKIP] $criterion"
    echo "         $file_path${function_name:+::$function_name} (exit 77 — skipped, container)"
    ((skipped++)) || true
  elif [ "$exit_code" -eq 78 ]; then
    echo "  [SKIP] $criterion"
    echo "         $file_path${function_name:+::$function_name} (exit 78 — not implemented, container)"
    ((skipped++)) || true
  else
    # Detect container infrastructure failures (not test failures)
    if echo "$test_output" | grep -q "payload does not match any of the supported image formats\|no policy.json file found\|Error: exec container process\|cannot find a cgroup"; then
      echo "  [SKIP] $criterion"
      echo "         Container runtime unavailable (cannot run nested containers)"
      ((skipped++)) || true
    else
      echo "  [FAIL] $criterion"
      echo "         $file_path${function_name:+::$function_name} (exit $exit_code, container)"
      ((failed++)) || true
      has_failure=true
      if [ "$VERBOSE" != "true" ] && [ -n "$test_output" ]; then
        echo "$test_output" | tail -5 | while IFS= read -r line; do
          echo "         | $line"
        done
      fi
    fi
  fi

  # Show captured output in verbose mode
  if [ "$VERBOSE" = "true" ] && [ -n "$test_output" ]; then
    echo "$test_output" | while IFS= read -r line; do
      echo "         | $line"
    done
  fi
}

#-----------------------------------------------------------------------------
# Helper: run a [judge] test
#-----------------------------------------------------------------------------
run_judge_test() {
  local criterion="$1"
  local file_path="$2"
  local function_name="$3"

  if [ ! -f "$file_path" ]; then
    echo "  [FAIL] $criterion"
    echo "         $file_path not found"
    ((failed++)) || true
    has_failure=true
    return
  fi

  # Reset judge state, source the test file, and call the rubric function
  judge_reset

  # shellcheck disable=SC1090
  source "$file_path"
  if [ -n "$function_name" ] && declare -f "$function_name" >/dev/null 2>&1; then
    "$function_name"
  fi

  # Invoke LLM judge via run_judge
  local judge_exit=0
  run_judge && judge_exit=0 || judge_exit=$?

  if [ "$judge_exit" -eq 0 ]; then
    echo "  [PASS] $criterion"
    if [ -n "$JUDGE_REASONING" ]; then
      echo "         \"$JUDGE_REASONING\""
    fi
    ((passed++)) || true
  elif [ "$judge_exit" -eq 2 ]; then
    # Error (missing files, LLM unavailable, etc.) — report as FAIL with reason
    echo "  [FAIL] $criterion"
    echo "         $JUDGE_REASONING"
    ((failed++)) || true
    has_failure=true
  else
    echo "  [FAIL] $criterion"
    if [ -n "$JUDGE_REASONING" ]; then
      echo "         \"$JUDGE_REASONING\""
    fi
    ((failed++)) || true
    has_failure=true
  fi
}

#-----------------------------------------------------------------------------
# Show annotation index for all spec files
#-----------------------------------------------------------------------------
show_annotation_index() {
  # Find all spec files (excluding README.md)
  local spec_files=()
  for f in "$SPECS_DIR"/*.md; do
    [ -f "$f" ] || continue
    [ "$(basename "$f")" = "README.md" ] && continue
    spec_files+=("$f")
  done

  if [ ${#spec_files[@]} -eq 0 ]; then
    echo "No spec files found in $SPECS_DIR/"
    return 0
  fi

  echo "Ralph Specs"
  echo "============================"

  local total_verify=0
  local total_judge=0
  local total_unannotated=0

  for spec_file in "${spec_files[@]}"; do
    local spec_name
    spec_name=$(basename "$spec_file")

    # Parse annotations; skip files without success criteria
    local annotations
    annotations=$(parse_spec_annotations "$spec_file" 2>/dev/null) || continue
    if [ -z "$annotations" ]; then
      continue
    fi

    # Count by annotation type (verify-wrapix counts as verify)
    local verify_count judge_count none_count
    verify_count=$(echo "$annotations" | awk -F'\t' '$2 == "verify" || $2 == "verify-wrapix"' | wc -l)
    judge_count=$(echo "$annotations" | awk -F'\t' '$2 == "judge"' | wc -l)
    none_count=$(echo "$annotations" | awk -F'\t' '$2 == "none"' | wc -l)

    # Trim whitespace from wc -l output
    verify_count=$((verify_count))
    judge_count=$((judge_count))
    none_count=$((none_count))

    total_verify=$((total_verify + verify_count))
    total_judge=$((total_judge + judge_count))
    total_unannotated=$((total_unannotated + none_count))

    # Display summary line
    printf '  %-24s %d verify, %d judge, %d unannotated\n' \
      "$spec_name" "$verify_count" "$judge_count" "$none_count"

    # Verbose: show per-criterion detail with annotation type and test path
    if [ "$VERBOSE" = "true" ]; then
      while IFS=$'\t' read -r criterion ann_type ann_file_path ann_function_name _checked; do
        if [ "$ann_type" != "none" ] && [ -n "$ann_file_path" ]; then
          local test_ref="$ann_file_path"
          if [ -n "$ann_function_name" ]; then
            test_ref="${test_ref}::${ann_function_name}"
          fi
          printf '    [%-6s] %s → %s\n' "$ann_type" "$criterion" "$test_ref"
        else
          printf '    [%-6s] %s\n' "$ann_type" "$criterion"
        fi
      done <<< "$annotations"
    fi
  done

  echo ""
  printf 'Total: %d verify, %d judge, %d unannotated\n' \
    "$total_verify" "$total_judge" "$total_unannotated"
}

#-----------------------------------------------------------------------------
# Run verify/judge tests for a single spec file
# Usage: run_single_spec_tests <spec_file> <label> <molecule_id>
# Increments passed/failed/skipped counters (caller must initialize).
# Sets has_failure=true on any failure.
# Prints per-spec header and results.
#-----------------------------------------------------------------------------
run_single_spec_tests() {
  local spec_file="$1"
  local label="$2"
  local molecule_id="$3"

  # Parse annotations from the spec
  local annotations
  annotations=$(parse_spec_annotations "$spec_file" 2>/dev/null) || return 0
  if [ -z "$annotations" ]; then
    return 0
  fi

  # Determine mode label
  local mode_label=""
  if [ "$VERIFY" = "true" ] && [ "$JUDGE" = "true" ]; then
    mode_label="Verify+Judge"
  elif [ "$VERIFY" = "true" ]; then
    mode_label="Verify"
  else
    mode_label="Judge"
  fi

  local header_text="Ralph $mode_label: $label"
  if [ -n "$molecule_id" ]; then
    header_text="$header_text ($molecule_id)"
  fi
  echo "$header_text"
  printf '=%.0s' $(seq 1 ${#header_text})
  echo ""

  while IFS=$'\t' read -r criterion ann_type file_path function_name _checked <&3; do
    if [ "$VERIFY" = "true" ] && [ "$JUDGE" = "true" ]; then
      # --all mode: run both verify and judge, skip only unannotated
      if [ "$ann_type" = "none" ]; then
        echo "  [SKIP] $criterion (no annotation)"
        ((skipped++)) || true
      elif [ "$ann_type" = "verify" ]; then
        run_verify_test "$criterion" "$file_path" "$function_name"
      elif [ "$ann_type" = "verify-wrapix" ]; then
        run_verify_wrapix_test "$criterion" "$file_path" "$function_name"
      elif [ "$ann_type" = "judge" ]; then
        run_judge_test "$criterion" "$file_path" "$function_name"
      fi
    elif [ "$VERIFY" = "true" ]; then
      if [ "$ann_type" = "verify" ]; then
        run_verify_test "$criterion" "$file_path" "$function_name"
      elif [ "$ann_type" = "verify-wrapix" ]; then
        run_verify_wrapix_test "$criterion" "$file_path" "$function_name"
      elif [ "$ann_type" = "none" ]; then
        echo "  [SKIP] $criterion (no annotation)"
        ((skipped++)) || true
      fi
      # judge-only criteria are silently omitted in verify mode
    elif [ "$JUDGE" = "true" ]; then
      if [ "$ann_type" = "judge" ]; then
        run_judge_test "$criterion" "$file_path" "$function_name"
      elif [ "$ann_type" = "none" ]; then
        echo "  [SKIP] $criterion (no annotation)"
        ((skipped++)) || true
      fi
      # verify-only and verify-wrapix criteria are silently omitted in judge mode
    fi
  done 3<<< "$annotations"

  echo ""

  # Signal that this spec had criteria (was not skipped)
  return 0
}

#-----------------------------------------------------------------------------
# Run verify/judge tests across specs
# When SPEC_FILTER is set: run only that spec (single-spec format).
# Otherwise: iterate all specs with grouped output and cross-spec summary.
#-----------------------------------------------------------------------------
run_spec_tests() {
  passed=0
  failed=0
  skipped=0
  has_failure=false

  if [ -n "$SPEC_FILTER" ]; then
    # Single-spec mode: run only the filtered spec
    local spec_file="$SPECS_DIR/${SPEC_FILTER}.md"
    if [ ! -f "$spec_file" ]; then
      error "Spec file not found: $spec_file"
    fi

    local molecule_id
    molecule_id=$(lookup_molecule_id "$SPEC_FILTER")

    run_single_spec_tests "$spec_file" "$SPEC_FILTER" "$molecule_id"

    echo "$passed passed, $failed failed, $skipped skipped"

    if [ "$has_failure" = "true" ]; then
      return 1
    fi
    return 0
  fi

  # Multi-spec mode: iterate all spec files
  local spec_files=()
  for f in "$SPECS_DIR"/*.md; do
    [ -f "$f" ] || continue
    [ "$(basename "$f")" = "README.md" ] && continue
    spec_files+=("$f")
  done

  if [ ${#spec_files[@]} -eq 0 ]; then
    echo "No spec files found in $SPECS_DIR/"
    return 0
  fi

  local spec_count=0

  for spec_file in "${spec_files[@]}"; do
    local label
    label=$(basename "$spec_file" .md)

    # Check if this spec has success criteria; skip silently if not
    local annotations
    annotations=$(parse_spec_annotations "$spec_file" 2>/dev/null) || continue
    if [ -z "$annotations" ]; then
      continue
    fi

    local molecule_id
    molecule_id=$(lookup_molecule_id "$label")

    run_single_spec_tests "$spec_file" "$label" "$molecule_id"
    ((spec_count++)) || true
  done

  # Cross-spec summary
  echo "Summary: $passed passed, $failed failed, $skipped skipped ($spec_count specs)"

  if [ "$has_failure" = "true" ]; then
    return 1
  fi
  return 0
}

#-----------------------------------------------------------------------------
# Parse arguments
#-----------------------------------------------------------------------------
VERBOSE=false
VERIFY=false
JUDGE=false
SPEC_FILTER=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --verbose)
      VERBOSE=true
      shift
      ;;
    --verify)
      VERIFY=true
      shift
      ;;
    --judge)
      JUDGE=true
      shift
      ;;
    --all)
      VERIFY=true
      JUDGE=true
      shift
      ;;
    --spec)
      SPEC_FILTER="$2"
      shift 2
      ;;
    -h|--help)
      echo "Usage: ralph spec [--verbose] [--verify/-v] [--judge/-j] [--all/-a] [--spec/-s NAME]"
      echo ""
      echo "Query spec annotations across all spec files."
      echo ""
      echo "Options:"
      echo "  --verbose      Show per-criterion detail (no short flag)"
      echo "  --verify, -v   Run [verify] shell tests (all specs by default)"
      echo "  --judge, -j    Run [judge] LLM evaluations (all specs by default)"
      echo "  --all, -a      Run both --verify and --judge"
      echo "  --spec, -s     Filter to a single spec file (by name, without .md)"
      echo "  --help, -h     Show this help message"
      echo ""
      echo "Default mode (no flags) is instant: scans annotations without"
      echo "executing tests or invoking LLMs."
      exit 0
      ;;
    -*)
      # Handle composed short flags: -v, -j, -a, -s, -vj, etc.
      local_flags="${1#-}"
      shift
      while [ -n "$local_flags" ]; do
        local_flag="${local_flags:0:1}"
        local_flags="${local_flags:1}"
        case "$local_flag" in
          v)
            VERIFY=true
            ;;
          j)
            JUDGE=true
            ;;
          a)
            VERIFY=true
            JUDGE=true
            ;;
          s)
            # -s consumes the next argument as the spec name
            if [ $# -gt 0 ]; then
              SPEC_FILTER="$1"
              shift
            else
              error "Option -s requires a spec name argument.
Run 'ralph spec --help' for usage."
            fi
            # Any remaining flags after -s would be ambiguous; stop processing
            local_flags=""
            ;;
          *)
            error "Unknown short flag: -$local_flag
Run 'ralph spec --help' for usage."
            ;;
        esac
      done
      ;;
    *)
      error "Unknown argument: $1
Run 'ralph spec --help' for usage."
      ;;
  esac
done

#-----------------------------------------------------------------------------
# Main dispatch
#-----------------------------------------------------------------------------
if [ "$VERIFY" = "true" ] || [ "$JUDGE" = "true" ]; then
  run_spec_tests
else
  show_annotation_index
fi
