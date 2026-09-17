#!/usr/bin/env bash
# Verify that every Rust profile package surface has no nightly derivation after
# updating a disposable copy of the live flake.
#
# Usage: tests/profiles/no-nightly-closure.sh [test_no_nightly_closure]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
FIXTURE_SHA="sha256-Hn2uaQzRLidAWpfmRwSRdImifGUCAb9HeAqTYFXWeQk="
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
  local derivations_json="$1"
  local derivations=()
  mapfile -t derivations < <(jq -r '.[]' <<<"$derivations_json")
  if [[ "${#derivations[@]}" -eq 0 ]]; then
    echo "no Rust profile derivations were returned" >&2
    return 1
  fi
  nix-store -q --requisites "${derivations[@]}" | awk 'tolower($0) ~ /nightly/ {n++} END {print n+0}'
}

profile_derivations() {
  local flake_root="$1"
  local system="$2"
  nix eval --json --impure --no-warn-dirty --expr "
    let
      flake = builtins.getFlake \"path:$flake_root\";
      lib = flake.legacyPackages.${system}.lib;
      surfaces = profile:
        map (package: package.drvPath) (
          profile.packages
          ++ (profile.hostPackages or [ ])
          ++ [ profile.toolchain ]
        );
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
  local fresh_flake
  local system
  local derivations
  local profile
  local count
  fresh_flake=$(copy_tracked_flake)
  TMPDIRS+=("$fresh_flake")

  nix flake update --flake "path:$fresh_flake"
  system=$(nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem')
  derivations=$(profile_derivations "$fresh_flake" "$system")

  for profile in default pinned; do
    count=$(count_nightly_in_closures "$(jq -c ".$profile" <<<"$derivations")")
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
