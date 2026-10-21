#!/usr/bin/env bash
set -euo pipefail

: "${NEXTEST_ENV:?nextest must provide the setup environment file}"
# Fixture HOME directories must not inherit the caller's admission store/cache.
# Wrix treats empty state overrides as HOME defaults. Nix does not do the same
# for cache overrides, so give external tools a nextest-owned scratch cache.
cache_root="${NEXTEST_ENV}.cache"
mkdir -m 700 "$cache_root"
printf '%s\n' 'XDG_STATE_HOME=' "XDG_CACHE_HOME=$cache_root" >>"$NEXTEST_ENV"
