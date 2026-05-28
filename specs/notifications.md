# Notification System

Desktop notifications when Claude Code needs attention.

## Problem Statement

When Claude Code stops and waits for input, users may not notice if they are working in another window, the terminal is in a background tab, or they have stepped away. Notifications must alert when the agent needs attention, suppress when the terminal is already focused, and traverse the container/host boundary.

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

Two processes: `wrapix-notify` is the in-container client invoked from a Claude Code Stop hook; `wrapix-notifyd` is the host-side daemon that displays notifications via the platform's native bridge. Transport differs by platform because VirtioFS cannot pass Unix sockets between host and container.

## Wire Protocol

Newline-delimited JSON, one envelope per notification:

```json
{"title": "Claude Code", "message": "Waiting", "sound": "Ping", "session_id": "0:1.0"}
```

| Field | Required | Description |
|-------|----------|-------------|
| `title` | Yes | Notification title |
| `message` | Yes | Notification body |
| `sound` | No | macOS sound name |
| `session_id` | No | tmux session for focus detection |

## Focus Detection

1. Launcher registers the tmux session with a window ID
2. The session file lands in the runtime directory
3. Daemon checks whether the session's window is focused
4. Notification is suppressed if focused

| Platform | Focus Detection Method |
|----------|----------------------|
| Linux (niri) | Query window ID via compositor |
| macOS | Check frontmost application |

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

## Security

The daemon's macOS TCP transport binds to the vmnet gateway (192.168.64.1:5959), reachable only from containers on the vmnet bridge. There is no authentication on the notification protocol; the worst-case abuse is unwanted desktop notification spam, since notifications are cosmetic and cannot execute code. Linux uses a Unix socket mounted into the container — filesystem permissions on the socket provide access control.

## Success Criteria

- Linux client sends a notification through the mounted Unix socket and the host daemon dispatches it via `notify-send`
  [system](bash tests/notify/linux-notify-send.sh)
- macOS client sends a notification over TCP to the gateway address and the host daemon dispatches it via `terminal-notifier`
  [system](bash tests/notify/macos-terminal-notifier.sh)
- Daemon suppresses the notification when the registered tmux session's window is focused
  [system](bash tests/notify/focus-suppression.sh)
- N concurrent clients each send one envelope and all N notifications are dispatched to the host bridge (no envelopes silently discarded)
  [system](bash tests/notify/concurrent-connections.sh)
- `wrapix-notify` invoked from inside a container reaches the host daemon
  [system](bash tests/notify/client-in-container.sh)
- macOS TCP listener binds to the vmnet gateway address (`192.168.64.1`); `0.0.0.0` is not used as a bind address
  [check](grep -nE '192\.168\.64\.1|0\.0\.0\.0' lib/notify/daemon.nix)

## Requirements

### Functional

1. **Client command** — `wrapix-notify <title> <message>` sends a notification envelope from inside the container.
2. **Host daemon** — `wrapix-notifyd` receives envelopes and dispatches via the platform-native notification bridge.
3. **Cross-platform transport** — Linux uses a Unix socket bind-mounted into the container at `/run/wrapix/notify.sock`; macOS uses TCP to the vmnet gateway (5959). VirtioFS does not pass Unix sockets between host and container, so TCP is mandatory on macOS.
4. **Focus-aware suppression** — when the registered tmux session's window is focused, the daemon discards the notification before dispatch.
5. **Session tracking** — the launcher registers `(session_id, window_id)` on container start so focus detection has a target.
6. **Sound support** — clients may pass an optional `sound` field consumed by `terminal-notifier` on macOS.

### Non-Functional

1. **Low latency** — notifications appear within one second of `wrapix-notify` invocation.
2. **Reliable** — daemon survives client disconnects and accepts new connections without restart.
3. **Non-blocking client** — `wrapix-notify` exits immediately after writing the envelope, with no acknowledgement round-trip.

## Out of Scope

- Mobile / remote notifications
- Notification history
- Custom notification actions
- Rate limiting (rejected — the latency of container→host hops is itself rate-limiting; the added complexity is not justified for cosmetic notifications)
