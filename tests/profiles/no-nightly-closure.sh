#!/usr/bin/env bash
# Reject nightly toolchains after updating a disposable copy of the live flake.
#
# Usage: tests/profiles/no-nightly-closure.sh [test_no_nightly_closure]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
FIXTURE_SHA="sha256-+xUr6Al1wZSqX8aEsq7DpuFOK4Ve/i+qTl+K5uWQ9GM="
TMPDIRS=()

cleanup() {
  local directory
  for directory in "${TMPDIRS[@]+"${TMPDIRS[@]}"}"; do
    if [[ -d "$directory" ]]; then
      rm -rf "$directory"
    fi
  done
}
trap cleanup EXIT

copy_tracked_flake() {
  local destination
  local relative_path
  destination=$(mktemp -d -t wrix-fresh-flake.XXXXXX)
  destination=$(cd "$destination" && pwd -P)
  while IFS= read -r -d '' relative_path; do
    mkdir -p "$destination/$(dirname "$relative_path")"
    cp -a "$REPO_ROOT/$relative_path" "$destination/$relative_path"
  done < <(git -C "$REPO_ROOT" ls-files -z)
  printf '%s\n' "$destination"
}

count_nightly_in_closures() {
  local derivations_json="$1" scan_root="$2"
  local derivations=() toolchains=()
  mapfile -t derivations < <(jq -r '.surfaces[]' <<<"$derivations_json")
  mapfile -t toolchains < <(jq -r '.toolchains[]' <<<"$derivations_json")
  if [[ "${#derivations[@]}" -eq 0 || "${#toolchains[@]}" -eq 0 ]]; then
    echo "no Rust profile or toolchain derivations were returned" >&2
    return 1
  fi
  nix-store -q --requisites "${derivations[@]}" >"$scan_root/closure"
  nix-store -q --requisites "${toolchains[@]}" >"$scan_root/toolchains"
  jq -n --argjson roots "$(jq '.surfaces' <<<"$derivations_json")" \
    --rawfile closure "$scan_root/closure" --rawfile toolchains "$scan_root/toolchains" \
    '{roots: $roots, closure: ($closure | split("\n") | map(select(length > 0))), toolchainClosure: ($toolchains | split("\n") | map(select(length > 0)))}' >"$scan_root/input.json"
  WRIX_NIGHTLY_SCAN="$scan_root/input.json" WRIX_NIGHTLY_CLASSIFIER="$SCRIPT_DIR/nightly-closure.nix" \
    nix eval --json --impure --expr '
      (import (builtins.toPath (builtins.getEnv "WRIX_NIGHTLY_CLASSIFIER")))
        (builtins.fromJSON (builtins.readFile (builtins.getEnv "WRIX_NIGHTLY_SCAN")))
    ' >"$scan_root/violations.json"
  jq -r '.[]' "$scan_root/violations.json" >&2
  jq 'length' "$scan_root/violations.json"
}

profile_derivations() {
  local flake_root="$1"
  local system="$2"
  nix eval --json --impure --no-warn-dirty --expr "
    let
      flake = builtins.getFlake \"path:$flake_root\";
      lib = flake.legacyPackages.${system}.lib;
      surfaces = profile: {
        surfaces = map (package: package.drvPath) (
          profile.packages
          ++ (profile.hostPackages or [ ])
          ++ [ profile.toolchain ]
        );
        toolchains = [ profile.toolchain.drvPath ]
          ++ builtins.attrNames (builtins.getContext profile.env.RUSTC);
      };
      pinned = lib.rustProfile {
        toolchain = $flake_root/tests/fixtures/rust-toolchain.toml;
        sha256 = \"$FIXTURE_SHA\";
      };
    in {
      default = surfaces lib.profiles.rust;
      pinned = surfaces pinned;
    }
  "
}

test_no_nightly_closure() {
  local fresh_flake scan_root
  local system
  local derivations
  local profile
  local count
  nix eval --json --file "$SCRIPT_DIR/nightly-closure-test.nix" >/dev/null
  scan_root=$(mktemp -d -t wrix-nightly-scan.XXXXXX)
  TMPDIRS+=("$scan_root")
  fresh_flake=$(copy_tracked_flake)
  TMPDIRS+=("$fresh_flake")

  nix flake update --flake "path:$fresh_flake"
  system=$(nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem')
  derivations=$(profile_derivations "$fresh_flake" "$system")

  for profile in default pinned; do
    count=$(count_nightly_in_closures "$(jq -c ".$profile" <<<"$derivations")" "$scan_root")
    if [[ "$count" -ne 0 ]]; then
      echo "$profile Rust profile package closures contain $count nightly derivation(s)" >&2
      return 1
    fi
  done
}

fn="${1:-test_no_nightly_closure}"
if ! declare -f "$fn" >/dev/null 2>&1; then
  echo "Unknown function: $fn" >&2
  exit 1
fi
"$fn"
