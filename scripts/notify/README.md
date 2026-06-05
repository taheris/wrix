# Wrix Notification Daemon

Runs `wrix-notifyd` on the host to receive notifications from containers.

## Overview

The daemon triggers native desktop notifications when Claude Code (inside a
container) needs attention.

**Transport**:
- **macOS**: TCP port 5959 - VirtioFS cannot pass Unix socket operations
- **Linux**: Unix socket (`~/.local/share/wrix/notify.sock`)

On macOS, the daemon listens on both TCP (for containers) and Unix socket
(for local testing). Containers connect to the host via the gateway IP.

## Installation

### Home-manager (recommended)

Add `wrix-notifyd` to your home-manager configuration for automatic startup.

**macOS** (launchd):

```nix
launchd.agents.wrix-notifyd = {
  enable = true;
  config = {
    ProgramArguments = [ "${pkgs.wrix-notifyd}/bin/wrix-notifyd" ];
    KeepAlive = true;
    RunAtLoad = true;
    StandardOutPath = "/tmp/wrix-notifyd.log";
    StandardErrorPath = "/tmp/wrix-notifyd.log";
  };
};
```

**Linux** (systemd):

```nix
systemd.user.services.wrix-notifyd = {
  Unit = {
    Description = "Wrix notification daemon";
    After = [ "graphical-session.target" ];
  };
  Service = {
    Type = "simple";
    ExecStart = "${pkgs.wrix-notifyd}/bin/wrix-notifyd";
    Restart = "always";
    RestartSec = 5;
  };
  Install = {
    WantedBy = [ "graphical-session.target" ];
  };
};
```

Rebuild your configuration and the daemon will start automatically.

### Manual Installation

For users not using home-manager, follow the platform-specific instructions below.

#### Files

- `com.local.wrix-notifyd.plist` - macOS LaunchAgent template
- `wrix-notifyd.service` - Linux systemd user service template

## macOS Manual Installation

### Option A: Run manually

```bash
nix run github:taheris/wrix#wrix-notifyd
```

### Option B: Install as LaunchAgent

1. Build the daemon and note its path:

```bash
nix build github:taheris/wrix#wrix-notifyd
DAEMON_PATH="$(nix path-info github:taheris/wrix#wrix-notifyd)/bin/wrix-notifyd"
echo "Daemon path: $DAEMON_PATH"
```

2. Generate the plist with the correct paths:

```bash
# Create log directory (XDG-compliant location)
LOG_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/wrix"
mkdir -p "$LOG_DIR"

# Generate plist with daemon and log paths
sed -e "s|__DAEMON_PATH__|$DAEMON_PATH|g" \
    -e "s|__LOG_DIR__|$LOG_DIR|g" \
    scripts/notify/com.local.wrix-notifyd.plist \
    > ~/Library/LaunchAgents/com.local.wrix-notifyd.plist
```

3. Load the agent:

```bash
launchctl load ~/Library/LaunchAgents/com.local.wrix-notifyd.plist
```

4. Verify it's running:

```bash
launchctl list | grep wrix-notifyd
tail -f ~/.local/state/wrix/wrix-notifyd.log
```

### macOS Manual Uninstall

```bash
launchctl unload ~/Library/LaunchAgents/com.local.wrix-notifyd.plist
rm ~/Library/LaunchAgents/com.local.wrix-notifyd.plist
```

## Linux Manual Installation

### Option A: Run manually

```bash
nix run github:taheris/wrix#wrix-notifyd
```

### Option B: Install as systemd user service (recommended)

1. Build the daemon and note its path:

```bash
nix build github:taheris/wrix#wrix-notifyd
DAEMON_PATH="$(nix path-info github:taheris/wrix#wrix-notifyd)/bin/wrix-notifyd"
echo "Daemon path: $DAEMON_PATH"
```

2. Generate the service file with the correct path:

```bash
mkdir -p ~/.config/systemd/user
sed "s|__DAEMON_PATH__|$DAEMON_PATH|g" scripts/notify/wrix-notifyd.service \
    > ~/.config/systemd/user/wrix-notifyd.service
```

3. Enable and start the service:

```bash
systemctl --user daemon-reload
systemctl --user enable --now wrix-notifyd
```

4. Verify it's running:

```bash
systemctl --user status wrix-notifyd
journalctl --user -u wrix-notifyd -f
```

### Linux Manual Uninstall

```bash
systemctl --user disable --now wrix-notifyd
rm ~/.config/systemd/user/wrix-notifyd.service
systemctl --user daemon-reload
```

## Testing

From inside a wrix container:

```bash
wrix-notify "Test" "Hello from container"
```

You should see a notification appear on your desktop.
