#!/usr/bin/env bash
#
# Event-driven daemon that monitors route changes and re-applies
# Tailscale routing fixes when needed.
#
# This listens to the kernel routing socket and triggers setup-routing.sh
# whenever routes change (e.g., when Tailscale reconnects or switches exit nodes).
#
# Usage: Run as a LaunchDaemon (see com.local.tailscale-routing.plist)
#

set -euo pipefail

# Ensure Homebrew binaries (including tailscale) are in PATH for LaunchDaemons
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SETUP_SCRIPT="$SCRIPT_DIR/setup-routing.sh"

log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $*"
}

log "Starting Tailscale routing daemon"
log "Monitoring route changes..."

# Run setup once at start.
# best-effort: the daemon must keep running even when setup-routing fails
# (no exit node yet, transient pfctl error). Capture the failure to the
# log so we know it happened, but don't propagate.
if [[ -x "$SETUP_SCRIPT" ]]; then
    log "Running initial route setup"
    "$SETUP_SCRIPT" 2>&1 || log "Initial setup skipped (exit node may not be active)"
fi

# Monitor route changes and re-apply when needed
route -n monitor | while read -r _; do
    # Debounce: wait for route changes to settle
    sleep 1

    # Only act if Tailscale is active with exit node.
    # best-effort: status non-zero / stderr just means "no exit node right
    # now" — fine, we just skip this tick.
    if tailscale status --json 2>/dev/null | grep -q '"ExitNodeStatus"'; then
        log "Route change detected, re-applying Tailscale routing fix"
        # best-effort: setup-routing may fail mid-reconfiguration; log and
        # wait for the next route change to retry.
        "$SETUP_SCRIPT" 2>&1 || log "Setup failed (will retry on next change)"
    fi
done
