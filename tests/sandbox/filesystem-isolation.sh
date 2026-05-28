#!/usr/bin/env bash
# Verifier for criterion 108 of specs/sandbox.md:
#
#   Host filesystem outside /workspace and declared mounts is not visible
#   inside the container.
#
# The substantive behaviour is exercised by the NixOS VM check in
# tests/sandbox/integration.nix (attr `filesystem-isolation`, derivation
# name `wrapix-filesystem-isolation`). That check loads the base sandbox
# image into a rootless podman inside a NixOS VM, writes a sentinel file
# at /tmp/host-secret.txt on the host, and asserts the container cannot
# read it through its own /tmp (which is isolated from the host) while
# still being able to read /workspace bind-mounts. It is wired into the
# flake checks via tests/default.nix only when /dev/kvm is available;
# without KVM (Darwin, or Linux in a container that cannot nest VMs) the
# attr is dropped at the tests/default.nix layer.
#
# This shell verifier drives the existing check rather than re-implementing
# filesystem isolation — we do not add a new backing test. We only translate
# environment availability into a verdict the orchestrator can consume:
#
#   Linux with /dev/kvm + nix    → run `nix build .#checks.<system>.filesystem-isolation`
#   Darwin                       → exit 77 (macOS branch covered by tests/darwin/*)
#   Linux without /dev/kvm       → exit 77 (matches tests/default.nix's KVM gate)
#   nix unavailable on PATH      → exit 77 (cannot drive the flake check at all)
#
# The exit-77 SKIP convention mirrors tests/sandbox/container-starts.sh and
# tests/sandbox/uid-mapping.sh: a clear stderr message plus exit 77 lets the
# runner distinguish "test cannot run here" from "test failed".

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
    skip "filesystem-isolation runs in a NixOS VM (Linux-only); macOS is covered by tests/darwin/*"
  fi

  if [[ "$uname_s" != "Linux" ]]; then
    skip "unsupported platform: $uname_s"
  fi

  if [[ ! -e /dev/kvm ]]; then
    skip "/dev/kvm not present — flake check filesystem-isolation is gated on KVM"
  fi

  if ! command -v nix >/dev/null 2>&1; then
    skip "nix not on PATH — cannot drive .#checks.<system>.filesystem-isolation"
  fi

  local system
  system=$(nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem')

  local attr=".#checks.${system}.filesystem-isolation"
  echo "=== nix build $attr ===" >&2
  if ! nix build --no-link --print-build-logs --no-warn-dirty "$attr"; then
    echo "FAIL: $attr did not build" >&2
    return 1
  fi
  echo "PASS: $attr built successfully" >&2
}

(cd "$REPO_ROOT" && main "$@")
