#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

skip() {
  local reason="$1"

  echo "SKIP: $reason" >&2
  exit 77
}

require_tool() {
  local tool="$1"

  command -v "$tool" >/dev/null 2>&1 || skip "$tool not on PATH"
}

store_bin_dir() {
  local tool="$1"
  local resolved

  resolved="$(readlink -f "$(command -v "$tool")")"
  dirname "$resolved"
}

require_tool bash
require_tool cat
require_tool chmod
require_tool dirname
require_tool env
require_tool git
require_tool mkdir
require_tool mktemp
require_tool prek
require_tool readlink
require_tool rm

BASH_BIN="$(readlink -f "$(command -v bash)")"
PREK_BIN="$(readlink -f "$(command -v prek)")"
BASH_DIR="$(store_bin_dir bash)"
COREUTILS_DIR="$(store_bin_dir env)"
GIT_DIR="$(store_bin_dir git)"
PREK_DIR="$(store_bin_dir prek)"

TEST_TMP="$(mktemp -d -t wrix-prek-nix-required.XXXXXX)"
trap 'rm -rf "$TEST_TMP"' EXIT
mkdir -p "$TEST_TMP/home"

ARGS_FILE="$TEST_TMP/pre-push-checks.args"
STDOUT_FILE="$TEST_TMP/stdout"
STDERR_FILE="$TEST_TMP/stderr"

cat > "$TEST_TMP/pre-push-checks" <<EOF
#!$BASH_BIN
set -euo pipefail
printf '%s\n' "\$*" > "$ARGS_FILE"
exit 0
EOF
chmod +x "$TEST_TMP/pre-push-checks"

cat > "$TEST_TMP/skip-if-missing" <<EOF
#!$BASH_BIN
set -euo pipefail
tool="\$1"
shift
if [[ "\${1:-}" != "--" ]]; then
  exit 2
fi
shift
if ! command -v "\$tool" >/dev/null 2>&1; then
  exit 0
fi
exec "\$@"
EOF
chmod +x "$TEST_TMP/skip-if-missing"

PATH_VALUE="$TEST_TMP:$PREK_DIR:$GIT_DIR:$COREUTILS_DIR:$BASH_DIR"
if env -i PATH="$PATH_VALUE" bash -c 'command -v nix >/dev/null 2>&1'; then
  echo "FAIL: controlled test PATH unexpectedly contains nix" >&2
  exit 1
fi

if ! env -i HOME="$TEST_TMP/home" PATH="$PATH_VALUE" PREK_COLOR=never \
  "$PREK_BIN" run nix-flake-check --stage pre-push --all-files \
  --config "$REPO_ROOT/.pre-commit-config.yaml" --no-progress \
  >"$STDOUT_FILE" 2>"$STDERR_FILE"; then
  echo "FAIL: prek did not run nix-flake-check successfully" >&2
  cat "$STDOUT_FILE" >&2
  cat "$STDERR_FILE" >&2
  exit 1
fi

if [[ ! -f "$ARGS_FILE" ]]; then
  echo "FAIL: nix-flake-check did not invoke pre-push-checks" >&2
  cat "$STDOUT_FILE" >&2
  cat "$STDERR_FILE" >&2
  exit 1
fi

if [[ "$(<"$ARGS_FILE")" != "nix flake check" ]]; then
  echo "FAIL: pre-push-checks received unexpected args: $(<"$ARGS_FILE")" >&2
  exit 1
fi

echo "PASS: nix-flake-check invokes pre-push-checks nix flake check without skip-if-missing"
