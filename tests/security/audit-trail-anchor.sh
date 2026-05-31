#!/usr/bin/env bash
# Verifier for criterion 207 of specs/security.md:
#
#   After a sandbox session, a session-metadata index file exists under
#   /workspace/.wrapix/log/; its timestamp_start, timestamp_end,
#   exit_code, mode, and claude_session_dir fields are populated; and
#   claude_session_dir resolves to an existing directory.
#
# The substantive behaviour is exercised by the NixOS VM check in
# tests/sandbox/integration.nix (attr `audit-trail-anchor`, derivation
# name `wrapix-audit-trail-anchor`). That check loads the base sandbox
# image into a rootless podman inside a NixOS VM, runs the entrypoint
# with a no-op command override so its EXIT trap fires without booting
# an agent runtime, and asserts the host-side
# `/tmp/workspace/.wrapix/log/*.json` exists with the contract fields
# populated and `claude_session_dir` resolving to an existing host
# directory. It is wired into the flake checks via tests/default.nix
# only when /dev/kvm is available; without KVM (Darwin, or Linux in a
# container that cannot nest VMs) the attr is dropped at the
# tests/default.nix layer.
#
# This shell verifier drives the existing check rather than
# re-implementing audit-trail emission — we do not add a new backing
# test. We only translate environment availability into a verdict the
# orchestrator can consume:
#
#   Linux with /dev/kvm + nix    -> nix build .#checks.<system>.audit-trail-anchor
#   Darwin                       -> exit 77 (entrypoint exercised by tests/darwin/*)
#   Linux without /dev/kvm       -> exit 77 (matches tests/default.nix's KVM gate)
#   nix unavailable on PATH      -> exit 77 (cannot drive the flake check at all)
#
# The exit-77 SKIP convention mirrors tests/sandbox/container-starts.sh,
# tests/sandbox/filesystem-isolation.sh, and tests/sandbox/uid-mapping.sh:
# a clear stderr message plus exit 77 lets the runner distinguish "test
# cannot run here" from "test failed".

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
    skip "audit-trail-anchor runs in a NixOS VM (Linux-only); macOS entrypoint is covered by tests/darwin/*"
  fi

  if [[ "$uname_s" != "Linux" ]]; then
    skip "unsupported platform: $uname_s"
  fi

  if [[ ! -e /dev/kvm ]]; then
    skip "/dev/kvm not present — flake check audit-trail-anchor is gated on KVM"
  fi

  if ! command -v nix >/dev/null 2>&1; then
    skip "nix not on PATH — cannot drive .#checks.<system>.audit-trail-anchor"
  fi

  local system
  system=$(nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem')

  local attr=".#checks.${system}.audit-trail-anchor"
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

(cd "$REPO_ROOT" && main "$@")
