#!/usr/bin/env bash
# Verifier for criterion 106 of specs/sandbox.md:
#
#   Files created inside /workspace carry the host UID/GID, not a
#   container-internal UID.
#
# The substantive behaviour is exercised by two existing backing tests:
#
#   Linux  — tests/sandbox/integration.nix attr `user-namespace`
#            (NixOS VM check; creates files inside the container under
#            `--userns=keep-id` and asserts host-side stat shows UID 1000).
#   Darwin — tests/darwin/uid.nix attr `darwin-uid-integration`
#            (drives Apple `container` CLI with HOST_UID propagation,
#            then runs tests/darwin/uid-test.sh inside the container).
#
# Both are wired into the flake's check set by tests/default.nix:
#   sandboxIntegrationTests = if isLinux && hasKvm then import ... else {};
#   darwinUidTests = import ./darwin/uid.nix { ... };
#
# This shell verifier drives the existing checks rather than re-implementing
# UID mapping — we do not add a new backing test. We only translate the
# environment availability into a verdict the orchestrator can consume:
#
#   Linux with /dev/kvm + nix → `nix build .#checks.<system>.user-namespace`
#   Darwin with nix           → `nix build .#checks.<system>.darwin-uid-integration`
#   Linux without /dev/kvm    → exit 77 (matches tests/default.nix's KVM gate)
#   nix unavailable on PATH   → exit 77 (cannot drive the flake check at all)
#   any other platform        → exit 77
#
# The exit-77 SKIP convention mirrors tests/sandbox/container-starts.sh: a
# clear stderr message plus exit 77 lets the runner distinguish "test cannot
# run here" from "test failed".

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

skip() {
  echo "SKIP: $1" >&2
  exit 77
}

drive_check() {
  local attr="$1"
  echo "=== nix build $attr ===" >&2
  # --impure: tests/default.nix gates sandboxIntegrationTests on
  # `builtins.pathExists "/dev/kvm"`, which returns false in pure eval
  # even when /dev/kvm exists. Without --impure the attr is missing
  # despite the shell-level KVM guard.
  if ! nix build --impure --no-link --print-build-logs --no-warn-dirty "$attr"; then
    echo "FAIL: $attr did not build" >&2
    return 1
  fi
  echo "PASS: $attr built successfully" >&2
}

main() {
  if ! command -v nix >/dev/null 2>&1; then
    skip "nix not on PATH — cannot drive .#checks.<system>.user-namespace or .darwin-uid-integration"
  fi

  local uname_s system
  uname_s=$(uname -s)
  system=$(nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem')

  case "$uname_s" in
    Linux)
      if [[ ! -e /dev/kvm ]]; then
        skip "/dev/kvm not present — flake check user-namespace is gated on KVM in tests/default.nix"
      fi
      drive_check ".#checks.${system}.user-namespace"
      ;;
    Darwin)
      drive_check ".#checks.${system}.darwin-uid-integration"
      ;;
    *)
      skip "unsupported platform: $uname_s"
      ;;
  esac
}

(cd "$REPO_ROOT" && main "$@")
