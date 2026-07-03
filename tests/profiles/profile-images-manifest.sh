#!/usr/bin/env bash
# Verify shape and presence of the profile-image manifest produced by
# `wrix.lib.${system}.mkProfileImages` (specs/profiles.md).
#
# Four contracts:
#
# 1. test_manifest_shape — `mkProfileImages { rust = ...; }` produces JSON
#    whose profile entry is keyed by the image's agent and whose variant carries
#    `ref`, `source`, `source_kind`, and `profile_config` fields, with `source`
#    and `source_kind` matching the corresponding `mkSandbox` image metadata.
#
# 2. test_flake_outputs_present — `packages.image-<name>[-<agent>]`,
#    `packages.sandbox-<name>[-<agent>][-mcp]`, `packages.profile-images`,
#    and `packages.profile-images-pi` evaluate for every built-in profile
#    (base, rust, python), and `packages.default` resolves to `sandbox-rust-pi`.
#
# 3. test_runtime_manifest_retains_store_context — the Pi runtime manifest
#    exported to Loom retains Nix context on `source` and `profile_config`.
#
# 4. test_eval_manifest_access_is_lightweight — `passthru.manifest` remains
#    available through evaluation-only inspection without building the runtime
#    manifest derivation exported to Loom.
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

    # Pull the manifest entry and the matching image metadata in one eval
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
        rustImageSource = rustImage.source;
        rustImageSourceKind = rustImage.source_kind;
        rustPiImageSource = rustPiImage.source;
        rustPiImageSourceKind = rustPiImage.source_kind;
      }
    "); then
        echo "nix eval of mkProfileImages failed" >&2
        return 1
    fi

    local source ref source_kind profile_config expected_source expected_source_kind pi_source pi_ref pi_source_kind pi_expected_source pi_expected_source_kind
    source=$(echo "$result" | jq -r '.rustEntry.source')
    ref=$(echo "$result" | jq -r '.rustEntry.ref')
    source_kind=$(echo "$result" | jq -r '.rustEntry.source_kind')
    profile_config=$(echo "$result" | jq -r '.rustEntry.profile_config')
    expected_source=$(echo "$result" | jq -r '.rustImageSource')
    expected_source_kind=$(echo "$result" | jq -r '.rustImageSourceKind')
    pi_source=$(echo "$result" | jq -r '.rustPiEntry.source')
    pi_ref=$(echo "$result" | jq -r '.rustPiEntry.ref')
    pi_source_kind=$(echo "$result" | jq -r '.rustPiEntry.source_kind')
    pi_expected_source=$(echo "$result" | jq -r '.rustPiImageSource')
    pi_expected_source_kind=$(echo "$result" | jq -r '.rustPiImageSourceKind')

    if [[ -z "$source" || "$source" == "null" ]]; then
        echo "manifest .rust.direct.source is missing or empty" >&2
        return 1
    fi
    if [[ -z "$ref" || "$ref" == "null" ]]; then
        echo "manifest .rust.direct.ref is missing or empty" >&2
        return 1
    fi
    if [[ -z "$source_kind" || "$source_kind" == "null" ]]; then
        echo "manifest .rust.direct.source_kind is missing or empty" >&2
        return 1
    fi
    if [[ -z "$profile_config" || "$profile_config" == "null" ]]; then
        echo "manifest .rust.direct.profile_config is missing or empty" >&2
        return 1
    fi

    if [[ "$source" != "$expected_source" ]]; then
        echo "manifest .rust.direct.source ($source) != mkSandbox image.source ($expected_source)" >&2
        return 1
    fi
    if [[ "$source_kind" != "$expected_source_kind" ]]; then
        echo "manifest .rust.direct.source_kind ($source_kind) != mkSandbox image.source_kind ($expected_source_kind)" >&2
        return 1
    fi
    if [[ "$pi_source" != "$pi_expected_source" ]]; then
        echo "manifest .rust.pi.source ($pi_source) != mkSandbox pi image.source ($pi_expected_source)" >&2
        return 1
    fi
    if [[ "$pi_source_kind" != "$pi_expected_source_kind" ]]; then
        echo "manifest .rust.pi.source_kind ($pi_source_kind) != mkSandbox pi image.source_kind ($pi_expected_source_kind)" >&2
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

    local outputs=(
        "profile-images"
        "profile-images-pi"
        "wrix"
    )

    local profile_suffix
    for profile_suffix in "-base" "-rust" "-python"; do
        outputs+=("image${profile_suffix}" "image${profile_suffix}-claude" "image${profile_suffix}-pi")
    done

    local sandbox_profile_suffix agent_suffix mcp_suffix
    for sandbox_profile_suffix in "" "-rust" "-python"; do
        for agent_suffix in "" "-claude" "-pi"; do
            for mcp_suffix in "" "-mcp"; do
                outputs+=("sandbox${sandbox_profile_suffix}${agent_suffix}${mcp_suffix}")
            done
        done
    done

    local out
    for out in "${outputs[@]}"; do
        if ! nix eval --raw --no-warn-dirty "$flake_url#$out.outPath" >/dev/null 2>&1; then
            echo "packages.$out failed to evaluate" >&2
            return 1
        fi
    done

    local default_name default_main
    if ! default_name=$(nix eval --raw --no-warn-dirty "$flake_url#default.name"); then
        echo "packages.default.name failed to evaluate" >&2
        return 1
    fi
    if [[ "$default_name" != "wrix-rust-pi" ]]; then
        echo "packages.default resolved to $default_name, expected wrix-rust-pi" >&2
        return 1
    fi
    if ! default_main=$(nix eval --raw --no-warn-dirty "$flake_url#default.meta.mainProgram"); then
        echo "packages.default.meta.mainProgram failed to evaluate" >&2
        return 1
    fi
    if [[ "$default_main" != "wrix-run" ]]; then
        echo "packages.default mainProgram is $default_main, expected wrix-run" >&2
        return 1
    fi
}

