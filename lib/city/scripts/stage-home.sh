#!/usr/bin/env bash
# Stage a gc home directory that isolates gc from the host's .beads/.
#
# gc writes dolt.auto-start: false and dolt-server.port to .beads/,
# which corrupts the host's beads config.  It ignores BEADS_DIR.
#
# Fix: create .gc/home/ with its own .beads/ containing config copies
# and the city's dolt port (not the host's).  gc discovers .beads/ by
# walking up from cwd, so running gc from .gc/home/ (or setting
# GC_CITY=.gc/home) makes all gc writes go there instead.
#
# The .beads/ in gc home has:
#   - config.yaml    — copy from host, plus dolt.auto-start: false
#   - metadata.json  — copy from host
#   - issues.jsonl   — copy from host (if present)
#   - dolt-server.port — city dolt port (so gc connects to the container)
#   - NO dolt/ dir   — prevents gc's dolt pack from starting a duplicate
#
# Environment:
#   GC_WORKSPACE  — workspace root (required)
#   GC_DOLT_PORT  — city dolt port (default: 3306)
#
# Output: prints the gc home path; caller should export GC_CITY to it.
set -euo pipefail

: "${GC_WORKSPACE:?stage-home.sh requires GC_WORKSPACE}"
DOLT_PORT="${GC_DOLT_PORT:-3306}"

GC_HOME="${GC_WORKSPACE}/.gc/home"
rm -rf "$GC_HOME"
mkdir -p "$GC_HOME/.gc"
mkdir -p "$GC_HOME/.beads"
chmod 700 "$GC_HOME/.beads"

# Copy beads config files
for f in config.yaml metadata.json issues.jsonl; do
  [ -f "${GC_WORKSPACE}/.beads/$f" ] && cp "${GC_WORKSPACE}/.beads/$f" "$GC_HOME/.beads/"
done

# Disable dolt auto-start (gc should use the city's dolt container)
# and record the city dolt port so gc's internal bd calls connect there.
if ! grep -q '^dolt\.auto-start:' "$GC_HOME/.beads/config.yaml" 2>/dev/null; then
  echo "dolt.auto-start: false" >> "$GC_HOME/.beads/config.yaml"
fi
echo "$DOLT_PORT" > "$GC_HOME/.beads/dolt-server.port"

# City config and .gc subdirectories.
# Strip workspace.provider — a stale "claude" value causes gc to use its
# built-in tmux provider for display commands instead of the exec session
# provider, making gc status/peek fail with "no tmux server". (wx-y4tx2)
if [[ -f "${GC_WORKSPACE}/city.toml" ]]; then
  sed '/^\[workspace\]/,/^\[/{/^provider = /d}' \
    "${GC_WORKSPACE}/city.toml" > "$GC_HOME/city.toml"
else
  true
fi
for d in formulas scripts prompts; do
  [ -d "${GC_WORKSPACE}/.gc/$d" ] && ln -sfn "../../$d" "$GC_HOME/.gc/$d"
done

# gc needs these writable dirs
mkdir -p "$GC_HOME/.gc/cache" "$GC_HOME/.gc/system" "$GC_HOME/.gc/runtime" "$GC_HOME/.gc/nudges"
touch "$GC_HOME/.gc/events.jsonl"

# bd requires a git repo at the working directory root.  Without this,
# gc's bd subprocess calls fail with "cannot determine repository root".
if [[ ! -d "$GC_HOME/.git" ]]; then
  git init -q "$GC_HOME"
fi

echo "$GC_HOME"
