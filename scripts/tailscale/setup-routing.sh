#!/usr/bin/env bash
#
# Configure macOS routing for Darwin containers with Tailscale exit node.
# Run this if containers can't reach the internet when using Tailscale
# with "Allow Local Network Access" enabled.
#
# Usage: sudo scripts/setup-tailscale-routing.sh
#

set -euo pipefail

die() { echo "error: $*" >&2; exit 1; }

# Must be root
[[ $EUID -eq 0 ]] || die "run with sudo"

# Check Tailscale is active with exit node
command -v tailscale >/dev/null || die "tailscale not installed"
# best-effort: status exits non-zero (with a stderr message) when the daemon
# is down; we only care about the exit code and convert it to our own error.
tailscale status --json >/dev/null 2>&1 || die "tailscale not running"

# Use subshell without pipefail to avoid SIGPIPE when grep -q closes the
# pipe early on a huge JSON status blob. Stderr suppressed because grep
# is the contract; tailscale-side warnings would just add noise here.
if ! (set +o pipefail; tailscale status --json 2>/dev/null | grep -q '"ExitNodeStatus"'); then
    die "no exit node configured"
fi

# Find Tailscale interface (utun with 100.x.x.x)
TAILSCALE_IF=$(netstat -rn | grep "^100\.64/10.*utun" | awk '{print $NF}' | head -1)
[[ -n "$TAILSCALE_IF" ]] || die "tailscale interface not found"

# vmnet subnet and bridge used by Apple container CLI
VMNET_SUBNET="192.168.64.0/24"
VMNET_BRIDGE="bridge100"

echo "Configuring routes for Tailscale + vmnet compatibility..."
echo "  Tailscale interface: $TAILSCALE_IF"
echo "  vmnet subnet: $VMNET_SUBNET"
echo "  vmnet bridge: $VMNET_BRIDGE"

# 1. Route vmnet subnet to bridge100 (so return traffic reaches containers).
#    Tailscale adds routes for 192.168.x.x through utun6 which breaks return path.
# best-effort: `route delete` fails noisily when no matching route exists
# (first run); this script must be idempotent so we swallow that case.
route delete -net "$VMNET_SUBNET" 2>/dev/null || true
route add -net "$VMNET_SUBNET" -interface "$VMNET_BRIDGE"
echo "  Added route: $VMNET_SUBNET -> $VMNET_BRIDGE"

# 2. Add NAT rule to forward container traffic through Tailscale.
#    This makes outbound container traffic go through the exit node.
# best-effort: `-F nat` on an empty anchor warns to stderr; the flush is
# idempotent in either direction, we just want to start from a clean slate.
pfctl -a "com.apple.internet-sharing" -F nat 2>/dev/null || true
# best-effort: pfctl chatters about syntax/loading on stderr; the exit
# code is the contract.
echo "nat on $TAILSCALE_IF from $VMNET_SUBNET to any -> ($TAILSCALE_IF)" | \
    pfctl -a "com.apple.internet-sharing" -f - 2>/dev/null
echo "  Added NAT: $VMNET_SUBNET -> $TAILSCALE_IF"

echo ""
echo "Done. Container networking should now work with Tailscale exit node."
echo ""
echo "Note: Settings reset on reboot. Re-run this script after restart if needed."
