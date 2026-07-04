# Notification System

Desktop notifications when Claude Code needs attention.

## Problem Statement

When Claude Code stops and waits for input, users may not notice if they are working in another window, the terminal is in a background tab, or they have stepped away. Notifications must alert when the agent needs attention, suppress when the terminal is already focused, and traverse the container/host boundary.

## Architecture

```
Container                          Host
---------                          ----
wrix-notify                      wrix-notifyd
    │                                  │
    ├─ Linux: Unix socket ────────────►├─ notify-send
    │   (/run/wrix/notify.sock)      │
    │                                  │
    └─ macOS: TCP:5959 ───────────────►└─ terminal-notifier
```

Two processes: `wrix-notify` is the in-container client invoked from a Claude Code Stop hook; `wrix-notifyd` is the host-side daemon that displays notifications via the platform's native bridge. Transport differs by platform because VirtioFS cannot pass Unix sockets between host and container.

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

1. Launcher registers the tmux session with the platform focus target
2. The session file lands in the runtime directory
3. Daemon checks whether the registered target is focused
4. Notification is suppressed if focused

| Platform | Focus Detection Method |
|----------|----------------------|
| Linux (niri) | Query window ID via compositor |
| macOS | Check frontmost application |

## Environment Variables

| Variable | Description |
|----------|-------------|
| `WRIX_NOTIFY_ALWAYS=1` | Disable focus checking |
| `WRIX_NOTIFY_VERBOSE=1` | Enable debug logging |
| `WRIX_NOTIFY_TCP=host:port` | Override TCP endpoint |

## Claude Code Hook Configuration

```json
{
  "hooks": {
    "Stop": [{
      "matcher": "",
      "hooks": [{
        "type": "command",
        "command": "wrix-notify 'Claude' 'Waiting'"
      }]
    }]
  }
}
```

## Security

The daemon's macOS TCP transport binds to the vmnet gateway (192.168.64.1:5959), reachable only from containers on the vmnet bridge. There is no authentication on the notification protocol; the worst-case abuse is unwanted desktop notification spam, since notifications are cosmetic and cannot execute code. Linux uses a Unix socket mounted into the container — filesystem permissions on the socket provide access control.

## Success Criteria

- `wrix-notify` invoked through `wrix spawn` from inside a container reaches a running host daemon via the platform-appropriate transport (Linux Unix socket, Darwin TCP to gateway); skips with exit 77 when the container runtime is unavailable
  [system](verify:notifications.container-transport)
- `wrix-notify` honors `WRIX_NOTIFY_TCP=host:port`, sends exactly one JSON envelope containing title, message, optional sound, and `session_id`, and exits without waiting for an acknowledgement
  [system](verify:notifications.client-tcp-endpoint-override)
- Claude Code settings invoke `wrix-notify` from a `Stop` hook
  [check](verify:notifications.claude-stop-hook-config)
- The host daemon dispatches via native notification bridges and continues serving after client disconnects
  [judge](../tests/judges/notifications.sh#test_native_dispatch_and_reliability)
- Focus-aware suppression happens only when the daemon positively identifies the registered session target as focused
  [judge](../tests/judges/notifications.sh#test_focus_suppression)
- Launchers register tmux session focus targets using the same session-file naming that the daemon reads
  [judge](../tests/judges/notifications.sh#test_session_registration)
- macOS TCP listener binds to the vmnet gateway address (`192.168.64.1`); `0.0.0.0` is not used as a bind address
  [check](verify:notifications.macos-tcp-bind-address)

## Requirements

### Functional

1. **Client command** — `wrix-notify <title> <message>` sends a notification envelope from inside the container.
   [system](verify:notifications.container-transport)
2. **Host daemon** — `wrix-notifyd` receives envelopes and dispatches via the platform-native notification bridge.
   [judge](../tests/judges/notifications.sh#test_native_dispatch_and_reliability)
3. **Cross-platform transport** — Linux uses a Unix socket bind-mounted into the container at `/run/wrix/notify.sock`; macOS uses TCP to the vmnet gateway (5959). VirtioFS does not pass Unix sockets between host and container, so TCP is mandatory on macOS.
   [system](verify:notifications.container-transport)
4. **Focus-aware suppression** — when the registered tmux session's window is focused, the daemon discards the notification before dispatch.
   [judge](../tests/judges/notifications.sh#test_focus_suppression)
5. **Session tracking** — the launcher registers `(session_id, window_id)` on Linux or `(session_id, terminal_app)` on macOS at container start so focus detection has a target.
   [judge](../tests/judges/notifications.sh#test_session_registration)
6. **Sound support** — clients may pass an optional `sound` field consumed by `terminal-notifier` on macOS.
   [system](verify:notifications.client-tcp-endpoint-override)

### Non-Functional

1. **Low latency** — notifications appear within one second of `wrix-notify` invocation.
   [system](verify:notifications.client-tcp-endpoint-override)
2. **Reliable** — daemon survives client disconnects and accepts new connections without restart.
   [judge](../tests/judges/notifications.sh#test_native_dispatch_and_reliability)
3. **Non-blocking client** — `wrix-notify` exits immediately after writing the envelope, with no acknowledgement round-trip.
   [system](verify:notifications.client-tcp-endpoint-override)

## Out of Scope

- Mobile / remote notifications
- Notification history
- Custom notification actions
- Rate limiting (rejected — the latency of container→host hops is itself rate-limiting; the added complexity is not justified for cosmetic notifications)
