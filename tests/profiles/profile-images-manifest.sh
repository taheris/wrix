#!/usr/bin/env bash
# Verify shape and presence of the profile-image manifest produced by
# `wrapix.lib.${system}.mkProfileImages` (specs/profiles.md, specs/loom-harness.md).
#
# Two contracts:
#
# 1. test_manifest_shape — `mkProfileImages { rust = ...; }` produces JSON
#    whose entry for each profile carries both `ref` and `source` fields,
#    with `source` resolving to the same Nix store path as the matching
#    `mkSandbox` image.
#
# 2. test_flake_outputs_present — `packages.image-<name>`,
#    `packages.sandbox-<name>`, and `packages.profile-images` evaluate for
#    every built-in profile (base, rust, python).
#
# Both are stubs: pending the `mkProfileImages` helper landing in
# lib/sandbox/profiles.nix and the matching flake outputs landing in
# modules/flake/packages.nix. Implementation lands in a follow-up task
# under wx-3hhwq; this file satisfies the spec annotation references so
# the spec commit can land.
#
# Usage: tests/profiles/profile-images-manifest.sh [function_name]
# Each function exits 0 on PASS, non-zero on FAIL, 77 to skip.

set -euo pipefail

test_manifest_shape() {
    echo "stub: mkProfileImages helper not yet implemented in lib/sandbox/profiles.nix" >&2
    return 77
}

test_flake_outputs_present() {
    echo "stub: packages.image-<name>/sandbox-<name>/profile-images outputs not yet wired in modules/flake/packages.nix" >&2
    return 77
}

fn="${1:-test_manifest_shape}"
if ! declare -f "$fn" >/dev/null 2>&1; then
    echo "Unknown function: $fn" >&2
    exit 1
fi
"$fn"
