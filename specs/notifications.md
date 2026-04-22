# Notification System

Desktop notifications when Claude Code needs attention.

## Problem Statement

When Claude Code stops and waits for input, users may not notice if:
- They're working in another window
- The terminal is in a background tab
- They stepped away from the computer

Users need non-intrusive notifications that:
- Alert when Claude needs attention
- Don't spam when the terminal is already focused
- Work across container boundaries

## Requirements

### Functional

1. **Client Command** - `wrapix-notify` sends notifications from container
2. **Host Daemon** - `wrapix-notifyd` receives and displays notifications
3. **Cross-Platform Transport**
   - Linux: Unix socket mounted from host
   - macOS: TCP to gateway IP (virtio-fs limitation)
4. **Focus-Aware Suppression** - Skip notification if terminal is focused
5. **Session Tracking** - Associate notifications with tmux sessions
6. **Sound Support** - Optional notification sounds

### Non-Functional

1. **Low Latency** - Notifications appear within 1 second
2. **Reliable** - Daemon handles disconnects gracefully
3. **Non-Blocking** - Client exits immediately after sending

## Architecture

```
Container                          Host
---------                          ----
wrapix-notify                      wrapix-notifyd
    │                                  │
    ├─ Linux: Unix socket ────────────►├─ notify-send
    │   (/run/wrapix/notify.sock)      │
    │                                  │
    └─ macOS: TCP:5959 ───────────────►└─ terminal-notifier
```

## Protocol

Newline-delimited JSON:

```json
{"title": "Claude Code", "message": "Waiting", "sound": "Ping", "session_id": "0:1.0"}
```

| Field | Required | Description |
|-------|----------|-------------|
| title | Yes | Notification title |
| message | Yes | Notification body |
| sound | No | macOS sound name |
| session_id | No | tmux session for focus detection |

## Focus Detection

1. Launcher registers tmux session with window ID
2. Session file stored in runtime directory
3. Daemon checks if session's window is focused
4. Notification suppressed if focused

| Platform | Focus Detection Method |
|----------|----------------------|
| Linux (niri) | Query window ID via compositor |
| macOS | Check frontmost application |

## Affected Files

| File | Role |
|------|------|
| `lib/notify/client.nix` | Container notification sender |
| `lib/notify/daemon.nix` | Host notification receiver |
| `lib/sandbox/*/entrypoint.sh` | Session registration |

## Environment Variables

| Variable | Description |
|----------|-------------|
| `WRAPIX_NOTIFY_ALWAYS=1` | Disable focus checking |
| `WRAPIX_NOTIFY_VERBOSE=1` | Enable debug logging |
| `WRAPIX_NOTIFY_TCP=host:port` | Override TCP endpoint |

## Claude Code Hook Configuration

```json
{
  "hooks": {
    "Stop": [{
      "matcher": "",
      "hooks": [{
        "type": "command",
        "command": "wrapix-notify 'Claude' 'Waiting'"
      }]
    }]
  }
}
```

## Success Criteria

- [ ] Notifications appear on Linux with notify-send
  [judge](../tests/judges/notifications.sh#test_linux_notify_send)
- [ ] Notifications appear on macOS with terminal-notifier
  [judge](../tests/judges/notifications.sh#test_macos_terminal_notifier)
- [ ] No notification when terminal is focused
  [judge](../tests/judges/notifications.sh#test_focus_suppression)
- [ ] Daemon handles multiple simultaneous connections
  [judge](../tests/judges/notifications.sh#test_concurrent_connections)
- [ ] Client works from inside container
  [judge](../tests/judges/notifications.sh#test_client_in_container)

## Security Considerations

### macOS TCP Transport

On macOS, the notification daemon listens on TCP port 5959 bound to 192.168.64.1 (the vmnet gateway). This is necessary because VirtioFS cannot pass Unix sockets between the host and containers.

**Exposure**: Any container running on the vmnet bridge can send notifications to the host. There is no authentication on the notification protocol.

**Impact**: Low. Notifications are cosmetic - they trigger native desktop alerts but cannot execute code. The worst case is unwanted notification spam.

**Mitigations**:
- Port bound to vmnet interface only (not 0.0.0.0), limiting exposure to local containers
- Focus-aware suppression prevents notifications when terminal is focused
- Notifications are rate-limited by the natural latency of container communication

Rate limiting was considered but rejected as the added complexity outweighs the marginal benefit for this low-impact scenario.

### Linux Unix Socket

On Linux, the daemon uses a Unix socket mounted into containers. This provides inherent access control through filesystem permissions - only containers with the socket mounted can send notifications.

## Out of Scope

- Mobile/remote notifications
- Notification history
- Custom notification actions
