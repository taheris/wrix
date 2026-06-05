# Tailscale Routing Fix for macOS Containers

Fixes networking for Darwin containers when using Tailscale with an exit node.

## Problem

Tailscale's exit node routes all traffic (including `192.168.x.x`) through the
VPN tunnel, which breaks connectivity to containers on the vmnet bridge
(`192.168.64.0/24`).

## Solution

These scripts override Tailscale's routes to keep container traffic local.

## Files

- `setup-routing.sh` - One-shot script to fix routes (run manually with sudo)
- `routing-daemon.sh` - Event-driven daemon that monitors route changes
- `com.local.tailscale-routing.plist` - LaunchDaemon config for automatic fixing

## Installation

### Option A: Run manually when needed

```bash
sudo scripts/tailscale/setup-routing.sh
```

### Option B: Install as LaunchDaemon (recommended)

1. Edit the plist to set the correct paths:

```bash
SCRIPT_PATH="$(cd scripts/tailscale && pwd)"
LOG_DIR="/var/log/wrix"
sudo mkdir -p "$LOG_DIR"

sed -e "s|__SCRIPT_PATH__|$SCRIPT_PATH|g" \
    -e "s|__LOG_DIR__|$LOG_DIR|g" \
    scripts/tailscale/com.local.tailscale-routing.plist \
    > /tmp/com.local.tailscale-routing.plist
```

2. Install and load:

```bash
sudo cp /tmp/com.local.tailscale-routing.plist /Library/LaunchDaemons/
sudo launchctl load /Library/LaunchDaemons/com.local.tailscale-routing.plist
```

3. Verify it's running:

```bash
sudo launchctl list | grep tailscale-routing
tail -f /var/log/wrix/tailscale-routing.log
```

## Uninstall

```bash
sudo launchctl unload /Library/LaunchDaemons/com.local.tailscale-routing.plist
sudo rm /Library/LaunchDaemons/com.local.tailscale-routing.plist
```