test_runtime_manifest_retains_store_context() {
    local flake_url="git+file://$REPO_ROOT"

    local system
    if ! system=$(nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem'); then
        echo "nix eval builtins.currentSystem failed" >&2
        return 1
    fi

    local result
    if ! result=$(nix eval --json --impure --no-warn-dirty --expr "
      let
        flake = builtins.getFlake \"$flake_url\";
        manifest = flake.packages.${system}.profile-images-pi.passthru.manifest;
        entryContext = entry: {
          source = builtins.hasContext entry.source;
          profile_config = builtins.hasContext entry.profile_config;
        };
      in {
        runtimeJson = builtins.hasContext (builtins.toJSON manifest);
        base = entryContext manifest.base.pi;
        rust = entryContext manifest.rust.pi;
        python = entryContext manifest.python.pi;
      }
    "); then
        echo "nix eval of profile-images-pi context failed" >&2
        return 1
    fi

    if ! jq -e '
      .runtimeJson == true and
      all([.base, .rust, .python][]; .source == true and .profile_config == true)
    ' <<<"$result" >/dev/null; then
        echo "packages.profile-images-pi runtime manifest lost Nix store context: $result" >&2
        return 1
    fi
}

test_eval_manifest_access_is_lightweight() {
    local flake_url="git+file://$REPO_ROOT"

    local system
    if ! system=$(nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem'); then
        echo "nix eval builtins.currentSystem failed" >&2
        return 1
    fi

    local result
    if ! result=$(nix eval --json --impure --no-warn-dirty --expr "
      let
        flake = builtins.getFlake \"$flake_url\";
        manifest = flake.packages.${system}.profile-images-pi.passthru.manifest;
      in {
        baseSource = manifest.base.pi.source;
        rustSource = manifest.rust.pi.source;
        pythonSource = manifest.python.pi.source;
        rustProfileConfig = manifest.rust.pi.profile_config;
      }
    "); then
        echo "nix eval of profile-images-pi passthru manifest failed" >&2
        return 1
    fi

    if ! jq -e '
      (.baseSource | type == "string") and
      (.rustSource | type == "string") and
      (.pythonSource | type == "string") and
      (.rustProfileConfig | type == "string")
    ' <<<"$result" >/dev/null; then
        echo "packages.profile-images-pi passthru manifest did not expose expected JSON strings: $result" >&2
        return 1
    fi
}

fn="${1:-test_manifest_shape}"
if ! declare -f "$fn" >/dev/null 2>&1; then
    echo "Unknown function: $fn" >&2
    exit 1
fi
"$fn"
