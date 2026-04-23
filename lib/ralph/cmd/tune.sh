#!/usr/bin/env bash
set -euo pipefail

# ralph tune
# Template editing with AI assistance
#
# Two modes:
# - Interactive mode (no stdin): AI-driven interview asking what to change
# - Integration mode (stdin with diff): Analyzes diff, interviews about each change
#
# Both modes run ralph check after edits to validate templates.

# Load shared helpers
# shellcheck source=util.sh
source "$(dirname "$0")/util.sh"

RALPH_DIR="${RALPH_DIR:-.wrapix/ralph}"

# Template directory: use RALPH_TEMPLATE_DIR if set and exists
if [ -n "${RALPH_TEMPLATE_DIR:-}" ] && [ -d "$RALPH_TEMPLATE_DIR" ]; then
  PACKAGED_DIR="$RALPH_TEMPLATE_DIR"
else
  PACKAGED_DIR=""
fi

# Source templates: if lib/ralph/template exists, we're in the ralph source repo
# and should edit there instead of .wrapix/ralph
SOURCE_TEMPLATE_DIR=""
if [ -d "lib/ralph/template" ]; then
  SOURCE_TEMPLATE_DIR="lib/ralph/template"
fi

show_usage() {
  echo "Usage: ralph tune"
  echo "       ralph diff | ralph tune"
  echo ""
  echo "AI-assisted template editing with two modes:"
  echo ""
  echo "Interactive mode (no stdin):"
  echo "  Asks what you want to change, analyzes templates, makes edits."
  echo "  Example: ralph tune"
  echo ""
  echo "Integration mode (stdin with diff):"
  echo "  Analyzes diff output, interviews about each change,"
  echo "  asks where it should go (keep, move to partial, new partial)."
  echo "  Example: ralph diff | ralph tune"
  echo ""
  echo "Both modes run 'ralph check' after edits to validate templates."
  echo ""
  echo "Environment:"
  echo "  RALPH_DIR           Local template directory (default: .wrapix/ralph)"
  echo "  RALPH_TEMPLATE_DIR  Packaged template directory (from nix develop)"
}

# Check for help flag
if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
  show_usage
  exit 0
fi

# Validate RALPH_TEMPLATE_DIR is set
if [ -z "$PACKAGED_DIR" ]; then
  error "RALPH_TEMPLATE_DIR not set or directory doesn't exist.

To fix this, do one of the following:
  - Run 'ralph sync' to fetch templates from GitHub
  - Set RALPH_TEMPLATE_DIR to point to an existing template directory

Current value: ${RALPH_TEMPLATE_DIR:-<not set>}"
fi

# Validate local ralph directory exists
if [ ! -d "$RALPH_DIR" ]; then
  error "No local templates found at $RALPH_DIR

Run 'ralph plan <label>' first to initialize local templates."
fi

# Detect mode based on stdin
DIFF_INPUT=""
if [ ! -t 0 ]; then
  # Stdin is not a terminal - integration mode
  DIFF_INPUT=$(cat)
  if [ -z "$DIFF_INPUT" ]; then
    echo "No diff input received. Run 'ralph diff' to see changes, or use interactive mode."
    exit 0
  fi
  MODE="integration"
else
  # Stdin is a terminal - interactive mode
  MODE="interactive"
fi

debug "Mode: $MODE"

