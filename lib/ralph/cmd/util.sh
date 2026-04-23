#!/usr/bin/env bash
# Shared helper functions for ralph scripts
# Source this file: source "$(dirname "$0")/lib.sh"
#
# SH-6 convention: util.sh is the display/lookup layer for ralph commands.
# `jq ... 2>/dev/null || <fallback>` sites below are best-effort lookups
# against state JSON, bd output, or claude log files: failures fall back to
# empty/default values so the calling command can continue rather than abort.
# Precondition checks (e.g. `[ -f "$state_file" ]`) are preferred where the
# caller can reasonably expect the input to exist; the remaining suppressed
# sites tolerate races, partial writes, and optional fields.

# Debug mode: set RALPH_DEBUG=1 to see verbose output
RALPH_DEBUG="${RALPH_DEBUG:-0}"

# Colors for output (disabled if not a tty)
if [ -t 2 ]; then
  RED='\033[0;31m'
  YELLOW='\033[0;33m'
  CYAN='\033[0;36m'
  NC='\033[0m' # No Color
else
  RED=''
  YELLOW=''
  CYAN=''
  NC=''
fi

# Debug log - only prints when RALPH_DEBUG=1
debug() {
  if [ "$RALPH_DEBUG" = "1" ]; then
    echo -e "${CYAN}[DEBUG]${NC} $*" >&2
  fi
}

# Warning - prints but doesn't exit
warn() {
  echo -e "${YELLOW}[WARN]${NC} $*" >&2
}

# Error - prints and exits
error() {
  echo -e "${RED}[ERROR]${NC} $*" >&2
  exit 1
}

