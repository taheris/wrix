#!/usr/bin/env bash
# Verifier for criterion 152 of specs/pre-commit.md:
#
#   A pre-commit hook configured in `.pre-commit-config.yaml` fires when
#   `git commit` runs inside a profile container.
#
# The substantive behaviour is exercised by the NixOS VM check in
# tests/sandbox/integration.nix (attr `container-pre-commit`, derivation
# name `wrapix-container-pre-commit`). That check loads the base sandbox
# image into a rootless podman inside a NixOS VM, seeds a workspace with
# a git repo, a .pre-commit-config.yaml that names both a `skip-if-missing`
# probe and a sentinel hook, then runs `git commit` as the entrypoint's
# command override. The sentinel hook writes a marker file outside the
# worktree (.git/sentinel-fired-precommit); its presence after the commit
# proves the bundled hook chain fired via core.hooksPath. It is wired into
# the flake checks via tests/default.nix only when /dev/kvm is available;
# without KVM (Darwin, or Linux in a container that cannot nest VMs) the
# attr is dropped at the tests/default.nix layer.
#
# This shell verifier drives the existing check rather than re-implementing
# container hook firing — we do not add a new backing test. We only translate
# environment availability into a verdict the orchestrator can consume:
#
#   Linux with /dev/kvm + nix    → run `nix build .#checks.<system>.container-pre-commit`
#   Darwin                       → exit 77 (macOS container hook parity not yet covered)
#   Linux without /dev/kvm       → exit 77 (matches tests/default.nix's KVM gate)
#   nix unavailable on PATH      → exit 77 (cannot drive the flake check at all)
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

main() {
  local uname_s
  uname_s=$(uname -s)

  if [[ "$uname_s" = "Darwin" ]]; then
    skip "container-pre-commit runs in a NixOS VM (Linux-only); macOS container hook parity is not yet exercised"
  fi

  if [[ "$uname_s" != "Linux" ]]; then
    skip "unsupported platform: $uname_s"
  fi

  if [[ ! -e /dev/kvm ]]; then
    skip "/dev/kvm not present — flake check container-pre-commit is gated on KVM"
  fi

  if ! command -v nix >/dev/null; then
    skip "nix not on PATH — cannot drive .#checks.<system>.container-pre-commit"
  fi

  local system
  system=$(nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem')

  local attr=".#checks.${system}.container-pre-commit"
  echo "=== nix build $attr ===" >&2
  if ! nix build --no-link --print-build-logs --no-warn-dirty "$attr"; then
    echo "FAIL: $attr did not build" >&2
    return 1
  fi
  echo "PASS: $attr built successfully" >&2
}

(cd "$REPO_ROOT" && main "$@")
