#!/usr/bin/env bash
# Verify that the rust profile's toolchain closures contain zero *-nightly-*
# derivations. Regression guard against re-introducing
# fenix.packages.${system}.rust-analyzer (built from the nightly source branch),
# which drags a matching nightly cargo/rustc/rust-std closure into every
# downstream flake on each input update.
#
# Usage: tests/profiles/no-nightly-closure.sh [test_no_nightly_closure]
# Exit 0 on success, 1 on regression, with a clear stderr message on failure.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

# Pinned sha256 for tests/fixtures/rust-toolchain.toml (channel 1.75.0).
# Update this if the fixture's channel/components change.
FIXTURE_SHA="sha256-SXRtAuO4IqNOQq+nLbrsDFbVk+3aVA8NNpSZsKlVH/8="

count_nightly_in_closure() {
  local drv="$1"
  nix-store -qR "$drv" | awk 'tolower($0) ~ /nightly/ {n++} END {print n+0}'
}

test_no_nightly_closure() {
  local fixture_path="$REPO_ROOT/tests/fixtures/rust-toolchain.toml"
  if [[ ! -f "$fixture_path" ]]; then
    echo "fixture not found: $fixture_path" >&2
    return 1
  fi

  local flake_url="git+file://$REPO_ROOT"

  local system
  if ! system=$(nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem'); then
    echo "nix eval builtins.currentSystem failed" >&2
    return 1
  fi

  local default_drv
  if ! default_drv=$(nix eval --raw --impure --no-warn-dirty --expr "
    (builtins.getFlake \"$flake_url\").legacyPackages.${system}.lib.profiles.rust.toolchain.drvPath
  "); then
    echo "nix eval for default rust toolchain drvPath failed" >&2
    return 1
  fi

  local default_count
  default_count=$(count_nightly_in_closure "$default_drv")
  if [[ "$default_count" -ne 0 ]]; then
    echo "default rust toolchain closure contains $default_count nightly-* derivation(s) — likely regression to fenix.packages.\${system}.rust-analyzer" >&2
    return 1
  fi

  local pinned_drv
  if ! pinned_drv=$(nix eval --raw --impure --no-warn-dirty --expr "
    let
      flake = builtins.getFlake \"$flake_url\";
      lib = flake.legacyPackages.${system}.lib;
      pinned = lib.rustProfile { toolchain = $fixture_path; sha256 = \"$FIXTURE_SHA\"; };
    in pinned.toolchain.drvPath
  "); then
    echo "nix eval for rustProfile toolchain drvPath failed" >&2
    return 1
  fi

  local pinned_count
  pinned_count=$(count_nightly_in_closure "$pinned_drv")
  if [[ "$pinned_count" -ne 0 ]]; then
    echo "rustProfile toolchain closure contains $pinned_count nightly-* derivation(s) — likely regression to fenix.packages.\${system}.rust-analyzer" >&2
    return 1
  fi
}

fn="${1:-test_no_nightly_closure}"
if ! declare -f "$fn" >/dev/null 2>&1; then
  echo "Unknown function: $fn" >&2
  exit 1
fi
"$fn"
