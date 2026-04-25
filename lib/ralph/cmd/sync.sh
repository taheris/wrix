#!/usr/bin/env bash
set -euo pipefail

# ralph sync [--dry-run] [--diff]
# Synchronizes local templates with packaged versions
# - Creates .wrapix/ralph/template/ with fresh packaged templates
# - Backs up existing customized templates to .wrapix/ralph/backup/
# - Copies all templates including partial/ directory
# - Verbose by default (prints actions taken)
# - Use --diff to show changes without syncing

# Load shared helpers
# shellcheck source=util.sh
source "$(dirname "$0")/util.sh"

RALPH_DIR="${RALPH_DIR:-.wrapix/ralph}"

# GitHub repo and branch for fetching templates when RALPH_TEMPLATE_DIR not set
RALPH_GITHUB_REPO="${RALPH_GITHUB_REPO:-taheris/wrapix}"
RALPH_GITHUB_REF="${RALPH_GITHUB_REF:-main}"

# Template directory: use RALPH_TEMPLATE_DIR if set and exists
if [ -n "${RALPH_TEMPLATE_DIR:-}" ] && [ -d "$RALPH_TEMPLATE_DIR" ]; then
  PACKAGED_DIR="$RALPH_TEMPLATE_DIR"
  FETCH_FROM_GITHUB=false
else
  PACKAGED_DIR=""
  FETCH_FROM_GITHUB=true
fi

# Templates to compare (base names without .md)
# Main templates in .wrapix/ralph/template/
DIFF_TEMPLATES=(
  "plan-new"
  "plan-update"
  "todo-new"
  "todo-update"
  "run"
)

# Partials in .wrapix/ralph/template/partial/
DIFF_PARTIALS=(
  "context-pinning"
  "exit-signals"
  "spec-header"
)

# Fetch templates from GitHub to a temp directory
# Returns: path to temp directory containing templates
fetch_github_templates() {
  local temp_dir
  temp_dir=$(mktemp -d)

  # List of template files to fetch
  local base_url="https://raw.githubusercontent.com/${RALPH_GITHUB_REPO}/${RALPH_GITHUB_REF}/lib/ralph/template"

  local files=(
    "config.nix"
    "plan-new.md"
    "plan-update.md"
    "todo-new.md"
    "todo-update.md"
    "run.md"
  )

  local partials=(
    "context-pinning.md"
    "exit-signals.md"
    "spec-header.md"
  )

  echo "Fetching templates from GitHub: $RALPH_GITHUB_REPO (ref: $RALPH_GITHUB_REF)" >&2

  # Fetch main template files
  local failed=false
  for file in "${files[@]}"; do
    local url="$base_url/$file"
    local dest="$temp_dir/$file"

    if [ "$DRY_RUN" = "true" ]; then
      echo "[dry-run] Would fetch: $file" >&2
    else
      if ! curl -sSfL "$url" -o "$dest"; then
        warn "Failed to fetch: $file from $url"
        failed=true
      else
        debug "Fetched: $file"
      fi
    fi
  done

  # Fetch partial files
  mkdir -p "$temp_dir/partial"
  for file in "${partials[@]}"; do
    local url="$base_url/partial/$file"
    local dest="$temp_dir/partial/$file"

    if [ "$DRY_RUN" = "true" ]; then
      echo "[dry-run] Would fetch: partial/$file" >&2
    else
      if ! curl -sSfL "$url" -o "$dest"; then
        warn "Failed to fetch: partial/$file from $url"
        failed=true
      else
        debug "Fetched: partial/$file"
      fi
    fi
  done

  if [ "$failed" = "true" ]; then
    rm -rf "$temp_dir"
    return 1
  fi

  echo "$temp_dir"
}

