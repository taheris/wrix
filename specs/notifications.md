# Notification System

Focus-aware desktop notifications when a sandboxed coding agent needs attention.

## Problem Statement

Users may miss an agent waiting for input while its terminal is in the
background. Wrix carries agent attention signals across the container boundary
to the host desktop and suppresses notifications when the associated terminal is
already focused, without treating conversation completion as container exit.

## Architecture

`wrix-notify` is the in-container client; `wrix-notifyd` is the host daemon.
Linux uses the mounted Unix socket `/run/wrix/notify.sock` and `notify-send`.
Darwin uses TCP on the vmnet gateway and `terminal-notifier`, consistent with
the mount contract in `sandbox.md`. Agents use small lifecycle adapters, not a
Wrix-owned event bus or agent framework.

Claude's native `Stop` hook invokes the client. Pi's adapter invokes it on
`agent_settled`, when Pi has no automatic continuation pending, not on
`agent_end`, which can precede recovery or queued work. Pi sends one
notification per settled event, with an agent-identifying title and a
waiting-for-input message. Consumer direct runners may invoke the same client
themselves; Wrix does not infer their conversation state from process output.

The focus target belongs to the host terminal, not a debugging tmux session
inside the container. Execution and conversation identities in `security.md` are
separate from this routing value.

