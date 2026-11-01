#!/usr/bin/env bash
set -euo pipefail

: "${NEXTEST_ENV:?nextest must provide the setup environment file}"
# Fixture HOME directories must not inherit the caller's admission store/cache.
# Wrix treats empty state overrides as HOME defaults. Nix does not do the same
# for cache overrides, so give external tools a nextest-owned scratch cache.
cache_root="${NEXTEST_ENV}.cache"
mkdir -m 700 "$cache_root"
cache_root="$(cd "$cache_root" && pwd -P)"
# Darwin socket fixtures need a short root, not the nested nix-shell TMPDIR.
temporary_root="$(cd /tmp && pwd -P)"
printf '%s\n' 'XDG_STATE_HOME=' "XDG_CACHE_HOME=$cache_root" "TMPDIR=$temporary_root" >>"$NEXTEST_ENV"