# Parse arguments
DRY_RUN=false
DIFF_MODE=false
DEPS_MODE=false
DIFF_TEMPLATE_NAME=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run|-d)
      DRY_RUN=true
      shift
      ;;
    --diff)
      DIFF_MODE=true
      shift
      # Optional template name after --diff
      if [[ $# -gt 0 ]] && [[ ! "$1" =~ ^- ]]; then
        DIFF_TEMPLATE_NAME="$1"
        shift
      fi
      ;;
    --deps)
      DEPS_MODE=true
      shift
      ;;
    -h|--help)
      echo "Usage: ralph sync [--dry-run] [--diff [template-name]] [--deps]"
      echo ""
      echo "Synchronizes local templates with packaged versions."
      echo ""
      echo "Options:"
      echo "  --dry-run, -d  Preview changes without executing"
      echo "  --diff [name]  Show local template changes vs packaged (no sync)"
      echo "                 If template-name given, diff only that template/partial"
      echo "  --deps         Show required nix packages for current spec's tests"
      echo "  --help, -h     Show this help message"
      echo ""
      echo "Sync Actions:"
      echo "  1. Creates .wrapix/ralph/template/ with fresh packaged templates"
      echo "  2. Backs up existing customized templates to .wrapix/ralph/backup/"
      echo "  3. Copies all templates including partial/ directory"
      echo ""
      echo "Templates (for --diff):"
      echo "  plan, plan-new, plan-update, todo, todo-new, todo-update, step"
      echo ""
      echo "Partials (for --diff):"
      echo "  context-pinning, exit-signals, spec-header"
      echo ""
      echo "After sync, use 'ralph sync --diff' to see what changed,"
      echo "then 'ralph tune' to merge customizations from backup."
      echo ""
      echo "Examples:"
      echo "  ralph sync                    # Sync all templates"
      echo "  ralph sync --dry-run          # Preview sync without changes"
      echo "  ralph sync --diff             # Show all template changes"
      echo "  ralph sync --diff run         # Show run.md changes only"
      echo "  ralph sync --diff | ralph tune  # Pipe to tune for integration"
      echo "  ralph sync --deps             # List nix packages for spec tests"
      echo ""
      echo "Environment:"
      echo "  RALPH_DIR           Local ralph directory (default: .wrapix/ralph)"
      echo "  RALPH_TEMPLATE_DIR  Packaged template directory (from nix develop)"
      echo "  RALPH_GITHUB_REPO   GitHub repo for templates (default: taheris/wrapix)"
      echo "  RALPH_GITHUB_REF    Git ref to fetch (default: main)"
      echo ""
      echo "If RALPH_TEMPLATE_DIR is not set, templates are fetched from GitHub."
      exit 0
      ;;
    *)
      error "Unknown option: $1
Run 'ralph sync --help' for usage."
      ;;
  esac
done

# Fetch templates from GitHub if RALPH_TEMPLATE_DIR not set
CLEANUP_TEMP_DIR=""
if [ "$FETCH_FROM_GITHUB" = "true" ]; then
  PACKAGED_DIR=$(fetch_github_templates)
  if [ -z "$PACKAGED_DIR" ] || [ ! -d "$PACKAGED_DIR" ]; then
    error "Failed to fetch templates from GitHub.

Check network connectivity or run from 'nix develop' shell which sets RALPH_TEMPLATE_DIR."
  fi
  CLEANUP_TEMP_DIR="$PACKAGED_DIR"
fi

# Cleanup temp directory on exit (only if we created one)
cleanup() {
  if [ -n "$CLEANUP_TEMP_DIR" ] && [ -d "$CLEANUP_TEMP_DIR" ]; then
    rm -rf "$CLEANUP_TEMP_DIR"
    debug "Cleaned up temp directory: $CLEANUP_TEMP_DIR"
  fi
}
trap cleanup EXIT

# Directories
TEMPLATES_DIR="$RALPH_DIR/template"
BACKUP_DIR="$RALPH_DIR/backup"

# prefix, action, ensure_dir, copy_file, list_files are sourced from util.sh