[cli.md](cli.md#verifier-results-and-worker-acceptance) owns verifier reporting
and sandbox-stage skip acceptance. A platform or runtime skip is not evidence of
notification delivery; live transport checks remain in the host-test stage.

## Wire Protocol

`wrix-notify <title> <message> [sound]` sends one newline-delimited JSON
envelope:

```json
{"title": "Pi", "message": "Waiting for input", "sound": "Ping", "focus_target": "0:1.0"}
```

`title` and `message` are required strings. `sound` is an optional macOS sound
name. `focus_target` is an optional opaque string identifying the registered
host terminal/tmux target. It is not an agent session ID; `session_id` is not a
wire alias. The client writes without waiting for an acknowledgement.

The launcher exports its registered host routing target to the container as
`WRIX_FOCUS_TARGET`. The client copies a nonempty value into `focus_target`;
when the variable is unset or empty, it omits the field and still sends the
notification. `WRIX_SESSION_ID` is not an input or compatibility alias. This
handoff does not use an agent conversation ID or discover a debugging tmux pane
inside the container.

## Focus Detection

The launcher registers the host terminal target at container start using the
same runtime-file naming as the daemon. Linux/niri records its window ID; Darwin
records its terminal application. Where applicable, tmux's active pane is also
checked. The daemon suppresses only when it positively identifies the registered
target as focused. Missing registration or unavailable focus information does
not suppress an otherwise valid notification.

`WRIX_NOTIFY_ALWAYS=1` bypasses focus checking, `WRIX_NOTIFY_VERBOSE=1` enables
diagnostic logging, and `WRIX_NOTIFY_TCP=host:port` overrides the client's TCP
endpoint.

## Pending Attention Navigation

Install the host package `wrix-focus-notification` and invoke it from a
consumer-owned keybinding. Desktop keybindings are not part of Wrix.

After successful native dispatch, the daemon records usable registered targets
in a host-local pending queue. Suppressed notifications are not queued. Repeated
targets move to the front; navigation visits newest first and consumes only
successful navigation or stale entries. Missing registrations, changed routes,
closed niri windows, and missing/dead tmux panes are discarded. Concurrent
delivery and navigation serialize queue transactions, including navigation, so a
newly delivered notification cannot be lost by a previous visit.
[system](verify:notifications.attention-navigation)

Linux navigation requires niri and focuses the registered window after selecting
the registered host tmux window/pane through its registered socket. Darwin uses
System Events accessibility to raise a uniquely named registered terminal
window, then selects its host tmux window/pane. The launcher records the
foreground terminal window title on Darwin. Accessibility permission is needed
for both registration and navigation; missing permission leaves notification
delivery intact. App-only legacy registrations are not navigable. Changed or
duplicate window titles are deliberately skipped rather than activating an
arbitrary Alacritty window. This is best-effort title-based macOS routing, not
stable window-ID routing; Linux/niri does not have that limitation. For multiple
Alacritty windows on Darwin, use unique stable window titles (disable dynamic
titles in the consuming Alacritty configuration). Live macOS window raising
requires validation on the consuming desktop; the portable verifier covers
argument routing, not macOS Accessibility itself.
[system](verify:notifications.attention-navigation)

Navigation happens only on explicit host invocation, never on delivery. Unrouted
notifications still dispatch normally. Queue storage failures are diagnosed
without failing notification delivery. Native dispatch failures do not enqueue
attention and do not stop later notifications.
[system](verify:notifications.attention-navigation)
[system](verify:notifications.focus-target-registration)

## Security

The Darwin listener binds to the vmnet gateway (`192.168.64.1:5959`), not
`0.0.0.0`. The protocol has no authentication; it carries cosmetic desktop
notifications, not commands to execute. A payload may refresh an existing
host-registered attention key, but cannot supply navigation commands, tmux
sockets, or window identities. Navigation resolves those fields from host
registrations and invokes fixed commands with separate arguments. The SQLite
queue is host-local, created with mode 0600, and not mounted into sandboxes. An
unauthenticated sender can spoof attention for a known registered key; explicit
host invocation is still required to move focus. Linux socket filesystem
permissions provide transport access control. Notification failure does not
terminate agent work, but failures are reported through diagnostics rather than
hidden.

## Success Criteria

- On Linux, the packaged client invoked through `wrix spawn` reaches the host
  daemon through the mounted Unix socket
  [system](verify:notifications.container-transport-linux)
- On Darwin, the packaged client invoked through `wrix spawn` reaches the host
  daemon through TCP on the vmnet gateway, including optional sound delivery
  [system](verify:notifications.container-transport-darwin)
- `WRIX_NOTIFY_TCP=host:port` selects the client TCP endpoint
  [system](verify:notifications.client-tcp-endpoint-override)
- The client sends exactly one envelope with title, message, optional sound, and
  `focus_target` copied from nonempty `WRIX_FOCUS_TARGET`; an unset or empty
  variable omits the field without preventing delivery, and neither
  `WRIX_SESSION_ID` nor a `session_id` wire alias is used
  [system](verify:notifications.focus-target-envelope)
- The client exits without waiting for an acknowledgement
  [system](verify:notifications.client-non-blocking)
- A notification reaches the daemon's native bridge within one second of client
  invocation [system](verify:notifications.daemon-dispatch-latency)
- Claude settings invoke `wrix-notify` from the native `Stop` hook
  [check](verify:notifications.claude-stop-hook-config)
- Packaged Pi emits one attention notification on settling, but none at an
  intermediate `agent_end` while recovery or queued work continues
  [system](verify:notifications.pi-settled)
- A Pi notification carries an agent-identifying title and the registered host
  focus target through the production client/daemon path
  [system](verify:notifications.pi-focus-routing)
- Notification transport failures are diagnosed without stopping Pi's agent work
  or turning a completed turn into a failed turn
  [system](verify:notifications.pi-notify-failure)
- Host native dispatch remains available after client disconnects
  [judge](../tests/judges/notifications.sh#test_native_dispatch_and_reliability)
- Launcher registration, the exported `WRIX_FOCUS_TARGET`, and daemon lookup
  agree on the opaque host target, suppress only positively focused targets, and
  never substitute an execution ID, agent conversation ID, or in-container
  debugging pane ID [system](verify:notifications.focus-target-registration)
- `WRIX_NOTIFY_ALWAYS=1` disables focus checking
  [system](verify:notifications.focus-override)
- `WRIX_NOTIFY_VERBOSE=1` enables diagnostic logging
  [system](verify:notifications.verbose-logging)
- The Darwin TCP listener binds to `192.168.64.1`, never `0.0.0.0`
  [check](verify:notifications.macos-tcp-bind-address)

## Requirements

### Functional

1. **Agent adapters** — Claude uses Stop and Pi uses settled as described in
   Architecture; external runners own their own attention semantics.
2. **Transport** — the client and host daemon implement the wire and platform
   contracts above, independently of the selected agent.
3. **Focus routing** — host terminal registration, not conversation identity,
   controls focus suppression.

### Non-Functional

1. **Low overhead** — the client does not await an acknowledgement, and native
   dispatch meets the one-second bound above.
2. **Best-effort notification** — failures remain visible but do not prevent
   agent work; disconnected clients do not terminate the daemon.

## Out of Scope

- Mobile or remote notifications, notification history, custom actions, and rate
  limiting; this is a local desktop attention channel
- A general lifecycle event bus or automatic recovery; agents own settled-state
  semantics and `security.md` owns execution records
- Automatic lifecycle detection for arbitrary consumer direct runners