# Check for stale ralph scripts or templates and warn the user.
# Compares build-time hashes (embedded in the Nix derivation) against the
# current source files in the workspace. Call early in todo.sh / run.sh.
#
# Checks:
#   1. Scripts: RALPH_METADATA_DIR/scripts-hash vs lib/ralph/cmd/*.sh
#   2. Templates: local .wrapix/ralph/template/ vs RALPH_TEMPLATE_DIR
#
# Usage: check_ralph_staleness
check_ralph_staleness() {
  local metadata_dir="${RALPH_METADATA_DIR:-}"
  local source_cmd_dir="lib/ralph/cmd"

  # --- Script staleness (Nix store vs workspace source) ---
  if [ -n "$metadata_dir" ] && [ -f "$metadata_dir/scripts-hash" ] && [ -d "$source_cmd_dir" ]; then
    local built_hash live_hash
    built_hash=$(<"$metadata_dir/scripts-hash")
    live_hash=$(cat "$source_cmd_dir"/*.sh 2>/dev/null | sha256sum | cut -d' ' -f1)
    if [ "$built_hash" != "$live_hash" ]; then
      warn "ralph scripts are stale — source has changed since last build"
      warn "  run: direnv reload   (or re-enter nix develop)"
      echo "" >&2
    fi
  fi

  # --- Template staleness (local copies vs packaged) ---
  local template_dir="${RALPH_TEMPLATE_DIR:-}"
  local local_dir="${RALPH_DIR:-.wrapix/ralph}/template"
  if [ -n "$template_dir" ] && [ -d "$template_dir" ] && [ -d "$local_dir" ]; then
    local stale_templates=()
    for f in "$local_dir"/*.md; do
      [ -f "$f" ] || continue
      local name
      name=$(basename "$f")
      local packaged="$template_dir/$name"
      if [ -f "$packaged" ] && ! diff -q "$packaged" "$f" >/dev/null 2>&1; then
        stale_templates+=("$name")
      fi
    done
    if [ ${#stale_templates[@]} -gt 0 ]; then
      warn "local templates are out-of-date: ${stale_templates[*]}"
      warn "  run: ralph sync"
      echo "" >&2
    fi
  fi
}

# Validate JSON string is valid
# Usage: validate_json "$json_string" "description"
validate_json() {
  local json="$1"
  local desc="${2:-JSON}"

  if [ -z "$json" ]; then
    warn "$desc is empty"
    return 1
  fi

  if ! echo "$json" | jq empty 2>/dev/null; then
    warn "$desc is not valid JSON: ${json:0:100}..."
    return 1
  fi

  debug "$desc is valid JSON"
  return 0
}

# Extract JSON from mixed output (removes warning lines, keeps JSON)
# Usage: extract_json "$mixed_output"
# Returns: the JSON portion of the output
extract_json() {
  local input="$1"

  # If input is already valid JSON, return as-is
  if echo "$input" | jq empty 2>/dev/null; then
    echo "$input"
    return 0
  fi

  # Find first line starting with [ or { and extract from there
  local json_start
  json_start=$(echo "$input" | grep -n -E '^[\[\{]' | head -1 | cut -d: -f1)
  if [ -n "$json_start" ]; then
    echo "$input" | tail -n +"$json_start"
    return 0
  fi

  # No JSON found, return original input
  echo "$input"
}

# Validate JSON is an array with at least one element
# Usage: validate_json_array "$json_string" "description"
validate_json_array() {
  local json="$1"
  local desc="${2:-JSON}"

  if ! validate_json "$json" "$desc"; then
    return 1
  fi

  local array_length
  array_length=$(echo "$json" | jq 'if type == "array" then length else -1 end')

  if [ "$array_length" = "-1" ]; then
    warn "$desc is not an array"
    return 1
  fi

  if [ "$array_length" = "0" ]; then
    debug "$desc is an empty array"
    return 1
  fi

  debug "$desc is an array with $array_length element(s)"
  return 0
}

# Extract field from JSON array's first element with validation
# Usage: json_array_field "$json" "field_name" "description"
# Returns: field value or empty string, warns if missing
# Note: For new code, use bd_json() to get clean JSON, then pipe to jq directly
json_array_field() {
  local json="$1"
  local field="$2"
  local desc="${3:-field}"

  # Handle potentially mixed output - extract JSON if needed
  if ! echo "$json" | jq empty 2>/dev/null; then
    # Find first line starting with [ or { and extract from there
    local json_start
    json_start=$(echo "$json" | grep -n -E '^[\[\{]' | head -1 | cut -d: -f1)
    if [ -n "$json_start" ]; then
      json=$(echo "$json" | tail -n +"$json_start")
    fi
  fi

  if ! validate_json_array "$json" "JSON for $desc"; then
    echo ""
    return 1
  fi

  local value
  value=$(echo "$json" | jq -r ".[0].$field // empty")

  if [ -z "$value" ]; then
    debug "$desc.$field is empty or missing"
    echo ""
    return 0
  fi

  debug "$desc.$field = ${value:0:50}..."
  echo "$value"
}

# Run a bd command with error capture
# Usage: bd_run "command description" bd list --label foo
# Returns: command output, warns on failure
bd_run() {
  local desc="$1"
  shift

  debug "Running: bd $*"

  local output
  local exit_code

  output=$("$@" 2>&1) && exit_code=0 || exit_code=$?

  if [ $exit_code -ne 0 ]; then
    warn "$desc failed (exit $exit_code): ${output:0:200}"
    echo ""
    return $exit_code
  fi

  debug "$desc succeeded"
  echo "$output"
}

# Run bd command and return clean JSON
# bd outputs warnings/info to stdout mixed with JSON; this wrapper filters them
# Usage: bd_json list --label foo --json
# Returns: clean JSON on stdout, warnings suppressed (or logged with RALPH_DEBUG=1)
bd_json() {
  local stderr_output
  local stdout_output
  local exit_code

  # Capture stderr separately so we can log it in debug mode without polluting JSON
  # Use a temp file for stderr since bash can't capture both streams independently
  local stderr_file
  stderr_file=$(mktemp)
  # shellcheck disable=SC2064
  trap "rm -f '$stderr_file'" RETURN

  debug "Running: bd $*"

  stdout_output=$(bd "$@" 2>"$stderr_file") && exit_code=0 || exit_code=$?
  stderr_output=$(cat "$stderr_file")

  # Log stderr in debug mode
  if [ -n "$stderr_output" ]; then
    debug "bd stderr: ${stderr_output:0:200}"
  fi

  if [ $exit_code -ne 0 ]; then
    warn "bd $1 failed (exit $exit_code): ${stderr_output:0:200}"
    echo "[]"
    return $exit_code
  fi

  # Return clean stdout (should be pure JSON)
  echo "$stdout_output"
}

# Check required variable is set
# Usage: require_var "VAR_NAME" "$VAR_VALUE" "description"
require_var() {
  local name="$1"
  local value="$2"
  local desc="${3:-$name}"

  if [ -z "$value" ]; then
    error "$desc ($name) is required but empty"
  fi

  debug "$name is set: ${value:0:50}..."
}

# Check required file exists
# Usage: require_file "$path" "description"
require_file() {
  local path="$1"
  local desc="${2:-file}"

  if [ ! -f "$path" ]; then
    error "$desc not found: $path"
  fi

  debug "$desc exists: $path"
}

# Parse bd list JSON output and extract issue IDs
# Usage: bd_list_ids "$json_output"
# Returns: space-separated list of IDs
bd_list_ids() {
  local json="$1"

  if ! validate_json_array "$json" "bd list output"; then
    echo ""
    return 1
  fi

  echo "$json" | jq -r '.[].id'
}

# Get first issue ID from bd list JSON output
# Usage: bd_list_first_id "$json_output"
# Note: For new code, use bd_json() to get clean JSON, then pipe to jq directly
bd_list_first_id() {
  local json="$1"

  # Handle potentially mixed output - extract JSON if needed
  if ! echo "$json" | jq empty 2>/dev/null; then
    local json_start
    json_start=$(echo "$json" | grep -n -E '^[\[\{]' | head -1 | cut -d: -f1)
    if [ -n "$json_start" ]; then
      json=$(echo "$json" | tail -n +"$json_start")
    fi
  fi

  if ! validate_json_array "$json" "bd list output"; then
    echo ""
    return 1
  fi

  echo "$json" | jq -r '.[0].id // empty'
}

# Strip "## Implementation Notes" section from markdown content
# This section provides transient context during ralph todo but shouldn't persist in permanent docs
# Usage: strip_implementation_notes "$markdown_content"
# Returns: markdown with Implementation Notes section removed
strip_implementation_notes() {
  local content="$1"

  # Use awk to remove the ## Implementation Notes section
  # Removes from "## Implementation Notes" to the next ## heading or end of file
  echo "$content" | awk '
    /^## Implementation Notes/ { skip = 1; next }
    /^## / && skip { skip = 0 }
    !skip { print }
  '
}

# Build jq filter for stream-json output based on config
# Usage: build_stream_filter "$config_json"
# Returns: jq filter string for processing claude stream-json output
build_stream_filter() {
  local config="$1"

  # Extract output config with defaults
  local responses tool_names tool_inputs tool_results thinking stats
  local max_tool_input max_tool_result

  # Note: jq's // operator treats false as null, so we use explicit null checks
  # to properly handle explicit false values vs missing keys
  responses=$(echo "$config" | jq -r 'if .output.responses == null then true else .output.responses end')
  tool_names=$(echo "$config" | jq -r 'if .output."tool-names" == null then true else .output."tool-names" end')
  tool_inputs=$(echo "$config" | jq -r 'if .output."tool-inputs" == null then true else .output."tool-inputs" end')
  tool_results=$(echo "$config" | jq -r 'if .output."tool-results" == null then true else .output."tool-results" end')
  thinking=$(echo "$config" | jq -r 'if .output.thinking == null then true else .output.thinking end')
  stats=$(echo "$config" | jq -r 'if .output.stats == null then true else .output.stats end')
  max_tool_input=$(echo "$config" | jq -r '.output."max-tool-input" // 200')
  max_tool_result=$(echo "$config" | jq -r '.output."max-tool-result" // 500')

  # Extract prefix config with defaults
  local prefix_response prefix_tool_result prefix_tool_error
  local prefix_thinking_start prefix_thinking_end prefix_stats_header prefix_stats_line
  prefix_response=$(echo "$config" | jq -r '.output.prefixes.response // "[response] "')
  prefix_tool_result=$(echo "$config" | jq -r '.output.prefixes."tool-result" // "[result] "')
  prefix_tool_error=$(echo "$config" | jq -r '.output.prefixes."tool-error" // "[ERROR] "')
  prefix_thinking_start=$(echo "$config" | jq -r '.output.prefixes."thinking-start" // "<thinking>\n"')
  prefix_thinking_end=$(echo "$config" | jq -r '.output.prefixes."thinking-end" // "\n</thinking>"')
  prefix_stats_header=$(echo "$config" | jq -r '.output.prefixes."stats-header" // "\n--- Stats ---\n"')
  prefix_stats_line=$(echo "$config" | jq -r '.output.prefixes."stats-line" // ""')

  debug "Output config: responses=$responses tool_names=$tool_names tool_inputs=$tool_inputs tool_results=$tool_results thinking=$thinking stats=$stats"
  debug "Prefixes: response='$prefix_response' tool_result='$prefix_tool_result' tool_error='$prefix_tool_error'"

  # Build the jq filter dynamically
  # We use a different approach: check message type first, then process content types
  local filter='
# Helper function for truncation
def truncate(n): if n == 0 then . elif (. | length) > n then .[0:n] + "..." else . end;

# Process assistant messages - extract text and thinking from content array
if .type == "assistant" and .message.content then
  .message.content[] |
'

  # Build content type checks within assistant message processing
  local content_checks=""

  if [ "$responses" = "true" ]; then
    content_checks+="
  if .type == \"text\" then \"$prefix_response\" + (.text // empty)"
  fi

  if [ "$thinking" = "true" ]; then
    if [ -n "$content_checks" ]; then
      content_checks+="
  elif .type == \"thinking\" then \"$prefix_thinking_start\" + .thinking + \"$prefix_thinking_end\""
    else
      content_checks+="
  if .type == \"thinking\" then \"$prefix_thinking_start\" + .thinking + \"$prefix_thinking_end\""
    fi
  fi

  # Add tool use inside assistant message content (names and/or inputs)
  if [ "$tool_names" = "true" ] || [ "$tool_inputs" = "true" ]; then
    if [ -n "$content_checks" ]; then
      if [ "$tool_inputs" = "true" ]; then
        content_checks+="
  elif .type == \"tool_use\" then \"[\" + .name + \"] \" + ((.input // {}) | tostring | truncate($max_tool_input))"
      else
        content_checks+='
  elif .type == "tool_use" then "[" + .name + "]"'
      fi
    else
      if [ "$tool_inputs" = "true" ]; then
        content_checks+="
  if .type == \"tool_use\" then \"[\" + .name + \"] \" + ((.input // {}) | tostring | truncate($max_tool_input))"
      else
        content_checks+='
  if .type == "tool_use" then "[" + .name + "]"'
      fi
    fi
  fi

  # Close content type checks or provide default
  if [ -n "$content_checks" ]; then
    filter+="$content_checks"'
  else empty end'
  else
    filter+='empty'
  fi

  # Add tool results
  if [ "$tool_results" = "true" ]; then
    filter+="

# Show tool results
elif .type == \"user\" and .message.content then
  .message.content[] |
  if .type == \"tool_result\" then
    if .is_error == true then
      \"$prefix_tool_error\" + ((.content // \"unknown error\") | tostring | truncate($max_tool_result))
    else
      \"$prefix_tool_result\" + ((.content // \"\") | tostring | truncate($max_tool_result))
    end
  else
    empty
  end"
  fi

  # Add stats output
  if [ "$stats" = "true" ]; then
    filter+="

# Show final stats
elif .type == \"result\" then
  \"$prefix_stats_header\" +
  \"${prefix_stats_line}Cost: \$\" + ((.cost_usd // 0) | tostring) + \"\n\" +
  \"${prefix_stats_line}Input tokens: \" + ((.usage.input_tokens // 0) | tostring) + \"\n\" +
  \"${prefix_stats_line}Output tokens: \" + ((.usage.output_tokens // 0) | tostring) + \"\n\" +
  \"${prefix_stats_line}Duration: \" + ((.duration_ms // 0) / 1000 | tostring) + \"s\""
  fi

  # Close the if chain
  filter+='

else
  empty
end'

  echo "$filter"
}

# Resolve model override for a given phase from config JSON
# Usage: resolve_model <phase> <config_json>
# Output: model string on stdout (empty if null/unset — use ANTHROPIC_MODEL default)
# Phases: run, check, plan, todo, watch
resolve_model() {
  local phase="$1"
  local config="$2"

  local model
  model=$(echo "$config" | jq -r ".model.\"$phase\" // empty" 2>/dev/null || true)

  if [ -n "$model" ] && [ "$model" != "null" ]; then
    debug "resolve_model: phase=$phase model=$model"
    echo "$model"
  fi
}

# Resolve the pinned-context file path relative to the workspace root.
# Reads config.pinnedContext from $RALPH_DIR/config.nix; falls back to
# docs/README.md when config is missing or unreadable.
# Usage: path=$(get_pinned_context_file)
get_pinned_context_file() {
  local ralph_dir="${RALPH_DIR:-.wrapix/ralph}"
  local config_file="$ralph_dir/config.nix"
  local default="docs/README.md"

  if [ ! -f "$config_file" ]; then
    debug "get_pinned_context_file: $config_file missing, using default $default"
    echo "$default"
    return 0
  fi

  local json
  if ! json=$(nix eval --json --file "$config_file"); then
    warn "get_pinned_context_file: failed to eval $config_file, using default $default"
    echo "$default"
    return 0
  fi

  local configured
  configured=$(echo "$json" | jq -r '.pinnedContext // empty')
  if [ -z "$configured" ]; then
    debug "get_pinned_context_file: config has no pinnedContext, using default $default"
    echo "$default"
    return 0
  fi

  echo "$configured"
}

# Run claude with stream-json output and configurable display
# Usage: run_claude_stream "$prompt" "$log_file" "$config_json" [model]
# Prompt is piped via stdin (avoids argv/environ ARG_MAX limits, wx-gaasw).
# If model is provided, --model <model> is prepended to claude CLI args
run_claude_stream() {
  local prompt="$1"
  local log_file="$2"
  local config="$3"
  local model="${4:-}"

  local jq_filter
  jq_filter=$(build_stream_filter "$config")

  debug "Running claude with stream-json output to $log_file"

  # Build claude args
  local -a claude_args=(--dangerously-skip-permissions --print --output-format stream-json --verbose)
  if [ -n "$model" ]; then
    claude_args+=(--model "$model")
    debug "Using model override: $model"
  fi

  # Prompt via stdin, not argv: argv is bounded by MAX_ARG_STRLEN (128KB
  # per string on Linux); multi-spec tier-1 diffs blow through it (wx-gaasw).
  printf '%s' "$prompt" \
    | claude "${claude_args[@]}" 2>&1 \
    | tee "$log_file" \
    | jq --unbuffered -r "$jq_filter" 2>/dev/null || true
}

# Validate template has required placeholders, reset from source if corrupted
# Usage: validate_template "$local_path" "$source_path" "$template_name"
# Returns: 0 if valid or repaired, 1 if repair failed
validate_template() {
  local local_path="$1"
  local source_path="$2"
  local template_name="${3:-template}"

  if [ ! -f "$local_path" ]; then
    warn "$template_name not found at $local_path"
    return 1
  fi

  # Resolve partials before checking for required placeholders
  # This handles templates that include {{LABEL}} via {{> partial-name}}
  local template_dir
  template_dir=$(dirname "$local_path")
  local partial_dir="$template_dir/partial"

  local content
  content=$(cat "$local_path")

  # Resolve partials if the directory exists
  if [ -d "$partial_dir" ]; then
    content=$(resolve_partials "$content" "$partial_dir")
  fi

  # Check for required placeholder in resolved content
  if ! echo "$content" | grep -q '{{LABEL}}'; then
    warn "$template_name is missing {{LABEL}} placeholder - resetting from source"

    if [ ! -f "$source_path" ]; then
      warn "Source template not found at $source_path - cannot repair"
      return 1
    fi

    # Backup corrupted file before overwriting
    cp "$local_path" "${local_path}.bak"
    debug "Backed up corrupted $template_name to ${local_path}.bak"

    cp "$source_path" "$local_path"
    debug "$template_name reset from $source_path"
  fi

  return 0
}

# Run claude interactively with an initial prompt
# Usage: run_claude_interactive "$prompt"
# Opens an interactive Claude console with the prompt as initial context
run_claude_interactive() {
  local prompt="$1"

  debug "Running claude interactively"

  # Run claude without --print to open interactive console
  # The prompt is passed as the initial message
  claude --dangerously-skip-permissions "$prompt"
}

# Get variable definitions from pre-computed metadata
# Usage: get_variable_definitions
# Returns: JSON object with all variable definitions
# Cached in RALPH_VAR_DEFS for performance
get_variable_definitions() {
  # Return cached value if available
  if [ -n "${RALPH_VAR_DEFS:-}" ]; then
    echo "$RALPH_VAR_DEFS"
    return 0
  fi

  # Find metadata file
  local metadata_file=""
  if [ -n "${RALPH_METADATA_DIR:-}" ] && [ -f "${RALPH_METADATA_DIR}/variables.json" ]; then
    metadata_file="${RALPH_METADATA_DIR}/variables.json"
  else
    # Try to find via script location (fallback for development)
    local script_dir
    script_dir="$(dirname "${BASH_SOURCE[0]}")"
    if [ -f "$script_dir/../share/ralph/variables.json" ]; then
      metadata_file="$script_dir/../share/ralph/variables.json"
    fi
  fi

  if [ -z "$metadata_file" ]; then
    warn "Variable definitions not found (RALPH_METADATA_DIR not set)"
    echo "{}"
    return 1
  fi

  local var_defs
  var_defs=$(cat "$metadata_file") || {
    warn "Failed to read variable definitions from $metadata_file"
    echo "{}"
    return 1
  }

  # Cache for subsequent calls
  export RALPH_VAR_DEFS="$var_defs"
  echo "$var_defs"
}

# Get template variables (list of required variables for a template)
# Usage: get_template_variables <template-name>
# Returns: JSON array of variable names, or empty array on error
get_template_variables() {
  local template_name="$1"

  # Find metadata file
  local metadata_file=""
  if [ -n "${RALPH_METADATA_DIR:-}" ] && [ -f "${RALPH_METADATA_DIR}/templates.json" ]; then
    metadata_file="${RALPH_METADATA_DIR}/templates.json"
  else
    # Try to find via script location (fallback for development)
    local script_dir
    script_dir="$(dirname "${BASH_SOURCE[0]}")"
    if [ -f "$script_dir/../share/ralph/templates.json" ]; then
      metadata_file="$script_dir/../share/ralph/templates.json"
    fi
  fi

  if [ -z "$metadata_file" ]; then
    warn "Template metadata not found (RALPH_METADATA_DIR not set)"
    echo "[]"
    return 1
  fi

  local vars
  vars=$(jq -r --arg name "$template_name" '.[$name] // []' "$metadata_file" 2>/dev/null) || {
    warn "Failed to get variables for template: $template_name"
    echo "[]"
    return 1
  }

  echo "$vars"
}

# Render a template with variable substitution
# Usage: render_template <template-name> [VAR=value ...]
#
# Reads the template from RALPH_TEMPLATE_DIR (or local .wrapix/ralph/template),
# resolves partials, validates required variables, and substitutes placeholders.
#
# Variables can be passed as arguments (VAR=value) or read from environment.
# Required variables that are missing will cause an error.
#
# Example:
#   render_template run LABEL=my-feature ISSUE_ID=beads-123
#   LABEL=my-feature render_template run
render_template() {
  local template_name="$1"
  shift

  # Determine template directory
  local template_dir="${RALPH_TEMPLATE_DIR:-}"
  local local_template_dir="${RALPH_DIR:-.wrapix/ralph}/template"

  # Prefer local template if it exists, otherwise use RALPH_TEMPLATE_DIR
  local template_path
  if [ -f "$local_template_dir/${template_name}.md" ]; then
    template_path="$local_template_dir/${template_name}.md"
    template_dir="$local_template_dir"
  elif [ -n "$template_dir" ] && [ -f "$template_dir/${template_name}.md" ]; then
    template_path="$template_dir/${template_name}.md"
  else
    warn "Template not found: ${template_name}.md (checked $local_template_dir and ${template_dir:-<unset>})"
    return 1
  fi

  debug "Rendering template: $template_path"

  # Parse VAR=value arguments into an associative array
  declare -A vars
  for arg in "$@"; do
    # Skip empty arguments
    [ -z "$arg" ] && continue
    if [[ "$arg" =~ ^([A-Z_][A-Z0-9_]*)=(.*)$ ]]; then
      vars["${BASH_REMATCH[1]}"]="${BASH_REMATCH[2]}"
      debug "  ${BASH_REMATCH[1]}=${BASH_REMATCH[2]:0:50}..."
    else
      warn "Invalid variable argument (expected VAR=value): $arg"
    fi
  done

  # Get required variables for this template from metadata
  local required_vars
  required_vars=$(get_template_variables "$template_name")

  if [ "$required_vars" = "[]" ]; then
    debug "No variable requirements found for template: $template_name"
  fi

  # Check that all required variables are provided (via args or environment)
  local missing_vars=()
  while IFS= read -r var_name; do
    [ -z "$var_name" ] && continue

    # Check if variable is in args
    if [ -n "${vars[$var_name]+set}" ]; then
      continue
    fi

    # Check if variable is in environment
    if [ -n "${!var_name+set}" ]; then
      vars["$var_name"]="${!var_name}"
      continue
    fi

    # Variable is missing - check if it's required in the definitions
    local var_defs
    var_defs=$(get_variable_definitions)
    local is_required
    is_required=$(echo "$var_defs" | jq -r --arg name "$var_name" '.[$name].required // false')

    if [ "$is_required" = "true" ]; then
      missing_vars+=("$var_name")
    else
      # Use default value if available
      local default_val
      default_val=$(echo "$var_defs" | jq -r --arg name "$var_name" '.[$name].default // empty')
      vars["$var_name"]="${default_val:-}"
      debug "Using default for $var_name: ${default_val:-<empty>}"
    fi
  done < <(echo "$required_vars" | jq -r '.[]')

  if [ ${#missing_vars[@]} -gt 0 ]; then
    warn "Missing required variables for template '$template_name': ${missing_vars[*]}"
    return 1
  fi

  # Read template content
  local content
  content=$(cat "$template_path")

  # Resolve partials ({{> partial-name}})
  local partial_dir="$template_dir/partial"
  if [ -d "$partial_dir" ]; then
    content=$(resolve_partials "$content" "$partial_dir")
  fi

  # Substitute variables
  # Process each variable, handling multiline values with bash string replacement
  for var_name in "${!vars[@]}"; do
    local var_value="${vars[$var_name]}"
    local marker="{{${var_name}}}"

    # Use bash string replacement for simple substitutions
    # This handles multiline values correctly and preserves blank lines
    content="${content//"$marker"/$var_value}"
  done

  echo "$content"
}

# Resolve partial markers {{> partial-name}} in template content
# Usage: resolve_partials "$content" "$partial_dir"
# Returns: content with partials resolved
resolve_partials() {
  local content="$1"
  local partial_dir="$2"

  if [ -z "$partial_dir" ] || [ ! -d "$partial_dir" ]; then
    debug "Partial directory not available, returning content unchanged"
    echo "$content"
    return 0
  fi

  # Find all partial references {{> partial-name}}
  local refs
  refs=$(echo "$content" | grep -oE '\{\{> [a-z-]+\}\}' | sed 's/{{> //;s/}}//' | sort -u || true)

  if [ -z "$refs" ]; then
    debug "No partial references found"
    echo "$content"
    return 0
  fi

  # Resolve each partial
  local result="$content"
  for ref in $refs; do
    local partial_path="$partial_dir/${ref}.md"
    if [ -f "$partial_path" ]; then
      local partial_content
      partial_content=$(cat "$partial_path")
      # Use awk for safe substitution of multi-line content
      result=$(echo "$result" | awk -v marker="{{> $ref}}" -v replacement="$partial_content" '{
        idx = index($0, marker)
        if (idx > 0) {
          before = substr($0, 1, idx - 1)
          after = substr($0, idx + length(marker))
          print before replacement after
        } else {
          print
        }
      }')
      debug "Resolved partial: $ref"
    else
      warn "Partial not found: $partial_path"
    fi
  done

  echo "$result"
}

# Parse annotation link in 'path#function' or legacy 'path::function' format
# Usage: parse_annotation_link "tests/notify-test.sh#test_notification_timing"
#        parse_annotation_link "../tests/notify-test.sh#test_timing" "specs"
# Output: two lines: file_path and function_name (function_name empty if no separator)
# When spec_dir is provided, resolves relative paths (e.g. ../tests/foo.sh -> tests/foo.sh)
# Returns: 0 on valid input, 1 on empty input
parse_annotation_link() {
  local link="$1"
  local spec_dir="${2:-}"

  if [ -z "$link" ]; then
    warn "Empty annotation link"
    return 1
  fi

  local file_path function_name
  # Support # (primary) or :: (legacy) as separator
  if [[ "$link" == *"#"* ]]; then
    file_path="${link%%#*}"
    function_name="${link#*#}"
  elif [[ "$link" == *"::"* ]]; then
    file_path="${link%%::*}"
    function_name="${link#*::}"
  else
    file_path="$link"
    function_name=""
  fi

  # Resolve spec-relative paths to repo-root-relative
  if [ -n "$spec_dir" ] && [[ "$file_path" == ../* ]]; then
    # Combine spec_dir with relative path and normalize ../
    local combined="${spec_dir}/${file_path}"
    # Repeatedly collapse "dir/../" segments
    while [[ "$combined" == *"/../"* || "$combined" == *"/.." ]]; do
      combined=$(echo "$combined" | sed 's|[^/][^/]*/\.\./||; s|[^/][^/]*/\.\.$||')
    done
    file_path="$combined"
  fi

  echo "$file_path"
  echo "$function_name"
  return 0
}

# Parse spec annotations from a spec file's Success Criteria section
# Scans for '- [ ]' or '- [x]' lines, then checks the next line for
# [verify](...) or [judge](...) links.
#
# Usage: parse_spec_annotations "specs/notifications.md"
# Output: TAB-separated records, one per criterion:
#   criterion_text<TAB>annotation_type<TAB>file_path<TAB>function_name<TAB>checked
#
# annotation_type: "verify", "judge", or "none"
# file_path/function_name: empty when annotation_type is "none"
# checked: "x" if [x], "" if [ ]
#
# Returns: 0 on success, 1 if file not found or no Success Criteria section
parse_spec_annotations() {
  local spec_file="$1"

  if [ ! -f "$spec_file" ]; then
    warn "Spec file not found: $spec_file"
    return 1
  fi

  # Get the directory containing the spec file for resolving relative paths
  local spec_dir
  spec_dir=$(dirname "$spec_file")

  local in_criteria=0
  local in_fence=0
  local prev_criterion=""
  local prev_checked=""
  local has_criteria=0

  while IFS= read -r line; do
    # Skip fenced code blocks: their headings are literal content, not sections.
    if [[ "$line" =~ ^[[:space:]]*\`\`\` ]]; then
      if [ "$in_fence" -eq 0 ]; then
        in_fence=1
      else
        in_fence=0
      fi
      continue
    fi

    if [ "$in_fence" -eq 1 ]; then
      continue
    fi

    # Detect start of Success Criteria section
    if [[ "$line" =~ ^##[[:space:]]+Success[[:space:]]+Criteria ]]; then
      in_criteria=1
      continue
    fi

    # Detect end of Success Criteria section (next ## heading)
    if [ "$in_criteria" -eq 1 ] && [[ "$line" =~ ^##[[:space:]] ]] && [[ ! "$line" =~ ^##[[:space:]]+Success ]]; then
      # Flush any pending criterion without annotation
      if [ -n "$prev_criterion" ]; then
        printf '%s\t%s\t%s\t%s\t%s\n' "$prev_criterion" "none" "" "" "$prev_checked"
        prev_criterion=""
        prev_checked=""
      fi
      break
    fi

    if [ "$in_criteria" -eq 0 ]; then
      continue
    fi

    # Match criterion lines: - [ ] or - [x]
    local criterion_re='^[[:space:]]*-[[:space:]]\[([[:space:]x])\][[:space:]]+(.*)'
    if [[ "$line" =~ $criterion_re ]]; then
      # Flush previous criterion if it had no annotation
      if [ -n "$prev_criterion" ]; then
        printf '%s\t%s\t%s\t%s\t%s\n' "$prev_criterion" "none" "" "" "$prev_checked"
      fi
      local check_mark="${BASH_REMATCH[1]}"
      prev_criterion="${BASH_REMATCH[2]}"
      prev_checked=""
      if [ "$check_mark" = "x" ]; then
        prev_checked="x"
      fi
      has_criteria=1
      continue
    fi

    # Match annotation lines: [verify](...) or [judge](...)
    if [ -n "$prev_criterion" ]; then
      local ann_type="" ann_target=""
      # Targets may contain one level of balanced parens (e.g. "Wait for X (via Y)").
      local verify_wrapix_re='^[[:space:]]*\[verify:wrapix\]\(([^()]*(\([^()]*\)[^()]*)*)\)'
      local verify_re='^[[:space:]]*\[verify\]\(([^()]*(\([^()]*\)[^()]*)*)\)'
      local judge_re='^[[:space:]]*\[judge\]\(([^()]*(\([^()]*\)[^()]*)*)\)'
      if [[ "$line" =~ $verify_wrapix_re ]]; then
        ann_type="verify-wrapix"
        ann_target="${BASH_REMATCH[1]}"
      elif [[ "$line" =~ $verify_re ]]; then
        ann_type="verify"
        ann_target="${BASH_REMATCH[1]}"
      elif [[ "$line" =~ $judge_re ]]; then
        ann_type="judge"
        ann_target="${BASH_REMATCH[1]}"
      fi

      if [ -n "$ann_type" ]; then
        # Parse the annotation link using parse_annotation_link with spec dir
        local parsed_output file_path function_name
        parsed_output=$(parse_annotation_link "$ann_target" "$spec_dir")
        file_path=$(echo "$parsed_output" | sed -n '1p')
        function_name=$(echo "$parsed_output" | sed -n '2p')
        printf '%s\t%s\t%s\t%s\t%s\n' "$prev_criterion" "$ann_type" "$file_path" "$function_name" "$prev_checked"
        prev_criterion=""
        prev_checked=""
        continue
      fi
    fi
  done < "$spec_file"

  # Flush final criterion if file ends inside Success Criteria
  if [ -n "$prev_criterion" ]; then
    printf '%s\t%s\t%s\t%s\t%s\n' "$prev_criterion" "none" "" "" "$prev_checked"
  fi

  if [ "$has_criteria" -eq 0 ]; then
    debug "No success criteria found in $spec_file"
    return 1
  fi

  return 0
}

#-----------------------------------------------------------------------------
# Companion Manifest Reader
#
# Reads companion directories from state JSON and concatenates their
# manifest.md files wrapped in XML-style <companion path="..."> tags.
#
# Usage: read_manifests <state_file>
# Output: concatenated manifests on stdout (empty string if no companions)
# Returns: 0 on success, 1 on error (missing directory or manifest)
#-----------------------------------------------------------------------------
read_manifests() {
  local state_file="$1"

  if [ ! -f "$state_file" ]; then
    error "read_manifests: state file not found: $state_file"
  fi

  # Read companions array from state JSON
  local companions_json
  companions_json=$(jq -r '.companions // empty' "$state_file" 2>/dev/null || echo "")

  # No companions field or null → return empty string
  if [ -z "$companions_json" ] || [ "$companions_json" = "null" ]; then
    debug "read_manifests: no companions declared"
    return 0
  fi

  # Check it's an array
  local count
  count=$(echo "$companions_json" | jq -r 'if type == "array" then length else -1 end' 2>/dev/null || echo "-1")
  if [ "$count" = "-1" ]; then
    error "read_manifests: companions field is not an array"
  fi

  # Empty array → return empty string
  if [ "$count" = "0" ]; then
    debug "read_manifests: companions array is empty"
    return 0
  fi

  local result=""
  local i=0
  while [ "$i" -lt "$count" ]; do
    local dir_path
    dir_path=$(echo "$companions_json" | jq -r ".[$i]")

    # Validate directory exists
    if [ ! -d "$dir_path" ]; then
      error "read_manifests: companion directory does not exist: $dir_path"
    fi

    # Validate manifest.md exists
    local manifest_path="$dir_path/manifest.md"
    if [ ! -f "$manifest_path" ]; then
      error "read_manifests: companion directory lacks manifest.md: $dir_path"
    fi

    local manifest_content
    manifest_content=$(<"$manifest_path")

    if [ -n "$result" ]; then
      result+=$'\n'
    fi
    result+="<companion path=\"$dir_path\">"
    result+=$'\n'"$manifest_content"$'\n'
    result+="</companion>"

    i=$((i + 1))
  done

  echo "$result"
}

#-----------------------------------------------------------------------------
# Error Extraction from Claude Logs
#
# Extracts error output from a JSON stream log file for retry context.
# Looks for error messages, tool errors, and the final result text.
#-----------------------------------------------------------------------------

# Extract error context from a claude stream-json log file
# Usage: extract_error_from_log <log_file>
# Output: A summary of error output (truncated to ~2000 chars)
extract_error_from_log() {
  local log_file="$1"
  local error_output=""

  if [ ! -f "$log_file" ]; then
    echo "(no log file found)"
    return 0
  fi

  # Extract the final result text
  local result_text
  result_text=$(jq -r 'select(.type == "result") | .result // empty' "$log_file" 2>/dev/null | head -50)

  # Extract tool errors (non-zero exit codes from bash, failed operations)
  local tool_errors
  tool_errors=$(jq -r 'select(.type == "tool_result" and .is_error == true) | .content // empty' "$log_file" 2>/dev/null | tail -30)

  # Build error summary
  if [ -n "$tool_errors" ]; then
    error_output="Tool errors:\n$tool_errors"
  fi

  if [ -n "$result_text" ]; then
    if [ -n "$error_output" ]; then
      error_output="$error_output\n\nFinal output:\n$result_text"
    else
      error_output="Final output:\n$result_text"
    fi
  fi

  # Fallback: last 30 lines of the log
  if [ -z "$error_output" ]; then
    error_output=$(tail -30 "$log_file" 2>/dev/null || echo "(could not read log)")
  fi

  # Truncate to ~2000 chars to avoid blowing up the prompt
  echo -e "$error_output" | head -80 | cut -c1-2000
}

#-----------------------------------------------------------------------------
# Notification Helper
#
# Fire-and-forget notification. Uses wrapix-notify if available,
# otherwise logs to stderr. Never fails the calling script.
#
# Usage: notify_event <title> [body]
#-----------------------------------------------------------------------------
notify_event() {
  local title="$1"
  local body="${2:-}"

  if command -v wrapix-notify &>/dev/null; then
    # best-effort: notifier may be unreachable (e.g. unit not running); never fail caller
    wrapix-notify "$title" "$body" || true
  else
    debug "[notify] ${title}${body:+: $body}"
  fi
}

#-----------------------------------------------------------------------------
# ralph:clarify Label Management
#
# Helpers for adding/removing the ralph:clarify label on beads, listing
# beads that carry it, and filtering queue output to skip them. Used by
# ralph run, ralph msg, ralph status, and the reviewer agent.
#-----------------------------------------------------------------------------

# Marker line preceding clarify notes appended to a bead's description.
# Kept as a literal HTML comment so it renders invisibly in markdown views
# while staying easy to locate with a grep/awk reader.
CLARIFY_NOTE_MARKER="<!-- ralph:clarify -->"

# Append a clarify note block to a bead's description.
# Block shape:
#     <!-- ralph:clarify -->
#     **Clarify:** <note>
# Usage: append_clarify_note <bead_id> <note>
# Returns: 0 on success, non-zero on failure (with warnings)
append_clarify_note() {
  local bead_id="$1"
  local note="$2"

  local current_desc
  current_desc=$(bd_json show "$bead_id" --json 2>/dev/null | jq -r '.[0].description // ""')

  local new_desc
  new_desc=$(printf '%s\n\n%s\n**Clarify:** %s' "$current_desc" "$CLARIFY_NOTE_MARKER" "$note")

  bd update "$bead_id" --description "$new_desc" || {
    warn "Failed to append clarify note to $bead_id description"
    return 1
  }

  return 0
}

# Extract the most recent clarify note from a bead description.
# Usage: extract_clarify_note <description>
# Output: clarify note text (empty string if none found)
extract_clarify_note() {
  local desc="$1"
  echo "$desc" | awk -v marker="$CLARIFY_NOTE_MARKER" '
    $0 == marker { in_block = 1; next }
    in_block && /^\*\*Clarify:\*\* / {
      sub(/^\*\*Clarify:\*\* /, "")
      last = $0
      in_block = 0
      next
    }
    in_block && NF == 0 { next }
    in_block { in_block = 0 }
    END { if (last != "") print last }
  '
}

# Resolve the question text for a clarify bead. Falls through in order:
#   1. clarify-note marker block in description (current convention)
#   2. legacy "Question: ..." line in notes
#   3. non-empty notes body as-is
#   4. bead title (ensures the list view never renders blank for a
#      ralph:clarify bead authored outside add_clarify_label)
# Usage: get_question_for_bead <description> <notes> [title]
get_question_for_bead() {
  local description="$1"
  local notes="$2"
  local title="${3:-}"

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
  if [ -n "$question" ]; then
    echo "$question"
    return 0
  fi

  echo "$title"
}

# Add ralph:clarify label to a bead, optionally appending a note to the
# bead's description. Emits a desktop notification only on first
# application (label not previously present on the bead).
# Usage: add_clarify_label <bead_id> [note]
# Returns: 0 on success, non-zero on failure (with warnings)
add_clarify_label() {
  local bead_id="$1"
  local note="${2:-}"

  local bead_json
  bead_json=$(bd_json show "$bead_id" --json 2>/dev/null) || bead_json="[]"

  local had_label=0
  if echo "$bead_json" | jq -e '.[0].labels // [] | any(. == "ralph:clarify")' >/dev/null 2>&1; then
    had_label=1
  fi

  if [ -n "$note" ]; then
    append_clarify_note "$bead_id" "$note" || true
  fi

  bd update "$bead_id" --add-label "ralph:clarify" || {
    warn "Failed to add ralph:clarify label to $bead_id"
    return 1
  }

  if [ "$had_label" = "0" ]; then
    local bead_title
    bead_title=$(echo "$bead_json" | jq -r '.[0].title // empty' 2>/dev/null || true)
    notify_event "Ralph" "Input needed for ${bead_title:-$bead_id}"
  fi

  return 0
}

# Remove ralph:clarify label from a bead
# Usage: remove_clarify_label <bead_id>
# Returns: 0 on success, non-zero on failure (with warnings)
remove_clarify_label() {
  local bead_id="$1"

  bd update "$bead_id" --remove-label "ralph:clarify" || {
    warn "Failed to remove ralph:clarify label from $bead_id"
    return 1
  }

  return 0
}

# List beads carrying the ralph:clarify label.
# Optionally filter by spec label (adds --label spec:<label>).
# Usage: list_clarify_beads [spec_label]
# Output: JSON array of beads ([] on error or no results)
list_clarify_beads() {
  local spec_label="${1:-}"
  local -a args=(list --label "ralph:clarify" --json)
  if [ -n "$spec_label" ]; then
    args+=(--label "spec:$spec_label")
  fi
  bd_json "${args[@]}" || echo "[]"
}

# Filter a JSON array of beads, dropping those carrying the ralph:clarify
# label. Consumes bd list/ready --json output; preserves array shape.
# Usage: filter_clarify_beads <json>
# Output: filtered JSON array ([] on parse error)
filter_clarify_beads() {
  local json="$1"
  echo "$json" \
    | jq '[.[] | select((.labels // []) | map(select(. == "ralph:clarify")) | length == 0)]' \
      2>/dev/null \
    || echo "[]"
}

#-----------------------------------------------------------------------------
# Iteration Counter (run ↔ check auto-iteration)
#
# Tracks how many unsuccessful check iterations a molecule has consumed.
# Persisted in state/<label>.json as .iteration_count. Bounded by
# loop.max-iterations; reset to 0 on clean RALPH_COMPLETE (push path) or
# when a clarify is cleared via ralph msg.
#-----------------------------------------------------------------------------

# Read iteration_count from state JSON (0 if absent or file missing).
# Usage: get_iteration_count <state_file>
get_iteration_count() {
  local state_file="$1"
  if [ ! -f "$state_file" ]; then
    echo 0
    return 0
  fi
  jq -r '.iteration_count // 0' "$state_file" 2>/dev/null || echo 0
}

# Write iteration_count to state JSON (creates file if absent).
# Usage: set_iteration_count <state_file> <n>
set_iteration_count() {
  local state_file="$1"
  local count="$2"
  local dir
  dir=$(dirname "$state_file")
  mkdir -p "$dir"
  if [ -f "$state_file" ]; then
    jq --argjson n "$count" '.iteration_count = $n' "$state_file" \
      > "$state_file.tmp" && mv "$state_file.tmp" "$state_file"
  else
    jq -n --argjson n "$count" '{iteration_count: $n}' > "$state_file"
  fi
}

# Reset iteration_count to 0.
# Usage: reset_iteration_count <state_file>
reset_iteration_count() {
  local state_file="$1"
  # Only touch the field if a state file exists — no-op if the spec has none.
  [ -f "$state_file" ] || return 0
  set_iteration_count "$state_file" 0
}

#-----------------------------------------------------------------------------
# Worktree Management for Parallel Dispatch
#
# Helpers for creating, merging, and cleaning up git worktrees used by
# parallel ralph run workers. Each worker gets an isolated worktree on
# a branch named ralph/<label>/<bead-id>.
#-----------------------------------------------------------------------------

# Create a git worktree for a parallel worker
# Usage: create_worktree <label> <bead_id>
# Output: worktree path on stdout
# Returns: 0 on success, 1 on failure
create_worktree() {
  local label="$1"
  local bead_id="$2"
  local branch_name="ralph/${label}/${bead_id}"
  local worktree_path
  worktree_path=$(mktemp -d "/tmp/ralph-worktree-XXXXXX")

  debug "Creating worktree at $worktree_path on branch $branch_name"

  if ! git worktree add "$worktree_path" -b "$branch_name" HEAD; then
    warn "Failed to create worktree for $bead_id"
    rm -rf "$worktree_path"
    return 1
  fi

  echo "$worktree_path"
}

# Merge a worktree branch back to the current branch
# Usage: merge_worktree <worktree_path> <bead_id>
# Returns: 0 on success, 1 on merge conflict (bead reopened with details)
merge_worktree() {
  local worktree_path="$1"
  local bead_id="$2"
  local branch_name
  branch_name=$(git -C "$worktree_path" rev-parse --abbrev-ref HEAD)

  debug "Merging worktree branch $branch_name for $bead_id"

  if git merge --no-edit "$branch_name"; then
    # Success — clean up worktree and branch
    # best-effort: worktree already removed by user or branch consumed upstream
    git worktree remove "$worktree_path" || true
    git branch -d "$branch_name" || true
    debug "Successfully merged and cleaned up worktree for $bead_id"
    return 0
  else
    # Merge conflict — abort merge, reopen bead with details
    warn "Merge conflict for $bead_id on branch $branch_name"
    # best-effort: no merge in progress if merge failed pre-commit
    git merge --abort || true

    # Reopen the bead with conflict information; bd failures are warned
    # rather than swallowed so we notice if the bead isn't actually reopened.
    bd update "$bead_id" --status=open || warn "bd update --status=open failed for $bead_id"
    bd update "$bead_id" --add-label "ralph:clarify" || warn "bd add-label failed for $bead_id"
    bd update "$bead_id" --append-notes "Merge conflict when merging worktree branch $branch_name back to main working branch. Manual resolution required." \
      || warn "bd append-notes failed for $bead_id"

    # Clean up worktree
    cleanup_worktree "$worktree_path"
    # best-effort: branch may already be gone if cleanup_worktree pruned it
    git branch -D "$branch_name" || true
    return 1
  fi
}

# Force-remove a worktree (for error cleanup)
# Usage: cleanup_worktree <worktree_path>
# Returns: 0 always
cleanup_worktree() {
  local worktree_path="$1"

  debug "Cleaning up worktree at $worktree_path"
  # best-effort: worktree may be corrupted; fall back to direct rm
  git worktree remove --force "$worktree_path" || rm -rf "$worktree_path"
  return 0
}

#-----------------------------------------------------------------------------
# Spec Hidden Detection
#
# Derives whether a spec is hidden from its spec_path in state JSON.
# Hidden specs have spec_path pointing into the ralph state directory.
# This replaces the old .hidden field in state JSON.
#
# Usage: spec_is_hidden "$state_file"
# Returns: 0 (true) if hidden, 1 (false) if not hidden
#-----------------------------------------------------------------------------
spec_is_hidden() {
  local state_file="$1"
  local spec_path
  spec_path=$(jq -r '.spec_path // ""' "$state_file" 2>/dev/null || echo "")
  [[ "$spec_path" == *"/state/"* ]]
}

#-----------------------------------------------------------------------------
# Spec Diff Computation (Four-Tier Fallback)
#
# Determines how to detect spec changes for `ralph todo`:
#   Tier 1 (diff):     base_commit exists and is valid → per-spec fan-out diff
#   Tier 2 (tasks):    no base_commit but molecule exists → fetch task list
#   Tier 3 (README):   no state file → discover molecule from README, reconstruct state, proceed as tier 2
#   Tier 4 (new):      no state file AND no molecule in README → full spec decomposition
#
# Usage: compute_spec_diff <state_file> [--since <commit>]
# Output on stdout:
#   Line 1: mode indicator — "diff", "tasks", or "new"
#   Line 2+: content (see format below; empty for tier 4 or empty tier 1 set)
#
# Tier 1 content format (git-tracked anchor, fan-out):
#   For each spec in the candidate set with non-empty diff:
#     === <spec_path> ===
#     <git diff output>
#   Empty candidate set → empty content.
#
# Tier 1 content format (hidden anchor): raw git diff (single-spec, no fan-out).
#
# --since <commit> forces tier 1 with the given commit as the anchor's cursor;
# sibling specs keep their own base_commit. Errors if commit is invalid.
#
# Environment:
#   GIT_DIR — optional, for non-standard git repos
#-----------------------------------------------------------------------------
compute_spec_diff() {
  local state_file="$1"
  shift
  local since_commit=""

  # Parse optional --since flag
  while [ $# -gt 0 ]; do
    case "$1" in
      --since)
        if [ -z "${2:-}" ]; then
          error "compute_spec_diff: --since requires a commit argument"
        fi
        since_commit="$2"
        shift 2
        ;;
      *)
        warn "compute_spec_diff: unknown argument: $1"
        shift
        ;;
    esac
  done

  # Tier 3/4: state file doesn't exist → try README discovery
  if [ ! -f "$state_file" ]; then
    # Derive label from state file path: state/<label>.json → <label>
    local label
    label=$(basename "$state_file" .json)
    debug "compute_spec_diff: state file not found, trying README discovery for label '$label'"

    local readme_molecule
    readme_molecule=$(discover_molecule_from_readme "$label")

    if [ -n "$readme_molecule" ]; then
      # Tier 3: reconstruct state file and proceed as tier 2
      debug "compute_spec_diff: tier 3 — README discovery found molecule '$readme_molecule'"
      local state_dir
      state_dir=$(dirname "$state_file")
      mkdir -p "$state_dir"
      jq -n \
        --arg label "$label" \
        --arg spec_path "specs/${label}.md" \
        --arg molecule "$readme_molecule" \
        '{label: $label, spec_path: $spec_path, molecule: $molecule, companions: []}' \
        > "$state_file"
      # Fall through to read state file and proceed (tier 2 path)
    else
      # Tier 4: no molecule in README → new mode
      debug "compute_spec_diff: tier 4 — no state file, no README molecule"
      echo "new"
      echo ""
      return 0
    fi
  fi

  local spec_path molecule base_commit
  spec_path=$(jq -r '.spec_path // ""' "$state_file")
  molecule=$(jq -r '.molecule // ""' "$state_file")
  base_commit=$(jq -r '.base_commit // ""' "$state_file")

  # --since overrides base_commit
  if [ -n "$since_commit" ]; then
    # Validate that the commit exists
    if ! git rev-parse --verify "$since_commit^{commit}" >/dev/null 2>&1; then
      error "compute_spec_diff: invalid commit: $since_commit"
    fi
    base_commit="$since_commit"
    debug "compute_spec_diff: --since override: $base_commit"
  fi

  # Tier 1: base_commit exists → git diff
  if [ -n "$base_commit" ]; then
    # Validate the commit is still reachable (not orphaned by rebase/amend)
    if git rev-parse --verify "$base_commit^{commit}" >/dev/null 2>&1; then
      # Check ancestry — if not an ancestor of HEAD, it was rebased
      if git merge-base --is-ancestor "$base_commit" HEAD 2>/dev/null; then
        debug "compute_spec_diff: tier 1 — git diff from $base_commit"

        # Hidden specs bypass fan-out (not in git, no sibling model)
        if [[ "$spec_path" == *"/state/"* ]]; then
          local diff_output
          diff_output=$(git diff "$base_commit" HEAD -- "$spec_path" 2>/dev/null || true)
          echo "diff"
          echo "$diff_output"
          return 0
        fi

        # Per-spec cursor fan-out: candidate set from anchor's cursor
        local candidate_list
        candidate_list=$(git diff "$base_commit" HEAD --name-only -- specs/ 2>/dev/null || true)

        echo "diff"
        if [ -z "$candidate_list" ]; then
          return 0
        fi

        local state_dir
        state_dir=$(dirname "$state_file")

        local candidate_spec cand_label cand_state effective_base spec_diff
        while IFS= read -r candidate_spec; do
          [ -z "$candidate_spec" ] && continue
          cand_label=$(basename "$candidate_spec" .md)
          cand_state="${state_dir}/${cand_label}.json"

          effective_base=""
          if [ -f "$cand_state" ]; then
            effective_base=$(jq -r '.base_commit // ""' "$cand_state" 2>/dev/null || echo "")
          fi

          if [ -z "$effective_base" ]; then
            debug "compute_spec_diff: $cand_label has no base_commit, seeding from anchor"
            effective_base="$base_commit"
          elif ! git rev-parse --verify "${effective_base}^{commit}" >/dev/null 2>&1; then
            debug "compute_spec_diff: $cand_label base_commit $effective_base missing, using anchor"
            effective_base="$base_commit"
          elif ! git merge-base --is-ancestor "$effective_base" HEAD 2>/dev/null; then
            debug "compute_spec_diff: $cand_label base_commit $effective_base orphaned, using anchor"
            effective_base="$base_commit"
          fi

          spec_diff=$(git diff "$effective_base" HEAD -- "$candidate_spec" 2>/dev/null || true)
          if [ -n "$spec_diff" ]; then
            echo "=== $candidate_spec ==="
            echo "$spec_diff"
          fi
        done <<< "$candidate_list"

        return 0
      else
        debug "compute_spec_diff: base_commit $base_commit is orphaned (not ancestor of HEAD)"
        # Fall through to tier 2
      fi
    else
      debug "compute_spec_diff: base_commit $base_commit no longer exists"
      # Fall through to tier 2
    fi

    # If --since was used and we get here, the commit was orphaned — that's an error
    if [ -n "$since_commit" ]; then
      error "compute_spec_diff: commit $since_commit is not an ancestor of HEAD"
    fi
  fi

  # Tier 2: molecule exists → fetch existing task list
  if [ -n "$molecule" ]; then
    debug "compute_spec_diff: tier 2 — molecule-based ($molecule)"
    local tasks_output=""
    local tasks_json
    tasks_json=$(bd_json list --parent "$molecule" --json 2>/dev/null || echo "[]")

    if echo "$tasks_json" | jq -e 'type == "array" and length > 0' >/dev/null 2>&1; then
      # Format tasks as markdown for LLM comparison
      tasks_output=$(echo "$tasks_json" | jq -r '.[] | "### \(.id): \(.title) (\(.status))\n\(.description // "")\n"')
    fi

    echo "tasks"
    echo "$tasks_output"
    return 0
  fi

  # Tier 4: neither → new mode
  debug "compute_spec_diff: tier 4 — new mode"
  echo "new"
  echo ""
  return 0
}

#-----------------------------------------------------------------------------
# Spec Cursor Advancement (Per-Spec Fan-Out on RALPH_COMPLETE)
#
# Sets base_commit = <head_commit> on <state_file>. If the state file does
# not exist, creates it with the minimal sibling shape:
#     {label, spec_path, base_commit, companions: []}
# Sibling state files MUST NOT contain molecule/implementation_notes/
# iteration_count — those are anchor-only fields (spec req 21).
#
# Usage: advance_spec_cursor <state_file> <label> <spec_path> <head_commit>
#-----------------------------------------------------------------------------
advance_spec_cursor() {
  local state_file="$1"
  local label="$2"
  local spec_path="$3"
  local head_commit="$4"

  if [ -f "$state_file" ]; then
    jq --arg bc "$head_commit" '.base_commit = $bc' \
      "$state_file" > "$state_file.tmp" && mv "$state_file.tmp" "$state_file"
  else
    mkdir -p "$(dirname "$state_file")"
    jq -n \
      --arg label "$label" \
      --arg spec_path "$spec_path" \
      --arg bc "$head_commit" \
      '{label: $label, spec_path: $spec_path, base_commit: $bc, companions: []}' \
      > "$state_file"
  fi
}

#-----------------------------------------------------------------------------
# Molecule Discovery from README
#
# Parses the configured pinnedContext file to find a molecule ID by spec
# label. Looks for a table row whose Spec column contains <label>.md, then
# extracts the Beads column value. Returns empty string when spec is not in
# the file or when the molecule ID is invalid/not found.
#
# Usage: discover_molecule_from_readme <label>
# Output: molecule ID on stdout (empty string if not found or invalid)
# Returns: 0 always (empty output signals "not found")
#-----------------------------------------------------------------------------
discover_molecule_from_readme() {
  local label="$1"
  local readme
  readme=$(get_pinned_context_file)

  if [ ! -f "$readme" ]; then
    debug "discover_molecule_from_readme: $readme not found"
    return 0
  fi

  # Find the row containing <label>.md in the Spec column and extract the Beads column (column 3)
  local molecule
  molecule=$(awk -F '|' -v pat="${label}.md" '
    $2 ~ pat {
      # Column 4 is the Beads column (1-indexed: empty, Spec, Code, Beads, Purpose, empty)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", $4)
      if ($4 != "" && $4 != "—" && $4 != "-") print $4
    }
  ' "$readme")

  if [ -z "$molecule" ]; then
    debug "discover_molecule_from_readme: no molecule found for label '$label'"
    return 0
  fi

  debug "discover_molecule_from_readme: found molecule '$molecule' for label '$label'"

  # Validate molecule exists via bd show
  if ! bd show "$molecule" >/dev/null 2>&1; then
    debug "discover_molecule_from_readme: molecule '$molecule' is invalid or not found"
    echo ""
    return 0
  fi

  echo "$molecule"
  return 0
}

#-----------------------------------------------------------------------------
# Spec Label Resolution
#
# Resolves the target workflow label for commands that accept --spec/-s.
# Resolution order:
#   1. If a label argument is provided (from --spec/-s flag), use it
#   2. Otherwise, read label from state/current (plain text file)
#   3. If state/current does not exist and no label given, error
#   4. Validate that state/<label>.json exists
#
# Usage: resolve_spec_label [label]
# Output: resolved label on stdout
# Returns: 0 on success, 1 on error (with message to stderr)
#
# Environment:
#   RALPH_DIR — ralph state directory (default: .wrapix/ralph)
#-----------------------------------------------------------------------------

# Resolve the spec label from --spec flag or state/current
# Usage: resolve_spec_label [label]
#   label — explicit label from --spec/-s flag (empty string if not provided)
# Output: resolved label on stdout
# Returns: 0 on success, exits with error on failure
resolve_spec_label() {
  local spec_arg="${1:-}"
  local ralph_dir="${RALPH_DIR:-.wrapix/ralph}"
  local label=""

  if [ -n "$spec_arg" ]; then
    # (1) Explicit --spec/-s flag provided
    label="$spec_arg"
    debug "resolve_spec_label: using explicit label: $label"
  else
    # (2) Read from state/current
    local current_file="$ralph_dir/state/current"
    if [ ! -f "$current_file" ]; then
      error "No active workflow. Run 'ralph plan <label>' first, or use --spec <name> to target a specific workflow."
    fi
    label=$(<"$current_file")
    label="${label#"${label%%[![:space:]]*}"}"  # trim leading whitespace
    label="${label%"${label##*[![:space:]]}"}"  # trim trailing whitespace
    if [ -z "$label" ]; then
      error "state/current is empty. Run 'ralph plan <label>' to set a workflow, or use --spec <name>."
    fi
    debug "resolve_spec_label: read label from state/current: $label"
  fi

  # (3) Validate state/<label>.json exists
  local state_file="$ralph_dir/state/${label}.json"
  if [ ! -f "$state_file" ]; then
    error "Workflow state not found: $state_file — run 'ralph plan $label' to initialize this workflow."
  fi

  debug "resolve_spec_label: validated state file: $state_file"
  echo "$label"
}

#-----------------------------------------------------------------------------
# Compaction re-pin helpers
#
# When Claude auto-compacts a long session, the initial orientation (label,
# spec path, exit signals) can be pushed out of context. Ralph installs a
# SessionStart hook with matcher "compact" that re-injects a condensed
# orientation via hookSpecificOutput.additionalContext.
#
# build_repin_content composes the orientation string from structured keys.
# install_repin_hook writes repin.sh + claude-settings.json under
# .wrapix/ralph/runtime/<label>/ and exports RALPH_RUNTIME_DIR.
#-----------------------------------------------------------------------------

# Compose the condensed re-pin content from known keys.
# Usage: build_repin_content <label> <command> [key=value ...]
#   label    — spec label (e.g. ralph-workflow)
#   command  — invoking ralph command (plan|todo|run|check)
#   keys: spec, mode, molecule, issue, title, companions, base
# Output: plain-text re-pin content on stdout (well under 10KB — excludes the
# full spec body, companion bodies, and issue description).
build_repin_content() {
  local label="$1"
  local command="$2"
  shift 2

  local spec="specs/${label}.md"
  local mode="" molecule="" issue="" title="" companions="" base=""

  local kv key value
  for kv in "$@"; do
    key="${kv%%=*}"
    value="${kv#*=}"
    case "$key" in
      spec) spec="$value" ;;
      mode) mode="$value" ;;
      molecule) molecule="$value" ;;
      issue) issue="$value" ;;
      title) title="$value" ;;
      companions) companions="$value" ;;
      base) base="$value" ;;
      *) warn "build_repin_content: unknown key: $key" ;;
    esac
  done

  {
    echo "# Ralph re-pin (${command})"
    echo ""
    echo "Session was auto-compacted. This orientation was re-injected on the next turn."
    echo ""
    echo "Label: ${label}"
    echo "Spec: ${spec}"
    [ -n "$mode" ] && echo "Mode: ${mode}"
    [ -n "$molecule" ] && echo "Molecule: ${molecule}"
    [ -n "$issue" ] && echo "Issue: ${issue}"
    [ -n "$title" ] && echo "Title: ${title}"
    [ -n "$companions" ] && echo "Companions: ${companions}"
    [ -n "$base" ] && echo "Base commit: ${base}"
    echo ""
    echo "Exit signals: RALPH_COMPLETE, RALPH_BLOCKED: <reason>, RALPH_CLARIFY: <question>"
    echo ""
    echo "Re-read as needed: ${spec}, \`bd show <id>\`, \`bd mol current\`, companion manifests."
  }
}

# Install the SessionStart[compact] re-pin hook under .wrapix/ralph/runtime/<label>/.
# Writes three files:
#   repin.sh              — wraps repin.content into hookSpecificOutput.additionalContext JSON
#   repin.content         — raw orientation text (read by repin.sh via jq --rawfile)
#   claude-settings.json  — hook fragment pointing at repin.sh
# Exports RALPH_RUNTIME_DIR so callers can pass it via wrapix --env.
#
# Usage: install_repin_hook <label> <content>
install_repin_hook() {
  local label="$1"
  local content="$2"
  local ralph_dir="${RALPH_DIR:-.wrapix/ralph}"
  local runtime_dir="${ralph_dir}/runtime/${label}"
  # Path as seen inside the wrapix container (bind mount at /workspace)
  local container_runtime_dir="/workspace/${ralph_dir}/runtime/${label}"

  mkdir -p "$runtime_dir"

  local repin_script="$runtime_dir/repin.sh"
  local content_file="$runtime_dir/repin.content"
  local settings_file="$runtime_dir/claude-settings.json"

  printf '%s' "$content" > "$content_file"

  cat > "$repin_script" <<'REPIN_SCRIPT_EOF'
#!/usr/bin/env bash
set -euo pipefail
dir="$(cd "$(dirname "$0")" && pwd)"
jq -n --rawfile c "$dir/repin.content" \
  '{hookSpecificOutput: {hookEventName: "SessionStart", additionalContext: $c}}'
REPIN_SCRIPT_EOF
  chmod +x "$repin_script"

  jq -n --arg cmd "${container_runtime_dir}/repin.sh" '{
    hooks: {
      SessionStart: [
        {
          matcher: "compact",
          hooks: [
            { type: "command", command: $cmd }
          ]
        }
      ]
    }
  }' > "$settings_file"

  export RALPH_RUNTIME_DIR="$runtime_dir"
}

#-----------------------------------------------------------------------------
# Judge test infrastructure
#
# Judge tests define rubrics via two setter functions:
#   judge_files "file1" "file2" ...   — source files for LLM to evaluate
#   judge_criterion "text"            — what the LLM evaluates
#
# The run_judge function reads those files, constructs a prompt, calls an LLM,
# and returns PASS/FAIL + reasoning.
#-----------------------------------------------------------------------------

# Accumulator variables for judge rubrics (set by judge_files/judge_criterion)
JUDGE_FILES=""
JUDGE_CRITERION=""

# Set source files for LLM evaluation
# Usage: judge_files "lib/ralph/cmd/status.sh" "lib/ralph/cmd/util.sh"
# Multiple files are space-separated
judge_files() {
  JUDGE_FILES="$*"
}

# Set the evaluation criterion text
# Usage: judge_criterion "Output includes progress percentage and status indicators"
judge_criterion() {
  JUDGE_CRITERION="$1"
}

# Reset judge state between test invocations
# Usage: judge_reset
judge_reset() {
  JUDGE_FILES=""
  JUDGE_CRITERION=""
}

# Run LLM judge evaluation
# Reads files from JUDGE_FILES, constructs prompt with JUDGE_CRITERION,
# calls Claude, and parses PASS/FAIL verdict + reasoning.
#
# Usage:
#   judge_files "lib/foo.sh"
#   judge_criterion "Code handles edge cases"
#   run_judge
#
# Output: Sets JUDGE_VERDICT (PASS or FAIL) and JUDGE_REASONING (text)
# Returns: 0 on PASS, 1 on FAIL, 2 on error (LLM unavailable, missing files, etc.)
run_judge() {
  JUDGE_VERDICT=""
  JUDGE_REASONING=""

  if [ -z "$JUDGE_FILES" ]; then
    warn "run_judge: no files specified (call judge_files first)"
    JUDGE_VERDICT="FAIL"
    JUDGE_REASONING="No source files specified for evaluation"
    return 2
  fi

  if [ -z "$JUDGE_CRITERION" ]; then
    warn "run_judge: no criterion specified (call judge_criterion first)"
    JUDGE_VERDICT="FAIL"
    JUDGE_REASONING="No evaluation criterion specified"
    return 2
  fi

  # Read contents of all specified files
  local file_contents=""
  for file in $JUDGE_FILES; do
    if [ ! -f "$file" ]; then
      warn "run_judge: file not found: $file"
      JUDGE_VERDICT="FAIL"
      JUDGE_REASONING="Source file not found: $file"
      return 2
    fi
    file_contents+="
--- $file ---
$(cat "$file")
"
  done

  # Check that claude CLI is available
  if ! command -v claude &>/dev/null; then
    warn "run_judge: claude CLI not found"
    JUDGE_VERDICT="FAIL"
    JUDGE_REASONING="claude CLI not available"
    return 2
  fi

  # Construct the judge prompt
  local prompt
  prompt="You are a code reviewer evaluating whether source code meets a specific criterion.

## Criterion
${JUDGE_CRITERION}

## Source Files
${file_contents}

## Instructions
Evaluate whether the source code meets the criterion above.
Respond with exactly one of these verdicts on the FIRST line:
PASS
FAIL

Then on subsequent lines, provide a brief explanation (1-3 sentences) of your reasoning.

Example response:
PASS
The code implements progress percentage display via the calc_progress function and shows status indicators for each issue state."

  debug "run_judge: evaluating criterion: $JUDGE_CRITERION"
  debug "run_judge: files: $JUDGE_FILES"

  # Pipe via stdin — prompt can exceed ARG_MAX when specs reference many files.
  local llm_output exit_code
  llm_output=$(printf '%s' "$prompt" | claude --dangerously-skip-permissions --print 2>&1) && exit_code=0 || exit_code=$?

  if [ $exit_code -ne 0 ]; then
    warn "run_judge: claude invocation failed (exit $exit_code)"
    JUDGE_VERDICT="FAIL"
    JUDGE_REASONING="LLM invocation failed (exit $exit_code): ${llm_output:0:200}"
    return 2
  fi

  # Parse verdict from first non-empty line
  local first_line
  first_line=$(echo "$llm_output" | grep -m1 -E '^(PASS|FAIL)' || true)

  if [ -z "$first_line" ]; then
    warn "run_judge: could not parse PASS/FAIL from LLM response"
    JUDGE_VERDICT="FAIL"
    JUDGE_REASONING="Could not parse verdict from LLM response: ${llm_output:0:200}"
    return 2
  fi

  JUDGE_VERDICT="${first_line%%[[:space:]]*}"
  # Extract reasoning: everything after the verdict line
  JUDGE_REASONING=$(echo "$llm_output" | sed '1{/^$/d}' | tail -n +2 | sed '/^$/d' | head -5)

  if [ -z "$JUDGE_REASONING" ]; then
    JUDGE_REASONING="(no reasoning provided)"
  fi

  debug "run_judge: verdict=$JUDGE_VERDICT"

  if [ "$JUDGE_VERDICT" = "PASS" ]; then
    return 0
  else
    return 1
  fi
}

# -------- Shared scaffold helpers (used by ralph init and ralph sync) --------
#
# scaffold_docs, scaffold_agents, scaffold_templates are the single code path
# that both `ralph init` and `ralph sync` invoke to materialize docs/,
# AGENTS.md, and .wrapix/ralph/template/. All are skip-if-exists and rely on
# the DRY_RUN global (defaults to "false" when unset) so sync's dry-run mode
# continues to work; init never sets DRY_RUN.

DRY_RUN="${DRY_RUN:-false}"

# Prefix string for dry-run-aware action messages.
prefix() {
  if [ "$DRY_RUN" = "true" ]; then
    echo "[dry-run] "
  else
    echo ""
  fi
}

# Print an action line with optional dry-run prefix.
action() {
  echo "$(prefix)$*"
}

# Create a directory if it does not exist (dry-run aware).
ensure_dir() {
  local dir="$1"
  if [ ! -d "$dir" ]; then
    action "Creating directory: $dir"
    if [ "$DRY_RUN" = "false" ]; then
      mkdir -p "$dir"
    fi
  fi
}

# Copy a file with action logging (dry-run aware, makes dest writable).
copy_file() {
  local src="$1"
  local dst="$2"
  local name="${3:-$(basename "$src")}"

  action "Copying: $name"
  debug "  from: $src"
  debug "  to:   $dst"

  if [ "$DRY_RUN" = "false" ]; then
    if [ -f "$dst" ]; then
      rm -f "$dst"
    fi
    cp "$src" "$dst"
    chmod 644 "$dst"
  fi
}

# List top-level files matching one or more patterns in a directory.
# Usage: list_files "$dir" "*.md" "*.nix"
list_files() {
  local dir="$1"
  shift

  if [ ! -d "$dir" ]; then
    return 0
  fi

  for pattern in "$@"; do
    find "$dir" -maxdepth 1 -type f -name "$pattern" || true
  done
}

# Default content for docs/README.md
DOCS_README_CONTENT='# Project Overview

<!-- Scaffolded by ralph sync — edit this file, then close the review bead -->

## Summary

Describe what this project does, who it is for, and its key capabilities.

## Specs

Individual spec files live in `specs/`. Add a row when each spec lands.

| Spec | Code | Beads | Purpose |
|------|------|-------|---------|
| _add rows as specs land_ | | | |

## Terminology

| Term | Definition |
|------|------------|
| Example | Replace with project-specific terms |
'

# Default content for docs/architecture.md
DOCS_ARCHITECTURE_CONTENT='# Architecture

<!-- Scaffolded by ralph sync — edit this file, then close the review bead -->

## System Design

Describe the high-level architecture: components, data flow, and key boundaries.

## Design Principles

1. Describe the guiding principles for this system.

## Source Layout

```
Describe the directory structure and what each area is responsible for.
```
'

# Default content for docs/style-guidelines.md
DOCS_STYLE_CONTENT='# Style Guidelines

<!-- Scaffolded by ralph sync — edit this file, then close the review bead -->

## Code Standards

Describe formatting, naming conventions, and patterns to follow.

## Review Criteria

- What reviewers should check for
- What patterns to avoid
'

# Default content for AGENTS.md (mirrored to CLAUDE.md via symlink)
DOCS_AGENTS_CONTENT='# Agent Instructions

<!-- Scaffolded by ralph sync — edit this file, then close the review bead -->
<!-- Recommended: `ln -s AGENTS.md CLAUDE.md` so Claude Code reads the same file -->

## Project Context

- **Overview & specs** — `docs/README.md` (pinned at session start)
- **Architecture** — `docs/architecture.md`
- **Style guidelines** — `docs/style-guidelines.md`
- **Individual specs** — `specs/<label>.md`

## Building

```bash
nix develop          # Enter devShell
nix build            # Build this project
```

## Issue Tracking (Beads)

Use `bd` for ALL issue tracking. Do NOT use markdown TODOs or external trackers.

```bash
bd ready                          # Show unblocked work
bd show <id>                      # Issue details
bd create --title="..." --description="..." --type=task --priority=2
bd update <id> --status=in_progress   # Claim before starting
bd close <id>                     # Mark complete
bd dep add <issue> <depends-on>   # Add dependency
```

**Priority:** 0-4 (critical to backlog, default 2). **Types:** task, bug, feature, epic.

## Session Protocol

### Start

```bash
bd dolt pull
```

### End ("land the plane")

```bash
git add <files>
git commit -m "..."
git push
beads-push            # Sync beads branch to GitHub
```

Work is NOT complete until both pushes succeed.

## Hidden Specs

Files in `.wrapix/ralph/state/` are hidden specs managed by ralph. Never copy or
commit them to `specs/`.
'

# Default content for docs/orchestration.md (Gas City projects only)
DOCS_ORCHESTRATION_CONTENT='# Orchestration

<!-- Scaffolded by ralph sync — edit this file, then close the review bead -->

## Deploy Commands

Describe how to deploy changes (e.g., `nix build`, `podman restart`).

## Scout Rules

Error patterns the scout watches for in service container logs.

### Immediate (P0 bead)

```
FATAL|PANIC|panic:
```

### Batched (collected over one poll cycle)

```
ERROR|Exception
```

### Ignore

```
# Add patterns for known noise
```

## Auto-deploy

<!-- Define criteria for changes that can be deployed without director approval -->
<!-- Remove this section or leave empty to require director approval for all deploys -->
'

# Detect whether the flake uses mkCity (Gas City integration).
flake_uses_mkcity() {
  if [ -f "flake.nix" ] && grep -q 'mkCity' "flake.nix"; then
    return 0
  fi
  return 1
}

# Scaffold a single docs file and create a review bead.
# Usage: scaffold_doc <filepath> <content> <description>
# Returns: 0 if created, 1 if already exists
scaffold_doc() {
  local filepath="$1"
  local content="$2"
  local description="$3"

  if [ -f "$filepath" ]; then
    debug "Docs file already exists: $filepath"
    return 1
  fi

  action "Scaffolding: $filepath"

  if [ "$DRY_RUN" = "false" ]; then
    mkdir -p "$(dirname "$filepath")"
    echo "$content" > "$filepath"

    if command -v bd &>/dev/null; then
      local bead_id
      bead_id=$(bd create \
        --title="Review scaffolded $filepath" \
        --description="$description" \
        --type=task \
        --priority=2 \
        --labels="ralph:scaffold,human" \
        --silent) || true
      if [ -n "$bead_id" ]; then
        action "Created review bead: $bead_id for $filepath"
      fi
    fi
  fi

  return 0
}

# Scaffold project docs (docs/README.md, docs/architecture.md,
# docs/style-guidelines.md, and docs/orchestration.md when mkCity is detected).
# AGENTS.md is scaffolded separately by scaffold_agents.
scaffold_docs() {
  local scaffolded=0

  echo ""
  echo "Checking docs scaffolding..."

  if scaffold_doc "docs/README.md" \
    "$DOCS_README_CONTENT" \
    "Review and customize the scaffolded project overview (docs/README.md). Add project-specific terminology and description."; then
    scaffolded=$((scaffolded + 1))
  fi

  if scaffold_doc "docs/architecture.md" \
    "$DOCS_ARCHITECTURE_CONTENT" \
    "Review and customize the scaffolded architecture document (docs/architecture.md). Describe the system design, components, and key boundaries."; then
    scaffolded=$((scaffolded + 1))
  fi

  if scaffold_doc "docs/style-guidelines.md" \
    "$DOCS_STYLE_CONTENT" \
    "Review and customize the scaffolded style guidelines (docs/style-guidelines.md). Define code standards and review criteria."; then
    scaffolded=$((scaffolded + 1))
  fi

  if flake_uses_mkcity; then
    if scaffold_doc "docs/orchestration.md" \
      "$DOCS_ORCHESTRATION_CONTENT" \
      "Review and customize the scaffolded orchestration config (docs/orchestration.md). Define deploy commands, scout error patterns, and auto-deploy criteria."; then
      scaffolded=$((scaffolded + 1))
    fi
  fi

  if [ "$scaffolded" -gt 0 ]; then
    echo "Scaffolded $scaffolded docs file(s) — review beads created for director approval"
  else
    echo "All docs files already exist"
  fi
}

# Scaffold AGENTS.md (skip-if-exists). CLAUDE.md symlinking is handled by
# bootstrap_claude_symlink during ralph init; sync users create the symlink
# manually per the scaffolded content's recommendation.
scaffold_agents() {
  if scaffold_doc "AGENTS.md" \
    "$DOCS_AGENTS_CONTENT" \
    "Review and customize the scaffolded agent instructions (AGENTS.md). Adjust build commands, session protocol, and tooling references to match this project. Optionally create a CLAUDE.md symlink: ln -s AGENTS.md CLAUDE.md"; then
    echo "Scaffolded AGENTS.md — review bead created for director approval"
  else
    echo "AGENTS.md already exists"
  fi
}

# Scaffold .wrapix/ralph/template/ from RALPH_TEMPLATE_DIR (skip-if-exists).
# Init-time first-time scaffolding — ralph sync performs a fuller
# backup/refresh via copy_fresh_templates.
scaffold_templates() {
  local ralph_dir="${RALPH_DIR:-.wrapix/ralph}"
  local target="$ralph_dir/template"
  local src="${RALPH_TEMPLATE_DIR:-}"

  if [ -d "$target" ]; then
    echo "Templates already present at: $target"
    return 1
  fi

  if [ -z "$src" ] || [ ! -d "$src" ]; then
    warn "RALPH_TEMPLATE_DIR not set or missing: $src"
    return 2
  fi

  ensure_dir "$target"

  local file_count=0
  while IFS= read -r src_file; do
    [ -f "$src_file" ] || continue
    local name
    name=$(basename "$src_file")
    if [ "$name" = "default.nix" ] || [ "$name" = "config.nix" ]; then
      debug "Skipping $name: internal Nix file"
      continue
    fi
    copy_file "$src_file" "$target/$name" "$name"
    file_count=$((file_count + 1))
  done < <(list_files "$src" "*.md" "*.nix")

  local src_partial="$src/partial"
  if [ -d "$src_partial" ]; then
    local target_partial="$target/partial"
    ensure_dir "$target_partial"
    while IFS= read -r src_file; do
      [ -f "$src_file" ] || continue
      local name
      name=$(basename "$src_file")
      copy_file "$src_file" "$target_partial/$name" "partial/$name"
      file_count=$((file_count + 1))
    done < <(list_files "$src_partial" "*.md")
  fi

  echo "Scaffolded $file_count template file(s) to: $target"
  return 0
}

# -------- ralph init bootstrap helpers --------
#
# Each helper creates one init-only artifact in the current directory and is
# idempotent: skip-if-exists, never overwrites. Helpers set BOOTSTRAP_DETAIL
# to a parenthetical string for the summary (e.g. "4 entries appended",
# "already initialized", "-> AGENTS.md") and return:
#   0 — created/modified
#   1 — skipped (artifact already present)
#   2 — error
#
# Helpers print nothing themselves. The caller (init.sh) renders the summary.

export BOOTSTRAP_DETAIL=""

# Copy $RALPH_TEMPLATE_DIR/flake.nix to ./flake.nix.
bootstrap_flake() {
  BOOTSTRAP_DETAIL=""
  if [ -e flake.nix ]; then
    BOOTSTRAP_DETAIL="already exists"
    return 1
  fi
  local src="${RALPH_TEMPLATE_DIR:-}/flake.nix"
  if [ ! -f "$src" ]; then
    BOOTSTRAP_DETAIL="template not found: $src"
    return 2
  fi
  cp "$src" flake.nix
  return 0
}

# Write 'use flake' to ./.envrc.
bootstrap_envrc() {
  BOOTSTRAP_DETAIL=""
  if [ -e .envrc ]; then
    BOOTSTRAP_DETAIL="already exists"
    return 1
  fi
  printf 'use flake\n' > .envrc
  return 0
}

# Append missing entries to ./.gitignore. Creates the file if absent.
bootstrap_gitignore() {
  BOOTSTRAP_DETAIL=""
  local entries=('.direnv/' '.wrapix/' 'result' 'result-*')
  local missing=()
  local entry
  for entry in "${entries[@]}"; do
    if [ ! -f .gitignore ] || ! grep -Fxq -- "$entry" .gitignore; then
      missing+=("$entry")
    fi
  done
  if [ ${#missing[@]} -eq 0 ]; then
    BOOTSTRAP_DETAIL="all entries present"
    return 1
  fi
  # Ensure trailing newline before appending so entries land on their own line.
  if [ -f .gitignore ] && [ -s .gitignore ] && [ -n "$(tail -c 1 .gitignore)" ]; then
    printf '\n' >> .gitignore
  fi
  for entry in "${missing[@]}"; do
    printf '%s\n' "$entry" >> .gitignore
  done
  local n=${#missing[@]}
  if [ "$n" -eq 1 ]; then
    BOOTSTRAP_DETAIL="1 entry appended"
  else
    BOOTSTRAP_DETAIL="${n} entries appended"
  fi
  return 0
}

# Copy $RALPH_TEMPLATE_DIR/pre-commit-config.yaml to ./.pre-commit-config.yaml.
bootstrap_precommit() {
  BOOTSTRAP_DETAIL=""
  if [ -e .pre-commit-config.yaml ]; then
    BOOTSTRAP_DETAIL="already exists"
    return 1
  fi
  local src="${RALPH_TEMPLATE_DIR:-}/pre-commit-config.yaml"
  if [ ! -f "$src" ]; then
    BOOTSTRAP_DETAIL="template not found: $src"
    return 2
  fi
  cp "$src" .pre-commit-config.yaml
  return 0
}

# Run 'bd init' if .beads/ does not exist.
bootstrap_beads() {
  BOOTSTRAP_DETAIL=""
  if [ -d .beads ]; then
    BOOTSTRAP_DETAIL="already initialized"
    return 1
  fi
  if ! command -v bd >/dev/null 2>&1; then
    BOOTSTRAP_DETAIL="bd not on PATH"
    return 2
  fi
  if ! bd init; then
    BOOTSTRAP_DETAIL="bd init failed"
    return 2
  fi
  return 0
}

# Create CLAUDE.md as a symlink to AGENTS.md when CLAUDE.md is absent.
bootstrap_claude_symlink() {
  BOOTSTRAP_DETAIL=""
  if [ -e CLAUDE.md ] || [ -L CLAUDE.md ]; then
    BOOTSTRAP_DETAIL="already exists"
    return 1
  fi
  ln -s AGENTS.md CLAUDE.md
  BOOTSTRAP_DETAIL="-> AGENTS.md"
  return 0
}