# Backup existing templates if they differ from packaged
backup_existing() {
  local templates_dir="$1"
  local backup_dir="$2"
  local packaged_dir="$3"

  if [ ! -d "$templates_dir" ]; then
    debug "No existing templates to backup"
    return 0
  fi

  local has_backups=false

  # Check each file in templates directory
  while IFS= read -r local_file; do
    [ -f "$local_file" ] || continue

    local name
    name=$(basename "$local_file")
    local packaged_file="$packaged_dir/$name"

    # Skip if no packaged version exists
    if [ ! -f "$packaged_file" ]; then
      debug "Skipping $name: no packaged version"
      continue
    fi

    # Check if local differs from packaged
    if ! diff -q "$packaged_file" "$local_file" >/dev/null 2>&1; then
      if [ "$has_backups" = "false" ]; then
        ensure_dir "$backup_dir"
        has_backups=true
      fi

      action "Backing up: $name (has local changes)"
      if [ "$DRY_RUN" = "false" ]; then
        cp "$local_file" "$backup_dir/$name"
      fi
    else
      debug "$name: matches packaged, no backup needed"
    fi
  done < <(list_files "$templates_dir" "*.md" "*.nix")

  # Handle partial directory
  local partial_dir="$templates_dir/partial"
  if [ -d "$partial_dir" ]; then
    local packaged_partial="$packaged_dir/partial"

    while IFS= read -r local_file; do
      [ -f "$local_file" ] || continue

      local name
      name=$(basename "$local_file")
      local packaged_file="$packaged_partial/$name"

      if [ ! -f "$packaged_file" ]; then
        debug "Skipping partial/$name: no packaged version"
        continue
      fi

      if ! diff -q "$packaged_file" "$local_file" >/dev/null 2>&1; then
        if [ "$has_backups" = "false" ]; then
          ensure_dir "$backup_dir"
          has_backups=true
        fi

        ensure_dir "$backup_dir/partial"
        action "Backing up: partial/$name (has local changes)"
        if [ "$DRY_RUN" = "false" ]; then
          cp "$local_file" "$backup_dir/partial/$name"
        fi
      fi
    done < <(list_files "$partial_dir" "*.md")
  fi

  if [ "$has_backups" = "true" ]; then
    echo ""
    echo "Customizations backed up to: $backup_dir"
    echo "Use 'ralph tune' to merge them after reviewing with 'ralph sync --diff'."
  fi
}

# Copy fresh templates from packaged directory
copy_fresh_templates() {
  local packaged_dir="$1"
  local templates_dir="$2"

  ensure_dir "$templates_dir"

  echo ""
  echo "Copying fresh templates from: $packaged_dir"

  # Copy top-level template files
  local file_count=0
  while IFS= read -r src_file; do
    [ -f "$src_file" ] || continue

    # Skip Nix internals - not user templates
    local name
    name=$(basename "$src_file")
    if [ "$name" = "default.nix" ] || [ "$name" = "config.nix" ]; then
      debug "Skipping $name: internal Nix file"
      continue
    fi

    copy_file "$src_file" "$templates_dir/$name" "$name"
    file_count=$((file_count + 1))
  done < <(list_files "$packaged_dir" "*.md" "*.nix")

  # Copy partial directory
  local packaged_partial="$packaged_dir/partial"
  if [ -d "$packaged_partial" ]; then
    local partial_dir="$templates_dir/partial"
    ensure_dir "$partial_dir"

    while IFS= read -r src_file; do
      [ -f "$src_file" ] || continue

      local name
      name=$(basename "$src_file")
      copy_file "$src_file" "$partial_dir/$name" "partial/$name"
      file_count=$((file_count + 1))
    done < <(list_files "$packaged_partial" "*.md")
  fi

  echo ""
  echo "Copied $file_count template files to: $templates_dir"
}

# === Diff Mode Functions ===

