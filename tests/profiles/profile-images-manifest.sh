#!/usr/bin/env bash
# Verify shape and presence of the profile-image manifest produced by
# `wrapix.lib.${system}.mkProfileImages` (specs/profiles.md).
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
# Usage: tests/profiles/profile-images-manifest.sh [function_name]
# Each function exits 0 on PASS, non-zero on FAIL, 77 to skip.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

test_manifest_shape() {
    local flake_url="git+file://$REPO_ROOT"

    local system
    if ! system=$(nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem'); then
        echo "nix eval builtins.currentSystem failed" >&2
        return 1
    fi

    # Pull the manifest entry and the matching image's outPath in one eval
    # so they're computed against the same flake state. Using passthru.manifest
    # avoids realizing the writeText derivation (which would transitively
    # build the rust profile image).
    local result
    if ! result=$(nix eval --json --impure --no-warn-dirty --expr "
      let
        flake = builtins.getFlake \"$flake_url\";
        lib = flake.legacyPackages.${system}.lib;
        rustImage = (lib.mkSandbox { profile = lib.profiles.rust; }).image;
        manifestDrv = lib.mkProfileImages { rust = rustImage; };
      in {
        rustEntry = manifestDrv.passthru.manifest.rust;
        rustImageOutPath = rustImage.outPath;
      }
    "); then
        echo "nix eval of mkProfileImages failed" >&2
        return 1
    fi

    local source ref expected_source
    source=$(echo "$result" | jq -r '.rustEntry.source')
    ref=$(echo "$result" | jq -r '.rustEntry.ref')
    expected_source=$(echo "$result" | jq -r '.rustImageOutPath')

    if [ -z "$source" ] || [ "$source" = "null" ]; then
        echo "manifest .rust.source is missing or empty" >&2
        return 1
    fi
    if [ -z "$ref" ] || [ "$ref" = "null" ]; then
        echo "manifest .rust.ref is missing or empty" >&2
        return 1
    fi

    if [ "$source" != "$expected_source" ]; then
        echo "manifest .rust.source ($source) != (mkSandbox { profile = profiles.rust; }).image outPath ($expected_source)" >&2
        return 1
    fi

    # ref is `[localhost/]wrapix-<name>:<hash>`; the prefix is platform-dependent
    # (linux uses localhost/, darwin omits it). Accept both.
    if ! [[ "$ref" =~ ^(localhost/)?wrapix-rust:[a-f0-9]+$ ]]; then
        echo "manifest .rust.ref ($ref) does not match expected pattern '[localhost/]wrapix-rust:<hex>'" >&2
        return 1
    fi
}

test_flake_outputs_present() {
    local flake_url="git+file://$REPO_ROOT"

    # `sandbox-base` is exposed as bare `sandbox`; `rust`/`python` keep the
    # `sandbox-<name>` suffix.
    local outputs=(
        "image-base"
        "image-rust"
        "image-python"
        "sandbox"
        "sandbox-rust"
        "sandbox-python"
        "profile-images"
        "wrapix"
    )

    local out
    for out in "${outputs[@]}"; do
        if ! nix eval --raw --no-warn-dirty "$flake_url#$out.outPath" >/dev/null 2>&1; then
            echo "packages.$out failed to evaluate" >&2
            return 1
        fi
    done
}

fn="${1:-test_manifest_shape}"
if ! declare -f "$fn" >/dev/null 2>&1; then
    echo "Unknown function: $fn" >&2
    exit 1
fi
"$fn"
