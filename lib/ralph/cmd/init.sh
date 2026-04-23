#!/usr/bin/env bash
# ralph init — cold-start bootstrap for a new wrapix project.
#
# Runs on the HOST: creates flake.nix and .envrc which must exist before any
# wrapix container can spin up. Idempotent; every artifact is skip-if-exists.
# No git operations, no container, no --spec, no path argument — always cwd.

set -euo pipefail

# shellcheck source=util.sh
# shellcheck disable=SC1091
source "$(dirname "$0")/util.sh"

usage() {
  cat <<'EOF'
Usage: ralph init

Bootstraps the current directory as a wrapix project. Safe to re-run;
existing artifacts are left untouched.

Artifacts (in order, all skip-if-exists):
  flake.nix, .envrc, .gitignore (append-missing), .pre-commit-config.yaml,
  .beads/ (bd init), docs/*.md, AGENTS.md, CLAUDE.md (-> AGENTS.md),
  .wrapix/ralph/template/

No git operations are performed.
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    *) error "Unknown argument: $1
Run 'ralph init --help' for usage." ;;
  esac
done

CREATED=()
SKIPPED=()

record_created() {
  local name="$1" detail="${2:-}"
  if [ -n "$detail" ]; then
    CREATED+=("$name  ($detail)")
  else
    CREATED+=("$name")
  fi
}

record_skipped() {
  local name="$1" detail="${2:-}"
  if [ -n "$detail" ]; then
    SKIPPED+=("$name  ($detail)")
  else
    SKIPPED+=("$name")
  fi
}

run_bootstrap() {
  local fn="$1" artifact="$2"
  local rc=0
  "$fn" || rc=$?
  case "$rc" in
    0) record_created "$artifact" "$BOOTSTRAP_DETAIL" ;;
    1) record_skipped "$artifact" "$BOOTSTRAP_DETAIL" ;;
    *) error "$artifact: $BOOTSTRAP_DETAIL" ;;
  esac
}

# scaffold_* helpers return: 0=created, 1=already exists, 2+=error.
# Branch explicitly so a real error aborts while a clean skip is tolerated.
run_scaffold() {
  local fn="$1"
  local rc=0
  "$fn" >/dev/null || rc=$?
  case "$rc" in
    0|1) return 0 ;;
    *) error "$fn failed (rc=$rc)" ;;
  esac
}

record_scaffolded() {
  local path="$1" pre_existed="$2"
  if [ "$pre_existed" = "1" ]; then
    record_skipped "$path" "already exists"
  elif [ -e "$path" ] || [ -d "$path" ]; then
    record_created "$path"
  else
    error "$path: scaffold did not produce expected artifact"
  fi
}

run_bootstrap bootstrap_flake     "flake.nix"
run_bootstrap bootstrap_envrc     ".envrc"
run_bootstrap bootstrap_gitignore ".gitignore"
run_bootstrap bootstrap_precommit ".pre-commit-config.yaml"
run_bootstrap bootstrap_beads     ".beads/"

DOCS_FILES=(docs/README.md docs/architecture.md docs/style-guidelines.md)
docs_pre=()
for f in "${DOCS_FILES[@]}"; do
  if [ -e "$f" ]; then docs_pre+=("1"); else docs_pre+=("0"); fi
done
run_scaffold scaffold_docs
for i in "${!DOCS_FILES[@]}"; do
  record_scaffolded "${DOCS_FILES[$i]}" "${docs_pre[$i]}"
done

agents_pre=0
[ -e AGENTS.md ] && agents_pre=1
run_scaffold scaffold_agents
record_scaffolded "AGENTS.md" "$agents_pre"

run_bootstrap bootstrap_claude_symlink "CLAUDE.md"

TEMPLATES_DIR="${RALPH_DIR:-.wrapix/ralph}/template"
templates_pre=0
[ -d "$TEMPLATES_DIR" ] && templates_pre=1
run_scaffold scaffold_templates
record_scaffolded "$TEMPLATES_DIR/" "$templates_pre"

echo ""
echo "✓ Bootstrapped wrapix project in ."
echo ""
if [ "${#CREATED[@]}" -gt 0 ]; then
  echo "Created:"
  for line in "${CREATED[@]}"; do
    echo "  $line"
  done
fi
if [ "${#SKIPPED[@]}" -gt 0 ]; then
  echo "Skipped:"
  for line in "${SKIPPED[@]}"; do
    echo "  $line"
  done
fi
echo ""
echo "Next steps:"
echo "  1. direnv allow            # devShell auto-enters"
echo "  2. ralph plan -n <label>   # start your first feature"
echo ""
echo "Docs: specs/ralph-harness.md"