# Show template differences between local and packaged versions
# Usage: show_template_diff [template-name]
show_template_diff() {
  local template_name="${1:-}"
  local templates_to_diff=("${DIFF_TEMPLATES[@]}")
  local partials_to_diff=("${DIFF_PARTIALS[@]}")
  local filter_partial=""

  # If specific template requested, validate it
  if [ -n "$template_name" ]; then
    # Normalize: remove .md suffix if provided
    template_name="${template_name%.md}"

    # Check if it's a template
    local valid_template=false
    for t in "${templates_to_diff[@]}"; do
      if [ "$t" = "$template_name" ]; then
        valid_template=true
        break
      fi
    done

    # Check if it's a partial
    local valid_partial=false
    for p in "${partials_to_diff[@]}"; do
      if [ "$p" = "$template_name" ]; then
        valid_partial=true
        break
      fi
    done

    if [ "$valid_template" = "false" ] && [ "$valid_partial" = "false" ]; then
      error "Unknown template or partial: $template_name

Valid templates: ${templates_to_diff[*]}
Valid partials: ${partials_to_diff[*]}"
    fi

    if [ "$valid_template" = "true" ]; then
      # Only diff the requested template
      templates_to_diff=("$template_name")
      partials_to_diff=()
    else
      # Only diff the requested partial
      templates_to_diff=()
      filter_partial="$template_name"
    fi
  fi

  # Validate local ralph directory exists
  if [ ! -d "$RALPH_DIR" ]; then
    echo "No local templates found at $RALPH_DIR"
    echo "Run 'ralph plan <label>' first to initialize local templates."
    return 0
  fi

  # Track changes and collect output
  local has_changes=false
  local diff_output=""

  # Diff templates
  for template in "${templates_to_diff[@]}"; do
    local local_file="$RALPH_DIR/template/${template}.md"
    local packaged_file="$PACKAGED_DIR/${template}.md"

    # Skip if local file doesn't exist (not customized)
    if [ ! -f "$local_file" ]; then
      debug "Skipping $template: no local file"
      continue
    fi

    # Skip if packaged file doesn't exist (shouldn't happen)
    if [ ! -f "$packaged_file" ]; then
      warn "Packaged template not found: $packaged_file"
      continue
    fi

    # Compare files
    if ! diff -q "$packaged_file" "$local_file" >/dev/null 2>&1; then
      has_changes=true

      # Generate unified diff with header
      local diff_result
      diff_result=$(diff -u \
        --label "packaged/${template}.md" \
        --label "local/${template}.md" \
        "$packaged_file" "$local_file" || true)  # diff exits 1 when files differ (expected)

      if [ -n "$diff_result" ]; then
        diff_output+="
### Template: ${template}.md
\`\`\`diff
$diff_result
\`\`\`
"
      fi
    else
      debug "$template: no changes"
    fi
  done

  # Diff partials
  diff_partial_file() {
    local partial="$1"
    local local_file="$RALPH_DIR/template/partial/${partial}.md"
    local packaged_file="$PACKAGED_DIR/partial/${partial}.md"

    # Skip if local file doesn't exist (not customized)
    if [ ! -f "$local_file" ]; then
      debug "Skipping partial/$partial: no local file"
      return
    fi

    # Skip if packaged file doesn't exist (shouldn't happen)
    if [ ! -f "$packaged_file" ]; then
      warn "Packaged partial not found: $packaged_file"
      return
    fi

    # Compare files
    if ! diff -q "$packaged_file" "$local_file" >/dev/null 2>&1; then
      has_changes=true

      # Generate unified diff with header
      local diff_result
      diff_result=$(diff -u \
        --label "packaged/partial/${partial}.md" \
        --label "local/partial/${partial}.md" \
        "$packaged_file" "$local_file" || true)  # diff exits 1 when files differ (expected)

      if [ -n "$diff_result" ]; then
        diff_output+="
### Partial: partial/${partial}.md
\`\`\`diff
$diff_result
\`\`\`
"
      fi
    else
      debug "partial/$partial: no changes"
    fi
  }

  # Diff partials (either all or filtered)
  if [ -n "$filter_partial" ]; then
    diff_partial_file "$filter_partial"
  else
    for partial in "${partials_to_diff[@]}"; do
      diff_partial_file "$partial"
    done
  fi

  # Output results
  if [ "$has_changes" = "true" ]; then
    echo "# Local Template Changes"
    echo ""
    echo "Comparing local templates (\`$RALPH_DIR\`) against packaged templates (\`$PACKAGED_DIR\`)."
    echo "$diff_output"

    # Hint for integration mode (only if stdout is a tty)
    if [ -t 1 ]; then
      echo ""
      echo "---"
      echo "Pipe to 'ralph tune' for interactive integration:"
      echo "  ralph sync --diff | ralph tune"
    fi
  else
    if [ -n "$template_name" ]; then
      echo "No changes in ${template_name}.md"
    else
      echo "No local template changes found."
      echo ""
      echo "Local templates match packaged templates."
    fi
  fi
}

# === Deps Mode Functions ===

# Map of command names to nixpkgs package names
# Usage: tool_to_nix_package "curl" -> "curl"
tool_to_nix_package() {
  local tool="$1"
  case "$tool" in
    curl)     echo "curl" ;;
    jq)       echo "jq" ;;
    tmux)     echo "tmux" ;;
    python|python3) echo "python3" ;;
    node|nodejs)    echo "nodejs" ;;
    git)      echo "git" ;;
    rsync)    echo "rsync" ;;
    wget)     echo "wget" ;;
    ssh|scp)  echo "openssh" ;;
    socat)    echo "socat" ;;
    nc|ncat|netcat) echo "netcat" ;;
    dig|nslookup)   echo "dnsutils" ;;
    sqlite3)  echo "sqlite" ;;
    psql)     echo "postgresql" ;;
    docker)   echo "docker" ;;
    podman)   echo "podman" ;;
    nix)      echo "nix" ;;
    shellcheck) echo "shellcheck" ;;
    shfmt)    echo "shfmt" ;;
    rg|ripgrep)  echo "ripgrep" ;;
    fd)       echo "fd" ;;
    fzf)      echo "fzf" ;;
    bat)      echo "bat" ;;
    diff)     echo "diffutils" ;;
    patch)    echo "patch" ;;
    make)     echo "gnumake" ;;
    gcc|cc)   echo "gcc" ;;
    go)       echo "go" ;;
    cargo|rustc) echo "rustc" ;;
    *)        return 1 ;;
  esac
}

# Scan a file for tool references and return nix package names
# Usage: scan_file_for_deps "tests/notify-test.sh"
# Output: one nix package per line (deduplicated within this file)
scan_file_for_deps() {
  local file="$1"

  if [ ! -f "$file" ]; then
    debug "File not found for dep scan: $file"
    return 0
  fi

  local content
  content=$(cat "$file")

  # Known tools to scan for — check command usage patterns
  local tools=(
    curl jq tmux python python3 node nodejs git rsync wget
    ssh scp socat nc ncat netcat dig nslookup sqlite3 psql
    docker podman nix shellcheck shfmt rg ripgrep fd fzf bat
    diff patch make gcc cc go cargo rustc
  )

  local found_pkgs=()
  for tool in "${tools[@]}"; do
    # Match tool as a command: at start of line, after pipe, in $(), after &&/||,
    # or as argument to command -v / which / type
    # Use word-boundary-like matching to avoid false positives
    if echo "$content" | grep -qE "(^|[[:space:]|;&\(])${tool}([[:space:]|;&\)]|$)"; then
      local pkg
      if pkg=$(tool_to_nix_package "$tool"); then
        found_pkgs+=("$pkg")
      fi
    fi
  done

  # Deduplicate and output
  printf '%s\n' "${found_pkgs[@]}" | sort -u
}

# Show required nix packages for current spec's verify/judge tests
show_deps() {
  # Find current spec via state/current pointer + per-label state JSON
  local current_pointer="$RALPH_DIR/state/current"
  if [ ! -f "$current_pointer" ]; then
    error "No active feature. Run 'ralph plan <label>' first."
  fi

  local label spec_file
  label=$(<"$current_pointer")
  label="${label#"${label%%[![:space:]]*}"}"
  label="${label%"${label##*[![:space:]]}"}"

  if [ -z "$label" ]; then
    error "No label in state/current. Run 'ralph plan <label>' first."
  fi

  local state_file="$RALPH_DIR/state/${label}.json"
  if [ ! -f "$state_file" ]; then
    error "No state file for '$label'. Run 'ralph plan $label' first."
  fi

  spec_file=$(jq -r '.spec_path // empty' "$state_file")
  if [ -z "$spec_file" ]; then
    spec_file="specs/$label.md"
  fi

  if [ ! -f "$spec_file" ]; then
    error "Spec file not found: $spec_file"
  fi

  debug "Scanning annotations in: $spec_file"

  # Parse annotations from the spec
  local annotations
  annotations=$(parse_spec_annotations "$spec_file") || {
    echo "No success criteria found in $spec_file" >&2
    return 0
  }

  if [ -z "$annotations" ]; then
    echo "No success criteria found in $spec_file" >&2
    return 0
  fi

  # Collect all unique test files from annotations
  local test_files=()
  while IFS=$'\t' read -r _criterion ann_type file_path _function_name _checked; do
    if [ "$ann_type" = "verify" ] || [ "$ann_type" = "judge" ]; then
      if [ -n "$file_path" ]; then
        test_files+=("$file_path")
      fi
    fi
  done <<< "$annotations"

  if [ ${#test_files[@]} -eq 0 ]; then
    debug "No annotated test files found"
    return 0
  fi

  # Deduplicate test files
  local unique_files
  unique_files=$(printf '%s\n' "${test_files[@]}" | sort -u)

  # Scan each test file for deps
  local all_pkgs=()
  while IFS= read -r file; do
    debug "Scanning for deps: $file"
    while IFS= read -r pkg; do
      [ -n "$pkg" ] && all_pkgs+=("$pkg")
    done < <(scan_file_for_deps "$file")
  done <<< "$unique_files"

  if [ ${#all_pkgs[@]} -eq 0 ]; then
    debug "No package dependencies detected"
    return 0
  fi

  # Deduplicate and output one per line
  printf '%s\n' "${all_pkgs[@]}" | sort -u
}

# Docs / AGENTS.md scaffolding delegated to util.sh (scaffold_docs + scaffold_agents)
# so ralph init and ralph sync share one code path.

# Main

# Handle deps mode separately - no sync, no diff
if [ "$DEPS_MODE" = "true" ]; then
  show_deps
  exit 0
fi

# Handle diff mode separately - no sync, just show changes
if [ "$DIFF_MODE" = "true" ]; then
  # For diff mode, we still need RALPH_TEMPLATE_DIR (can't fetch from GitHub for diff)
  if [ -z "$PACKAGED_DIR" ]; then
    error "RALPH_TEMPLATE_DIR not set or directory doesn't exist.

To fix this, do one of the following:
  - Run 'ralph sync' to fetch templates from GitHub
  - Set RALPH_TEMPLATE_DIR to point to an existing template directory

Current value: ${RALPH_TEMPLATE_DIR:-<not set>}"
  fi

  show_template_diff "$DIFF_TEMPLATE_NAME"
  exit 0
fi

# Sync mode
echo "Ralph Template Sync"
echo "==================="
echo ""

if [ "$DRY_RUN" = "true" ]; then
  echo "DRY RUN MODE - No changes will be made"
  echo ""
fi

echo "Packaged templates: $PACKAGED_DIR"
echo "Local templates:    $TEMPLATES_DIR"
echo ""

# Step 1: Backup existing customizations
backup_existing "$TEMPLATES_DIR" "$BACKUP_DIR" "$PACKAGED_DIR"

# Step 2: Copy fresh templates
copy_fresh_templates "$PACKAGED_DIR" "$TEMPLATES_DIR"

# Step 3: Sync config.nix to ralph root (not templates directory)
packaged_config="$PACKAGED_DIR/config.nix"
local_config="$RALPH_DIR/config.nix"
if [ -f "$packaged_config" ]; then
  echo ""
  echo "Syncing config.nix to: $RALPH_DIR"

  # Backup if local differs from packaged
  if [ -f "$local_config" ]; then
    if ! diff -q "$packaged_config" "$local_config" >/dev/null 2>&1; then
      ensure_dir "$BACKUP_DIR"
      action "Backing up: config.nix (has local changes)"
      if [ "$DRY_RUN" = "false" ]; then
        cp "$local_config" "$BACKUP_DIR/config.nix"
      fi
    fi
  fi

  copy_file "$packaged_config" "$local_config" "config.nix"
fi

# Step 4: Scaffold missing docs files + AGENTS.md (shared with ralph init)
scaffold_docs
scaffold_agents

# Step 5: Retrofit beads config for existing projects (shared with ralph init)
if [ "$DRY_RUN" = "false" ]; then
  ensure_beads_config
fi

echo ""
if [ "$DRY_RUN" = "true" ]; then
  echo "Dry run complete. Run without --dry-run to apply changes."
else
  echo "Sync complete."
  echo ""
  echo "Next steps:"
  echo "  ralph sync --diff  - Review changes vs packaged templates"
  echo "  ralph check        - Validate template syntax"
fi