# Build context about available templates
build_template_context() {
  local context=""

  context+="## Template Structure

Local templates: \`$RALPH_DIR\`
Packaged templates: \`$PACKAGED_DIR\`

### Main Templates

| Template | Purpose |
|----------|---------|
| plan-new.md | New feature spec interview |
| plan-update.md | Update existing spec |
| todo-new.md | Issue creation from new spec |
| todo-update.md | Add tasks to existing spec |
| run.md | Single-issue implementation |
| config.nix | Project configuration |

### Partials (in partial/)

| Partial | Purpose |
|---------|---------|
| context-pinning.md | Project context loading |
| exit-signals.md | Exit signal format |
| spec-header.md | Label and spec path block |

### Placeholder Variables

| Variable | Description |
|----------|-------------|
| \`{{LABEL}}\` | Feature label |
| \`{{SPEC_PATH}}\` | Path to spec file |
| \`{{PINNED_CONTEXT}}\` | Content from pinnedContext file |
| \`{{SPEC_CONTENT}}\` | Full spec content |
| \`{{ISSUE_ID}}\` | Beads issue ID |
| \`{{TITLE}}\` | Issue title |
| \`{{DESCRIPTION}}\` | Issue description |
| \`{{> partial-name}}\` | Include partial content |

"

  # List current template files
  context+="### Current Local Templates
\`\`\`
"
  for f in "$RALPH_DIR"/*.md "$RALPH_DIR"/*.nix; do
    [ -f "$f" ] && context+="$(basename "$f")
"
  done
  context+="\`\`\`

"

  # List partials if they exist
  if [ -d "$RALPH_DIR/partial" ]; then
    context+="### Current Local Partials
\`\`\`
"
    for f in "$RALPH_DIR/partial"/*.md; do
      [ -f "$f" ] && context+="$(basename "$f")
"
    done
    context+="\`\`\`

"
  fi

  echo "$context"
}

# Build the interactive mode prompt
build_interactive_prompt() {
  local template_context
  template_context=$(build_template_context)

  cat << EOF
# Template Tuning - Interactive Mode

You are helping tune ralph templates to improve AI-assisted development workflows.

$template_context

## Your Task

1. **Ask what to change**: Find out what the user wants to improve in their templates
2. **Analyze templates**: Read the relevant template files to understand current content
3. **Propose changes**: Suggest specific edits with rationale
4. **Make edits**: Apply changes using the Edit tool
5. **Validate**: Run \`ralph check\` to ensure templates are valid

## Guidelines

- Ask clarifying questions if the request is ambiguous
- Show the user what you're changing before making edits
- Consider whether content should go in a main template or a partial
- New shared content that appears in multiple templates should become a partial
- Preserve existing placeholder variables ({{LABEL}}, {{SPEC_PATH}}, etc.)
- After edits, always run \`ralph check\` to validate

## File Locations

$(if [ -n "$SOURCE_TEMPLATE_DIR" ]; then
  echo "**IMPORTANT: You are in the ralph source repo.**"
  echo ""
  echo "- Source templates: \`$SOURCE_TEMPLATE_DIR\` ← EDIT THESE (tracked in git)"
  echo "- Local overrides: \`$RALPH_DIR\` (gitignored, do NOT edit)"
  echo "- Packaged templates: \`$PACKAGED_DIR\` (read-only, built from source)"
else
  echo "- Local templates: \`$RALPH_DIR\`"
  echo "- Packaged templates: \`$PACKAGED_DIR\` (read-only reference)"
fi)

Start by asking the user what they'd like to change about their templates.
EOF
}

# Build the integration mode prompt
build_integration_prompt() {
  local diff_content="$1"
  local template_context
  template_context=$(build_template_context)

  cat << EOF
# Template Tuning - Integration Mode

You are helping integrate template changes from a diff into the user's local templates.

$template_context

## Diff to Process

The following diff shows changes between packaged templates and local templates:

$diff_content

## Your Task

For each change in the diff:

1. **Analyze the change**: Understand what was added, removed, or modified
2. **Interview the user**: Ask where this change should go:
   - **Keep in place**: The change stays in the current template file
   - **Move to existing partial**: The change should move to an existing partial
   - **Create new partial**: The change should become a new partial file
3. **Apply the decision**: Make the appropriate edits
4. **Continue**: Process the next change until all are handled

## Interview Format

For each change, show:
\`\`\`
Change N/M: <template-name> lines X-Y
<diff excerpt>

Where should this go?
  1. Keep in <template-name>
  2. Move to partial/<existing-partial>
  3. Create new partial
\`\`\`

Wait for user input before proceeding.

## Guidelines

- Process changes one at a time, asking about each
- Show the diff context so the user understands the change
- For partial creation, suggest a descriptive name
- Preserve existing placeholder variables
- After ALL changes are processed, run \`ralph check\` to validate

## File Locations

$(if [ -n "$SOURCE_TEMPLATE_DIR" ]; then
  echo "**IMPORTANT: You are in the ralph source repo.**"
  echo ""
  echo "- Source templates: \`$SOURCE_TEMPLATE_DIR\` ← EDIT THESE (tracked in git)"
  echo "- Local overrides: \`$RALPH_DIR\` (gitignored, do NOT edit)"
  echo "- Packaged templates: \`$PACKAGED_DIR\` (read-only, built from source)"
else
  echo "- Local templates: \`$RALPH_DIR\`"
  echo "- Packaged templates: \`$PACKAGED_DIR\` (read-only reference)"
fi)

Start by analyzing the diff and processing the first change.
EOF
}

# Run interactive Claude session
echo "Starting ralph tune ($MODE mode)..."
echo ""

if [ "$MODE" = "interactive" ]; then
  PROMPT_CONTENT=$(build_interactive_prompt)
else
  PROMPT_CONTENT=$(build_integration_prompt "$DIFF_INPUT")
fi

# Pass directly — do not export (environ bloat breaks child execs, wx-gaasw).
run_claude_interactive "$PROMPT_CONTENT"

# After Claude session, run validation
echo ""
echo "Running template validation..."
echo ""

if ralph-check; then
  echo ""
  echo "Templates are valid."
else
  echo ""
  echo "Template validation found issues. Please review and fix."
  exit 1
fi
