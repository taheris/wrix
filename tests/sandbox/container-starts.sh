#!/usr/bin/env bash
# Verifier for criterion 104 of specs/sandbox.md:
#
#   A built sandbox starts a container and exits cleanly on both Linux and
#   macOS.
#
# The substantive behaviour is exercised by the NixOS VM check in
# tests/sandbox/integration.nix (attr `container-start`, derivation name
# `wrapix-container-start`). That check loads the base sandbox image into a
# rootless podman inside a NixOS VM, runs the image with `--network=pasta
# --userns=keep-id`, and asserts the container both prints `container-started`
# and that pasta-mode networking is configured. It is wired into the flake
# checks via tests/default.nix only when /dev/kvm is available; without KVM
# (Darwin, or Linux in a container that cannot nest VMs) the attr is dropped
# at the tests/default.nix layer.
#
# This shell verifier drives the existing check rather than re-implementing
# container startup — we do not add a new backing test. We only translate
# environment availability into a verdict the orchestrator can consume:
#
#   Linux with /dev/kvm + nix    → run `nix build .#checks.<system>.container-start`
#   Darwin                       → exit 77 (macOS branch covered by tests/darwin/*)
#   Linux without /dev/kvm       → exit 77 (matches tests/default.nix's KVM gate)
#   nix unavailable on PATH      → exit 77 (cannot drive the flake check at all)
#
# The exit-77 SKIP convention mirrors tests/sandbox/smoke.nix's
# `skipImageTest` path: a clear stderr message plus exit 77 lets the runner
# distinguish "test cannot run here" from "test failed".

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

skip() {
  echo "SKIP: $1" >&2
  exit 77
}

main() {
  local uname_s
  uname_s=$(uname -s)

  if [[ "$uname_s" = "Darwin" ]]; then
    skip "container-start runs in a NixOS VM (Linux-only); macOS is covered by tests/darwin/*"
  fi

  if [[ "$uname_s" != "Linux" ]]; then
    skip "unsupported platform: $uname_s"
  fi

  if [[ ! -e /dev/kvm ]]; then
    skip "/dev/kvm not present — flake check container-start is gated on KVM"
  fi

  if ! command -v nix >/dev/null 2>&1; then
    skip "nix not on PATH — cannot drive .#checks.<system>.container-start"
  fi

  local system
  system=$(nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem')

  local attr=".#checks.${system}.container-start"
  echo "=== nix build $attr ===" >&2
  # --impure: tests/default.nix gates sandboxIntegrationTests on
  # `builtins.pathExists "/dev/kvm"`, which returns false in pure eval
  # even when /dev/kvm exists. Without --impure the attr is missing
  # despite the shell-level KVM guard above.
  if ! nix build --impure --no-link --print-build-logs --no-warn-dirty "$attr"; then
    echo "FAIL: $attr did not build" >&2
    return 1
  fi
  echo "PASS: $attr built successfully" >&2
}

main "$@"
