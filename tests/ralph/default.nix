# Ralph workflow tests - pure Nix tests that don't require Claude
# Tests verify ralph utility functions work correctly
{
  pkgs,
}:

let
  inherit (pkgs)
    bash
    coreutils
    jq
    runCommandLocal
    ;

  utilScript = ../.. + "/lib/ralph/cmd/util.sh";

  # Import template tests
  templateTests = import ./templates.nix {
    inherit pkgs;
    inherit (pkgs) lib;
  };

in
templateTests
// {
  # Test: validate_json function correctly validates JSON
  util-validate-json =
    runCommandLocal "ralph-util-validate-json"
      {
        nativeBuildInputs = [
          bash
          jq
        ];
      }
      ''
        set -euo pipefail
        source ${utilScript}

        echo "Test: validate_json with valid JSON object..."
        validate_json '{"key": "value"}' "test object" || exit 1

        echo "Test: validate_json with valid JSON array..."
        validate_json '[1, 2, 3]' "test array" || exit 1

        echo "Test: validate_json with invalid JSON..."
        if validate_json 'not json' "invalid" 2>/dev/null; then
          echo "FAIL: Should have rejected invalid JSON"
          exit 1
        fi

        echo "Test: validate_json with empty string..."
        if validate_json "" "empty" 2>/dev/null; then
          echo "FAIL: Should have rejected empty string"
          exit 1
        fi

        echo "PASS: validate_json tests"
        mkdir $out
      '';

  # Test: validate_json_array function correctly validates JSON arrays
  util-validate-json-array =
    runCommandLocal "ralph-util-validate-json-array"
      {
        nativeBuildInputs = [
          bash
          jq
        ];
      }
      ''
        set -euo pipefail
        source ${utilScript}

        echo "Test: validate_json_array with non-empty array..."
        validate_json_array '[{"id": "1"}]' "test array" || exit 1

        echo "Test: validate_json_array with multi-element array..."
        validate_json_array '[1, 2, 3]' "numbers" || exit 1

        echo "Test: validate_json_array rejects object..."
        if validate_json_array '{"key": "value"}' "object" 2>/dev/null; then
          echo "FAIL: Should have rejected object"
          exit 1
        fi

        echo "Test: validate_json_array rejects empty array..."
        if validate_json_array '[]' "empty array" 2>/dev/null; then
          echo "FAIL: Should have rejected empty array"
          exit 1
        fi

        echo "PASS: validate_json_array tests"
        mkdir $out
      '';

  # Test: extract_json function extracts JSON from mixed output
  util-extract-json =
    runCommandLocal "ralph-util-extract-json"
      {
        nativeBuildInputs = [
          bash
          jq
        ];
      }
      ''
            set -euo pipefail
            source ${utilScript}

            echo "Test: extract_json from pure JSON array..."
            result=$(extract_json '[{"id": "beads-001"}]')
            if [ "$result" != '[{"id": "beads-001"}]' ]; then
              echo "FAIL: Expected pure JSON to pass through"
              exit 1
            fi

            echo "Test: extract_json from mixed output with warning prefix..."
            mixed_output="Warning: something happened
        [{\"id\": \"beads-001\"}]"
            result=$(extract_json "$mixed_output")
            expected='[{"id": "beads-001"}]'
            if [ "$result" != "$expected" ]; then
              echo "FAIL: Expected '$expected', got '$result'"
              exit 1
            fi

            echo "Test: extract_json from output with multiple warning lines..."
            multi_warn="⚠ Warning line 1
        ⚠ Warning line 2
        [{\"id\": \"beads-002\", \"title\": \"Test\"}]"
            result=$(extract_json "$multi_warn")
            if ! echo "$result" | jq -e '.[0].id == "beads-002"' >/dev/null; then
              echo "FAIL: Could not extract JSON from multi-warning output"
              exit 1
            fi

            echo "PASS: extract_json tests"
            mkdir $out
      '';

  # Test: strip_implementation_notes removes Implementation Notes section
  util-strip-implementation-notes =
    runCommandLocal "ralph-util-strip-implementation-notes"
      {
        nativeBuildInputs = [
          bash
          coreutils
        ];
      }
      ''
            set -euo pipefail
            source ${utilScript}

            echo "Test: strip section at end of document..."
            input="# Feature Spec

        ## Requirements
        - Requirement 1

        ## Implementation Notes

        > This section is transient

        - Implementation detail"

            result=$(strip_implementation_notes "$input")

            if echo "$result" | grep -q "Implementation Notes"; then
              echo "FAIL: Implementation Notes section should be removed"
              exit 1
            fi

            if ! echo "$result" | grep -q "Requirements"; then
              echo "FAIL: Requirements section should remain"
              exit 1
            fi

            echo "Test: strip section in middle of document..."
            input2="# Feature

        ## Design
        Design content

        ## Implementation Notes
        Transient notes

        ## Success Criteria
        - Criterion 1"

            result2=$(strip_implementation_notes "$input2")

            if echo "$result2" | grep -q "Transient notes"; then
              echo "FAIL: Implementation Notes content should be removed"
              exit 1
            fi

            if ! echo "$result2" | grep -q "Success Criteria"; then
              echo "FAIL: Success Criteria section should remain"
              exit 1
            fi

            if ! echo "$result2" | grep -q "Criterion 1"; then
              echo "FAIL: Success Criteria content should remain"
              exit 1
            fi

            echo "Test: document without Implementation Notes unchanged..."
            input3="# Simple Spec

        ## Requirements
        Just requirements"

            result3=$(strip_implementation_notes "$input3")

            if [ "$result3" != "$input3" ]; then
              echo "FAIL: Document without Implementation Notes should be unchanged"
              exit 1
            fi

            echo "PASS: strip_implementation_notes tests"
            mkdir $out
      '';

  # Test: ralph script syntax validation
  ralph-script-syntax =
    runCommandLocal "ralph-script-syntax"
      {
        nativeBuildInputs = [ bash ];
      }
      ''
        set -euo pipefail

        echo "Checking ralph script syntax..."
        for script in ${../.. + "/lib/ralph/cmd"}/*.sh; do
          echo "  Validating: $script"
          bash -n "$script"
        done

        echo "PASS: All ralph scripts have valid syntax"
        mkdir $out
      '';

  # Test: resolve_partials function resolves {{> partial-name}} markers
  util-resolve-partials =
    runCommandLocal "ralph-util-resolve-partials"
      {
        nativeBuildInputs = [
          bash
          coreutils
        ];
      }
      ''
        set -euo pipefail
        source ${utilScript}

        # Create test partial directory
        mkdir -p partials
        echo "Hello from greeting!" > partials/greeting.md
        echo "Goodbye!" > partials/farewell.md

        echo "Test: resolve single partial..."
        content="Start {{> greeting}} End"
        result=$(resolve_partials "$content" "partials")
        expected="Start Hello from greeting! End"
        if [ "$result" != "$expected" ]; then
          echo "FAIL: Expected '$expected', got '$result'"
          exit 1
        fi

        echo "Test: resolve multiple partials..."
        content2="{{> greeting}} then {{> farewell}}"
        result2=$(resolve_partials "$content2" "partials")
        if ! echo "$result2" | grep -q "Hello from greeting!"; then
          echo "FAIL: greeting partial not resolved"
          exit 1
        fi
        if ! echo "$result2" | grep -q "Goodbye!"; then
          echo "FAIL: farewell partial not resolved"
          exit 1
        fi

        echo "Test: no partials returns unchanged content..."
        content3="No partials here"
        result3=$(resolve_partials "$content3" "partials")
        if [ "$result3" != "$content3" ]; then
          echo "FAIL: Content without partials should be unchanged"
          exit 1
        fi

        echo "Test: missing partial dir returns unchanged content..."
        content4="{{> greeting}}"
        result4=$(resolve_partials "$content4" "nonexistent")
        if [ "$result4" != "$content4" ]; then
          echo "FAIL: Content with missing partial dir should be unchanged"
          exit 1
        fi

        echo "Test: nested/multiline content preserved..."
        cat > partials/complex.md << 'PARTIAL'
        ## Section Header

        - List item 1
        - List item 2

        ```bash
        echo "code block"
        ```
        PARTIAL
        content5="Before {{> complex}} After"
        result5=$(resolve_partials "$content5" "partials")
        if ! echo "$result5" | grep -q "Section Header"; then
          echo "FAIL: Section header not preserved"
          exit 1
        fi
        if ! echo "$result5" | grep -q "List item 1"; then
          echo "FAIL: List items not preserved"
          exit 1
        fi
        if ! echo "$result5" | grep -q 'echo "code block"'; then
          echo "FAIL: Code block not preserved"
          exit 1
        fi
        if ! echo "$result5" | grep -q "Before"; then
          echo "FAIL: Content before partial not preserved"
          exit 1
        fi
        if ! echo "$result5" | grep -q "After"; then
          echo "FAIL: Content after partial not preserved"
          exit 1
        fi

        echo "Test: missing partial file handled gracefully..."
        content6="Start {{> nonexistent-partial}} End"
        # Should warn but not fail - partial reference stays in output
        result6=$(resolve_partials "$content6" "partials" 2>&1)
        if ! echo "$result6" | grep -q "nonexistent-partial"; then
          echo "FAIL: Missing partial should be preserved in output"
          exit 1
        fi

        echo "PASS: resolve_partials tests"
        mkdir $out
      '';

  # Test: ralph-tune help flag works
  # Note: We test the help output by grepping the script directly since help is shown
  # before util.sh is sourced, and our test verifies the script's help content
  tune-help =
    runCommandLocal "ralph-tune-help"
      {
        nativeBuildInputs = [
          bash
          coreutils
        ];
      }
      ''
        set -euo pipefail

        echo "Test: ralph-tune --help content..."

        # Read the script and verify help text content exists
        script="${../.. + "/lib/ralph/cmd/tune.sh"}"

        if grep -q "AI-assisted template editing" "$script"; then
          echo "PASS: Help mentions AI-assisted template editing"
        else
          echo "FAIL: Help missing AI-assisted editing mention"
          exit 1
        fi

        if grep -q "Interactive mode" "$script"; then
          echo "PASS: Help mentions interactive mode"
        else
          echo "FAIL: Help missing interactive mode"
          exit 1
        fi

        if grep -q "Integration mode" "$script"; then
          echo "PASS: Help mentions integration mode"
        else
          echo "FAIL: Help missing integration mode"
          exit 1
        fi

        if grep -q "ralph check" "$script"; then
          echo "PASS: Help mentions ralph check validation"
        else
          echo "FAIL: Help missing ralph check mention"
          exit 1
        fi

        echo "PASS: ralph-tune help tests"
        mkdir $out
      '';

  # Test: ralph-tune mode detection logic
  # Verifies the script properly detects stdin vs no stdin
  tune-mode-detection =
    runCommandLocal "ralph-tune-mode-detection"
      {
        nativeBuildInputs = [
          bash
          coreutils
        ];
      }
      ''
        set -euo pipefail

        echo "Test: ralph-tune mode detection logic..."

        script="${../.. + "/lib/ralph/cmd/tune.sh"}"

        # Verify the script checks for stdin
        if grep -q '\[ ! -t 0 \]' "$script"; then
          echo "PASS: Script checks for stdin terminal status"
        else
          echo "FAIL: Script missing stdin detection"
          exit 1
        fi

        # Verify the script sets MODE based on detection
        if grep -q 'MODE="integration"' "$script" && grep -q 'MODE="interactive"' "$script"; then
          echo "PASS: Script sets integration and interactive modes"
        else
          echo "FAIL: Script missing mode assignment"
          exit 1
        fi

        # Verify empty diff detection
        if grep -q "No diff input received" "$script"; then
          echo "PASS: Script handles empty diff input"
        else
          echo "FAIL: Script missing empty diff handling"
          exit 1
        fi

        echo "PASS: ralph-tune mode detection tests"
        mkdir $out
      '';

  # Test: ralph-tune requires RALPH_TEMPLATE_DIR
  # Verifies the script checks for required environment
  tune-env-validation =
    runCommandLocal "ralph-tune-env-validation"
      {
        nativeBuildInputs = [
          bash
          coreutils
        ];
      }
      ''
        set -euo pipefail

        echo "Test: ralph-tune environment validation..."

        script="${../.. + "/lib/ralph/cmd/tune.sh"}"

        # Verify the script requires RALPH_TEMPLATE_DIR
        if grep -q 'RALPH_TEMPLATE_DIR' "$script"; then
          echo "PASS: Script references RALPH_TEMPLATE_DIR"
        else
          echo "FAIL: Script missing RALPH_TEMPLATE_DIR reference"
          exit 1
        fi

        # Verify error message for missing env
        if grep -q "RALPH_TEMPLATE_DIR not set" "$script"; then
          echo "PASS: Script shows error for missing RALPH_TEMPLATE_DIR"
        else
          echo "FAIL: Script missing RALPH_TEMPLATE_DIR error message"
          exit 1
        fi

        # Verify RALPH_DIR is used
        if grep -q 'RALPH_DIR' "$script"; then
          echo "PASS: Script uses RALPH_DIR"
        else
          echo "FAIL: Script missing RALPH_DIR usage"
          exit 1
        fi

        echo "PASS: ralph-tune env validation tests"
        mkdir $out
      '';

  # Test: ralph-tune prompt building
  # Verifies the script builds appropriate prompts for both modes
  tune-prompt-building =
    runCommandLocal "ralph-tune-prompt-building"
      {
        nativeBuildInputs = [
          bash
          coreutils
        ];
      }
      ''
        set -euo pipefail

        echo "Test: ralph-tune prompt building..."

        script="${../.. + "/lib/ralph/cmd/tune.sh"}"

        # Verify interactive mode prompt content
        if grep -q "build_interactive_prompt" "$script"; then
          echo "PASS: Script has interactive prompt builder"
        else
          echo "FAIL: Script missing interactive prompt builder"
          exit 1
        fi

        # Verify integration mode prompt content
        if grep -q "build_integration_prompt" "$script"; then
          echo "PASS: Script has integration prompt builder"
        else
          echo "FAIL: Script missing integration prompt builder"
          exit 1
        fi

        # Verify template context is built
        if grep -q "build_template_context" "$script"; then
          echo "PASS: Script builds template context"
        else
          echo "FAIL: Script missing template context builder"
          exit 1
        fi

        # Verify ralph check is run after edits
        if grep -q "ralph-check" "$script"; then
          echo "PASS: Script runs ralph-check after edits"
        else
          echo "FAIL: Script missing ralph-check validation"
          exit 1
        fi

        echo "PASS: ralph-tune prompt building tests"
        mkdir $out
      '';

  # Test: ralph sync --diff help flag content (formerly ralph-diff)
  diff-help =
    runCommandLocal "ralph-sync-diff-help"
      {
        nativeBuildInputs = [
          bash
          coreutils
        ];
      }
      ''
        set -euo pipefail

        echo "Test: ralph sync --diff help content..."

        script="${../.. + "/lib/ralph/cmd/sync.sh"}"

        if grep -q "Show local template changes vs packaged" "$script"; then
          echo "PASS: Help mentions showing local template changes"
        else
          echo "FAIL: Help missing description"
          exit 1
        fi

        if grep -q "plan-new" "$script" && grep -q "plan-update" "$script"; then
          echo "PASS: Help lists plan variants"
        else
          echo "FAIL: Help missing plan variants"
          exit 1
        fi

        if grep -q "todo-new" "$script" && grep -q "todo-update" "$script"; then
          echo "PASS: Help lists todo variants"
        else
          echo "FAIL: Help missing todo variants"
          exit 1
        fi

        if grep -q "context-pinning" "$script"; then
          echo "PASS: Help lists partials"
        else
          echo "FAIL: Help missing partials"
          exit 1
        fi

        if grep -q "ralph tune" "$script"; then
          echo "PASS: Help mentions piping to ralph tune"
        else
          echo "FAIL: Help missing ralph tune integration"
          exit 1
        fi

        echo "PASS: ralph sync --diff help tests"
        mkdir $out
      '';

  # Test: ralph sync --diff template list includes all variants (formerly ralph-diff)
  diff-template-list =
    runCommandLocal "ralph-sync-diff-template-list"
      {
        nativeBuildInputs = [
          bash
          coreutils
        ];
      }
      ''
        set -euo pipefail

        echo "Test: ralph sync --diff template list..."

        script="${../.. + "/lib/ralph/cmd/sync.sh"}"

        # Check all template variants are listed
        for template in plan-new plan-update todo-new todo-update run; do
          if grep -q "\"$template\"" "$script"; then
            echo "PASS: Template '$template' in list"
          else
            echo "FAIL: Template '$template' missing from list"
            exit 1
          fi
        done

        # Check all partials are listed
        for partial in context-pinning exit-signals spec-header; do
          if grep -q "\"$partial\"" "$script"; then
            echo "PASS: Partial '$partial' in list"
          else
            echo "FAIL: Partial '$partial' missing from list"
            exit 1
          fi
        done

        echo "PASS: ralph sync --diff template list tests"
        mkdir $out
      '';

  # Test: ralph sync --diff partial handling (formerly ralph-diff)
  diff-partial-handling =
    runCommandLocal "ralph-sync-diff-partial-handling"
      {
        nativeBuildInputs = [
          bash
          coreutils
        ];
      }
      ''
        set -euo pipefail

        echo "Test: ralph sync --diff partial handling..."

        script="${../.. + "/lib/ralph/cmd/sync.sh"}"

        # Check diff_partial_file function exists
        if grep -q "diff_partial_file()" "$script"; then
          echo "PASS: Script has diff_partial_file function"
        else
          echo "FAIL: Script missing diff_partial_file function"
          exit 1
        fi

        # Check partial directory path is correct
        if grep -q 'template/partial/' "$script"; then
          echo "PASS: Script uses correct partial directory path"
        else
          echo "FAIL: Script missing partial directory path"
          exit 1
        fi

        # Check filter_partial handling for specific partial diff
        if grep -q 'filter_partial' "$script"; then
          echo "PASS: Script handles filtered partial diffing"
        else
          echo "FAIL: Script missing filter_partial handling"
          exit 1
        fi

        echo "PASS: ralph sync --diff partial handling tests"
        mkdir $out
      '';

  # Test: ralph sync --diff requires RALPH_TEMPLATE_DIR (formerly ralph-diff)
  diff-env-validation =
    runCommandLocal "ralph-sync-diff-env-validation"
      {
        nativeBuildInputs = [
          bash
          coreutils
        ];
      }
      ''
        set -euo pipefail

        echo "Test: ralph sync --diff environment validation..."

        script="${../.. + "/lib/ralph/cmd/sync.sh"}"

        # Verify the script requires RALPH_TEMPLATE_DIR
        if grep -q 'RALPH_TEMPLATE_DIR' "$script"; then
          echo "PASS: Script references RALPH_TEMPLATE_DIR"
        else
          echo "FAIL: Script missing RALPH_TEMPLATE_DIR reference"
          exit 1
        fi

        # Verify error message for missing env
        if grep -q "RALPH_TEMPLATE_DIR not set" "$script"; then
          echo "PASS: Script shows error for missing RALPH_TEMPLATE_DIR"
        else
          echo "FAIL: Script missing RALPH_TEMPLATE_DIR error message"
          exit 1
        fi

        # Verify RALPH_DIR is used
        if grep -q 'RALPH_DIR' "$script"; then
          echo "PASS: Script uses RALPH_DIR"
        else
          echo "FAIL: Script missing RALPH_DIR usage"
          exit 1
        fi

        echo "PASS: ralph sync --diff env validation tests"
        mkdir $out
      '';

  # Test: ralph sync --diff output format is pipe-friendly (formerly ralph-diff)
  diff-output-format =
    runCommandLocal "ralph-sync-diff-output-format"
      {
        nativeBuildInputs = [
          bash
          coreutils
        ];
      }
      ''
        set -euo pipefail

        echo "Test: ralph sync --diff output format..."

        script="${../.. + "/lib/ralph/cmd/sync.sh"}"

        # Check for markdown-friendly output headers
        if grep -q '# Local Template Changes' "$script"; then
          echo "PASS: Script outputs markdown header"
        else
          echo "FAIL: Script missing markdown header"
          exit 1
        fi

        # Check for code fence around diffs (escaped backticks in shell)
        if grep -qF '\`\`\`diff' "$script"; then
          echo "PASS: Script uses diff code fence"
        else
          echo "FAIL: Script missing diff code fence"
          exit 1
        fi

        # Check for TTY-only hint
        if grep -q '\[ -t 1 \]' "$script"; then
          echo "PASS: Script checks for TTY before hint"
        else
          echo "FAIL: Script missing TTY check"
          exit 1
        fi

        echo "PASS: ralph sync --diff output format tests"
        mkdir $out
      '';

  # Test: ralph sync --diff validation logic for templates and partials (formerly ralph-diff)
  diff-validation-logic =
    runCommandLocal "ralph-sync-diff-validation-logic"
      {
        nativeBuildInputs = [
          bash
          coreutils
        ];
      }
      ''
        set -euo pipefail

        echo "Test: ralph sync --diff validation logic..."

        script="${../.. + "/lib/ralph/cmd/sync.sh"}"

        # Check that both templates and partials are validated
        if grep -q 'valid_template' "$script" && grep -q 'valid_partial' "$script"; then
          echo "PASS: Script validates both templates and partials"
        else
          echo "FAIL: Script missing template or partial validation"
          exit 1
        fi

        # Check error message includes both valid options
        if grep -q 'Valid templates:' "$script" && grep -q 'Valid partials:' "$script"; then
          echo "PASS: Error message shows valid options"
        else
          echo "FAIL: Error message missing valid options"
          exit 1
        fi

        # Check that specific template filters out partials
        if grep -q 'partials_to_diff=()' "$script"; then
          echo "PASS: Script clears partials when template is specified"
        else
          echo "FAIL: Script doesn't clear partials for template filter"
          exit 1
        fi

        # Check that specific partial filters out templates
        if grep -q 'templates_to_diff=()' "$script"; then
          echo "PASS: Script clears templates when partial is specified"
        else
          echo "FAIL: Script doesn't clear templates for partial filter"
          exit 1
        fi

        echo "PASS: ralph sync --diff validation logic tests"
        mkdir $out
      '';

  # Test: ralph-check help flag content
  check-help =
    runCommandLocal "ralph-check-help"
      {
        nativeBuildInputs = [
          bash
          coreutils
        ];
      }
      ''
        set -euo pipefail

        echo "Test: ralph-check --help content..."

        script="${../.. + "/lib/ralph/cmd/check.sh"}"

        if grep -q "Validates all ralph templates" "$script"; then
          echo "PASS: Help mentions template validation"
        else
          echo "FAIL: Help missing template validation"
          exit 1
        fi

        if grep -q "Partial files exist" "$script"; then
          echo "PASS: Help mentions partial file checks"
        else
          echo "FAIL: Help missing partial file checks"
          exit 1
        fi

        if grep -q "Body files parse correctly" "$script"; then
          echo "PASS: Help mentions body file parsing"
        else
          echo "FAIL: Help missing body file parsing"
          exit 1
        fi

        if grep -q "No syntax errors in Nix expressions" "$script"; then
          echo "PASS: Help mentions Nix expression syntax"
        else
          echo "FAIL: Help missing Nix expression syntax"
          exit 1
        fi

        if grep -q "Dry-run render with dummy values" "$script"; then
          echo "PASS: Help mentions dry-run rendering"
        else
          echo "FAIL: Help missing dry-run rendering"
          exit 1
        fi

        if grep -q "Exit codes:" "$script"; then
          echo "PASS: Help documents exit codes"
        else
          echo "FAIL: Help missing exit code documentation"
          exit 1
        fi

        echo "PASS: ralph-check help tests"
        mkdir $out
      '';

  # Test: ralph-check resolves a template directory (RALPH_TEMPLATE_DIR or
  # $RALPH_DIR/template fallback) and errors clearly when neither exists
  check-env-validation =
    runCommandLocal "ralph-check-env-validation"
      {
        nativeBuildInputs = [
          bash
          coreutils
        ];
      }
      ''
        set -euo pipefail

        echo "Test: ralph-check environment validation..."

        script="${../.. + "/lib/ralph/cmd/check.sh"}"

        # Verify the script references RALPH_TEMPLATE_DIR
        if grep -q 'RALPH_TEMPLATE_DIR' "$script"; then
          echo "PASS: Script references RALPH_TEMPLATE_DIR"
        else
          echo "FAIL: Script missing RALPH_TEMPLATE_DIR reference"
          exit 1
        fi

        # Verify the script falls back to $RALPH_DIR/template
        if grep -q '\$RALPH_DIR/template' "$script"; then
          echo "PASS: Script falls back to \$RALPH_DIR/template"
        else
          echo "FAIL: Script missing \$RALPH_DIR/template fallback"
          exit 1
        fi

        # Verify error message when no template dir is found
        if grep -q "No template directory found" "$script"; then
          echo "PASS: Script shows error when no template dir is found"
        else
          echo "FAIL: Script missing no-template-dir error message"
          exit 1
        fi

        # Verify nix develop shell instruction
        if grep -q "nix develop" "$script"; then
          echo "PASS: Script mentions nix develop"
        else
          echo "FAIL: Script missing nix develop instruction"
          exit 1
        fi

        echo "PASS: ralph-check env validation tests"
        mkdir $out
      '';

  # Test: ralph-check validates all template types
  check-template-types =
    runCommandLocal "ralph-check-template-types"
      {
        nativeBuildInputs = [
          bash
          coreutils
        ];
      }
      ''
        set -euo pipefail

        echo "Test: ralph-check template types..."

        script="${../.. + "/lib/ralph/cmd/check.sh"}"

        # Check that all body files are listed
        for template in plan-new plan-update todo-new todo-update run; do
          if grep -q "\"$template.md\"" "$script" || grep -q "'$template.md'" "$script"; then
            echo "PASS: Template '$template.md' in body files list"
          else
            echo "FAIL: Template '$template.md' missing from body files"
            exit 1
          fi
        done

        # Check that all expected partials are listed
        for partial in context-pinning exit-signals spec-header; do
          if grep -q "$partial.md" "$script"; then
            echo "PASS: Partial '$partial.md' in expected partials"
          else
            echo "FAIL: Partial '$partial.md' missing from expected partials"
            exit 1
          fi
        done

        echo "PASS: ralph-check template types tests"
        mkdir $out
      '';

  # Test: ralph-check validates Nix expressions
  check-nix-validation =
    runCommandLocal "ralph-check-nix-validation"
      {
        nativeBuildInputs = [
          bash
          coreutils
        ];
      }
      ''
        set -euo pipefail

        echo "Test: ralph-check Nix validation..."

        script="${../.. + "/lib/ralph/cmd/check.sh"}"

        # Check that default.nix is validated
        if grep -q 'default.nix' "$script"; then
          echo "PASS: Script validates default.nix"
        else
          echo "FAIL: Script doesn't validate default.nix"
          exit 1
        fi

        # Check that nix-instantiate --parse is used for syntax checking
        if grep -q 'nix-instantiate --parse' "$script"; then
          echo "PASS: Script uses nix-instantiate for syntax checking"
        else
          echo "FAIL: Script doesn't use nix-instantiate for syntax"
          exit 1
        fi

        # Check that nix eval is used for evaluation checking
        if grep -q 'nix eval' "$script"; then
          echo "PASS: Script uses nix eval for evaluation checking"
        else
          echo "FAIL: Script doesn't use nix eval"
          exit 1
        fi

        # Check that config.nix is validated if present
        if grep -q 'config.nix' "$script"; then
          echo "PASS: Script checks config.nix"
        else
          echo "FAIL: Script doesn't check config.nix"
          exit 1
        fi

        echo "PASS: ralph-check Nix validation tests"
        mkdir $out
      '';

  # Test: ralph-check validates partial references
  check-partial-references =
    runCommandLocal "ralph-check-partial-references"
      {
        nativeBuildInputs = [
          bash
          coreutils
        ];
      }
      ''
        set -euo pipefail

        echo "Test: ralph-check partial references..."

        script="${../.. + "/lib/ralph/cmd/check.sh"}"

        # Check that partial references pattern is used (use extended regex for +)
        if grep -E -q '\{\{> [a-z-]+\}\}' "$script"; then
          echo "PASS: Script extracts partial references"
        else
          echo "FAIL: Script doesn't extract partial references"
          exit 1
        fi

        # Check that PARTIAL_DIR is used correctly
        if grep -q 'PARTIAL_DIR' "$script"; then
          echo "PASS: Script uses PARTIAL_DIR for partial validation"
        else
          echo "FAIL: Script doesn't define PARTIAL_DIR"
          exit 1
        fi

        echo "PASS: ralph-check partial references tests"
        mkdir $out
      '';

  # Test: ralph-check performs dry-run rendering
  check-dry-run-render =
    runCommandLocal "ralph-check-dry-run-render"
      {
        nativeBuildInputs = [
          bash
          coreutils
        ];
      }
      ''
        set -euo pipefail

        echo "Test: ralph-check dry-run rendering..."

        script="${../.. + "/lib/ralph/cmd/check.sh"}"

        # Check that dummy values are used for rendering
        if grep -q 'allDummyVars' "$script"; then
          echo "PASS: Script uses allDummyVars for dry-run"
        else
          echo "FAIL: Script doesn't use allDummyVars"
          exit 1
        fi

        # Check that template.render is called
        if grep -q 'template.render' "$script"; then
          echo "PASS: Script calls template.render"
        else
          echo "FAIL: Script doesn't call template.render"
          exit 1
        fi

        # Check that script uses variableDefinitions for variable metadata
        # (Variables are dynamically read from Nix definitions, not hardcoded)
        if grep -q 'variableDefinitions' "$script"; then
          echo "PASS: Script reads from variableDefinitions"
        else
          echo "FAIL: Script doesn't read from variableDefinitions"
          exit 1
        fi

        # Check that script generates dummy values dynamically
        if grep -q 'makeDummy' "$script"; then
          echo "PASS: Script uses makeDummy to generate dummy values"
        else
          echo "FAIL: Script doesn't use makeDummy"
          exit 1
        fi

        echo "PASS: ralph-check dry-run rendering tests"
        mkdir $out
      '';

  # Test: ralph-check exit codes are correct
  check-exit-codes =
    runCommandLocal "ralph-check-exit-codes"
      {
        nativeBuildInputs = [
          bash
          coreutils
        ];
      }
      ''
        set -euo pipefail

        echo "Test: ralph-check exit codes..."

        script="${../.. + "/lib/ralph/cmd/check.sh"}"

        # Check that exit 0 is used for success
        if grep -q 'exit 0' "$script"; then
          echo "PASS: Script exits 0 on success"
        else
          echo "FAIL: Script doesn't exit 0 on success"
          exit 1
        fi

        # Check that exit 1 is used for errors
        if grep -q 'exit 1' "$script"; then
          echo "PASS: Script exits 1 on errors"
        else
          echo "FAIL: Script doesn't exit 1 on errors"
          exit 1
        fi

        # Check that ERRORS array is used for collecting errors
        if grep -q 'ERRORS=(' "$script" && grep -q '#ERRORS\[@\]' "$script"; then
          echo "PASS: Script collects errors in ERRORS array"
        else
          echo "FAIL: Script doesn't properly use ERRORS array"
          exit 1
        fi

        echo "PASS: ralph-check exit codes tests"
        mkdir $out
      '';

  # Test: ralph-check validates variable declarations
  check-variable-declarations =
    runCommandLocal "ralph-check-variable-declarations"
      {
        nativeBuildInputs = [
          bash
          coreutils
        ];
      }
      ''
        set -euo pipefail

        echo "Test: ralph-check variable declarations..."

        script="${../.. + "/lib/ralph/cmd/check.sh"}"

        # Check that variable declaration checking exists
        if grep -q 'Checking variable declarations' "$script"; then
          echo "PASS: Script checks variable declarations"
        else
          echo "FAIL: Script doesn't check variable declarations"
          exit 1
        fi

        # Check that undeclared variables are detected
        if grep -q 'undeclared' "$script"; then
          echo "PASS: Script detects undeclared variables"
        else
          echo "FAIL: Script doesn't detect undeclared variables"
          exit 1
        fi

        echo "PASS: ralph-check variable declarations tests"
        mkdir $out
      '';

  # Test: parse_spec_annotations counts verify/judge/none correctly
  util-parse-spec-annotations-counts =
    runCommandLocal "ralph-util-parse-spec-annotations-counts"
      {
        nativeBuildInputs = [
          bash
          coreutils
        ];
      }
      ''
        set -euo pipefail
        source ${utilScript}

        # Create test spec file with mixed annotations
        cat > spec.md << 'SPEC'
        # Test Feature

        ## Success Criteria

        - [ ] Notification appears within 2s
          [verify](tests/notify-test.sh::test_notification_timing)
        - [ ] Clear visibility into current state
          [judge](tests/judges/notify.sh::test_clear_visibility)
        - [ ] Works on both Linux and macOS
        - [x] Basic functionality works
          [verify](tests/basic.sh)
        - [ ] Handles edge cases gracefully
          [judge](tests/judges/edge.sh::test_edge_cases)
        - [ ] No security vulnerabilities

        ## Out of Scope
        SPEC

        echo "Test: parse_spec_annotations counts annotations correctly..."
        output=$(parse_spec_annotations spec.md)

        # Count by type
        verify_count=$(echo "$output" | awk -F'\t' '$2 == "verify"' | wc -l)
        judge_count=$(echo "$output" | awk -F'\t' '$2 == "judge"' | wc -l)
        none_count=$(echo "$output" | awk -F'\t' '$2 == "none"' | wc -l)
        total=$(echo "$output" | wc -l)

        if [ "$verify_count" -ne 2 ]; then
          echo "FAIL: Expected 2 verify, got $verify_count"
          exit 1
        fi
        echo "PASS: 2 verify annotations"

        if [ "$judge_count" -ne 2 ]; then
          echo "FAIL: Expected 2 judge, got $judge_count"
          exit 1
        fi
        echo "PASS: 2 judge annotations"

        if [ "$none_count" -ne 2 ]; then
          echo "FAIL: Expected 2 unannotated, got $none_count"
          exit 1
        fi
        echo "PASS: 2 unannotated criteria"

        if [ "$total" -ne 6 ]; then
          echo "FAIL: Expected 6 total, got $total"
          exit 1
        fi
        echo "PASS: 6 total criteria"

        # Verify checked status is captured
        checked=$(echo "$output" | awk -F'\t' '$5 == "x"' | wc -l)
        if [ "$checked" -ne 1 ]; then
          echo "FAIL: Expected 1 checked, got $checked"
          exit 1
        fi
        echo "PASS: 1 checked criterion"

        echo "PASS: parse_spec_annotations counts tests"
        mkdir $out
      '';

  # Test: parse_spec_annotations handles edge cases
  util-parse-spec-annotations-edge-cases =
    runCommandLocal "ralph-util-parse-spec-annotations-edge-cases"
      {
        nativeBuildInputs = [
          bash
          coreutils
        ];
      }
      ''
        set -euo pipefail
        source ${utilScript}

        echo "Test: no Success Criteria section..."
        cat > no-criteria.md << 'SPEC'
        # Feature
        ## Requirements
        Some requirements.
        SPEC

        if parse_spec_annotations no-criteria.md >/dev/null 2>&1; then
          echo "FAIL: Should return error when no Success Criteria"
          exit 1
        fi
        echo "PASS: Returns error for no Success Criteria"

        echo "Test: nonexistent file..."
        if parse_spec_annotations nonexistent.md >/dev/null 2>&1; then
          echo "FAIL: Should return error for nonexistent file"
          exit 1
        fi
        echo "PASS: Returns error for nonexistent file"

        echo "Test: empty Success Criteria..."
        cat > empty.md << 'SPEC'
        # Feature
        ## Success Criteria
        ## Out of Scope
        SPEC

        if parse_spec_annotations empty.md >/dev/null 2>&1; then
          echo "FAIL: Should return error for empty Success Criteria"
          exit 1
        fi
        echo "PASS: Returns error for empty Success Criteria"

        echo "Test: all unannotated..."
        cat > unannotated.md << 'SPEC'
        # Feature
        ## Success Criteria
        - [ ] First criterion
        - [ ] Second criterion
        - [ ] Third criterion
        ## Out of Scope
        SPEC

        output=$(parse_spec_annotations unannotated.md)
        total=$(echo "$output" | wc -l)
        none_count=$(echo "$output" | awk -F'\t' '$2 == "none"' | wc -l)

        if [ "$total" -ne 3 ]; then
          echo "FAIL: Expected 3 total, got $total"
          exit 1
        fi
        if [ "$none_count" -ne 3 ]; then
          echo "FAIL: Expected 3 unannotated, got $none_count"
          exit 1
        fi
        echo "PASS: All unannotated criteria parsed correctly"

        echo "Test: criteria at end of file (no closing heading)..."
        cat > eof.md << 'SPEC'
        # Feature
        ## Success Criteria
        - [ ] First criterion
          [verify](tests/first.sh::test_first)
        - [ ] Last criterion at EOF
        SPEC

        output=$(parse_spec_annotations eof.md)
        total=$(echo "$output" | wc -l)
        if [ "$total" -ne 2 ]; then
          echo "FAIL: Expected 2 criteria at EOF, got $total"
          exit 1
        fi
        echo "PASS: Handles criteria at EOF"

        echo "Test: malformed annotation treated as unannotated..."
        cat > malformed.md << 'SPEC'
        # Feature
        ## Success Criteria
        - [ ] Has valid verify
          [verify](tests/foo.sh::bar)
        - [ ] Has invalid annotation
          [notaverb](something)
        - [ ] Normal criterion
        SPEC

        output=$(parse_spec_annotations malformed.md)
        total=$(echo "$output" | wc -l)
        if [ "$total" -ne 3 ]; then
          echo "FAIL: Expected 3 criteria with malformed, got $total"
          exit 1
        fi
        line2=$(echo "$output" | sed -n '2p')
        if ! echo "$line2" | grep -q 'none'; then
          echo "FAIL: Malformed annotation should be 'none': $line2"
          exit 1
        fi
        echo "PASS: Malformed annotation treated as unannotated"

        echo "PASS: parse_spec_annotations edge case tests"
        mkdir $out
      '';

  # Test: parse_annotation_link function
  util-parse-annotation-link =
    runCommandLocal "ralph-util-parse-annotation-link"
      {
        nativeBuildInputs = [
          bash
          coreutils
        ];
      }
      ''
        set -euo pipefail
        source ${utilScript}

        echo "Test: path::function format..."
        output=$(parse_annotation_link "tests/notify-test.sh::test_notification_timing")
        file_path=$(echo "$output" | sed -n '1p')
        function_name=$(echo "$output" | sed -n '2p')

        if [ "$file_path" != "tests/notify-test.sh" ]; then
          echo "FAIL: Expected file path 'tests/notify-test.sh', got '$file_path'"
          exit 1
        fi
        if [ "$function_name" != "test_notification_timing" ]; then
          echo "FAIL: Expected function 'test_notification_timing', got '$function_name'"
          exit 1
        fi
        echo "PASS: path::function parsed correctly"

        echo "Test: path-only format..."
        output=$(parse_annotation_link "tests/basic.sh")
        file_path=$(echo "$output" | sed -n '1p')
        function_name=$(echo "$output" | sed -n '2p')

        if [ "$file_path" != "tests/basic.sh" ]; then
          echo "FAIL: Expected file path 'tests/basic.sh', got '$file_path'"
          exit 1
        fi
        if [ -n "$function_name" ]; then
          echo "FAIL: Expected empty function, got '$function_name'"
          exit 1
        fi
        echo "PASS: path-only parsed correctly"

        echo "Test: path#function format (new-style)..."
        output=$(parse_annotation_link "tests/notify-test.sh#test_notification_timing")
        file_path=$(echo "$output" | sed -n '1p')
        function_name=$(echo "$output" | sed -n '2p')

        if [ "$file_path" != "tests/notify-test.sh" ]; then
          echo "FAIL: Expected file path 'tests/notify-test.sh', got '$file_path'"
          exit 1
        fi
        if [ "$function_name" != "test_notification_timing" ]; then
          echo "FAIL: Expected function 'test_notification_timing', got '$function_name'"
          exit 1
        fi
        echo "PASS: path#function parsed correctly"

        echo "Test: spec-relative path resolution..."
        output=$(parse_annotation_link "../tests/notify-test.sh#test_notification_timing" "specs")
        file_path=$(echo "$output" | sed -n '1p')
        function_name=$(echo "$output" | sed -n '2p')

        if [ "$file_path" != "tests/notify-test.sh" ]; then
          echo "FAIL: Expected resolved path 'tests/notify-test.sh', got '$file_path'"
          exit 1
        fi
        if [ "$function_name" != "test_notification_timing" ]; then
          echo "FAIL: Expected function 'test_notification_timing', got '$function_name'"
          exit 1
        fi
        echo "PASS: spec-relative path resolved correctly"

        echo "Test: empty input returns error..."
        if parse_annotation_link "" 2>/dev/null; then
          echo "FAIL: Should return error for empty input"
          exit 1
        fi
        echo "PASS: Empty input returns error"

        echo "PASS: parse_annotation_link tests"
        mkdir $out
      '';

  # Note: spec.sh syntax is checked by the ralph-script-syntax test above
  # which validates all *.sh files in lib/ralph/cmd/.
}
