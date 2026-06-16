#!/usr/bin/env bash
# Verify shape and presence of the profile-image manifest produced by
# `wrix.lib.${system}.mkProfileImages` (specs/profiles.md).
#
# Two contracts:
#
# 1. test_manifest_shape — `mkProfileImages { rust = ...; }` produces JSON
#    whose profile entry is keyed by the image's agent and whose variant carries
#    `ref`, `source`, and `profile_config` fields, with `source` resolving to
#    the same Nix store path as the matching `mkSandbox` image.
#
# 2. test_flake_outputs_present — `packages.image-<name>`,
#    `packages.sandbox-<name>`, `packages.profile-images`, and
#    `packages.profile-images-pi` evaluate for every built-in profile (base,
#    rust, python).
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
        rustPiImage = (lib.mkSandbox { profile = lib.profiles.rust; agent = \"pi\"; }).image;
        manifestDrv = lib.mkProfileImages { rust = rustImage; };
        piManifestDrv = lib.mkProfileImages { rust = rustPiImage; };
      in {
        rustEntry = manifestDrv.passthru.manifest.rust.direct;
        rustPiEntry = piManifestDrv.passthru.manifest.rust.pi;
        rustImageOutPath = rustImage.outPath;
        rustPiImageOutPath = rustPiImage.outPath;
      }
    "); then
        echo "nix eval of mkProfileImages failed" >&2
        return 1
    fi

    local source ref profile_config expected_source pi_source pi_ref pi_expected_source
    source=$(echo "$result" | jq -r '.rustEntry.source')
    ref=$(echo "$result" | jq -r '.rustEntry.ref')
    profile_config=$(echo "$result" | jq -r '.rustEntry.profile_config')
    expected_source=$(echo "$result" | jq -r '.rustImageOutPath')
    pi_source=$(echo "$result" | jq -r '.rustPiEntry.source')
    pi_ref=$(echo "$result" | jq -r '.rustPiEntry.ref')
    pi_expected_source=$(echo "$result" | jq -r '.rustPiImageOutPath')

    if [[ -z "$source" || "$source" == "null" ]]; then
        echo "manifest .rust.direct.source is missing or empty" >&2
        return 1
    fi
    if [[ -z "$ref" || "$ref" == "null" ]]; then
        echo "manifest .rust.direct.ref is missing or empty" >&2
        return 1
    fi
    if [[ -z "$profile_config" || "$profile_config" == "null" ]]; then
        echo "manifest .rust.direct.profile_config is missing or empty" >&2
        return 1
    fi

    if [[ "$source" != "$expected_source" ]]; then
        echo "manifest .rust.direct.source ($source) != (mkSandbox { profile = profiles.rust; }).image outPath ($expected_source)" >&2
        return 1
    fi
    if [[ "$pi_source" != "$pi_expected_source" ]]; then
        echo "manifest .rust.pi.source ($pi_source) != (mkSandbox { profile = profiles.rust; agent = \"pi\"; }).image outPath ($pi_expected_source)" >&2
        return 1
    fi

    # ref is `[localhost/]wrix-<name>:<hash>`; the prefix is platform-dependent
    # (linux uses localhost/, darwin omits it). Accept both.
    if ! [[ "$ref" =~ ^(localhost/)?wrix-rust:[a-f0-9]+$ ]]; then
        echo "manifest .rust.direct.ref ($ref) does not match expected pattern '[localhost/]wrix-rust:<hex>'" >&2
        return 1
    fi
    if ! [[ "$pi_ref" =~ ^(localhost/)?wrix-rust-pi:[a-f0-9]+$ ]]; then
        echo "manifest .rust.pi.ref ($pi_ref) does not match expected pattern '[localhost/]wrix-rust-pi:<hex>'" >&2
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
        "profile-images-pi"
        "wrix"
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
