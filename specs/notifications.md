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

## Native host agents

Install the host packages `wrix`, `wrix-notify-host`, `wrix-notifyd`, and
`wrix-focus-notification`. Start the daemon in the desktop user session. The
supported registration interface is supervised execution:

```sh
wrix notify run -- COMMAND [ARGS ...]
```

Wrix captures origin **before** starting COMMAND, exports an opaque
`WRIX_FOCUS_TARGET` to COMMAND and its descendants, waits for COMMAND, then
releases the registration. No profile config, sandbox, or workspace is required.
An external launcher can read the target in its child environment; it must not
parse it, construct one, or register from a completion callback. Incoming
`WRIX_FOCUS_TARGET` is deliberately replaced, not trusted as origin. Arguments,
standard streams, and ordinary exit codes pass through unchanged.
Registration/cleanup errors are diagnosed but never prevent COMMAND from running
or replace its exit status.
[test](native_notify::registration_failure_still_runs_command_without_inherited_routing)
[test](native_notify::spawn_failure_releases_registration)

Each supervised launch has an independent random target and host-held lease,
even when launches share a terminal/pane. The route uses the same registration,
origin discovery, focus detection and queue as sandbox launches. Tmux routes use
stable pane IDs from `TMUX_PANE`, scoped to the socket from `TMUX`. Reindexing
panes does not redirect attention. Missing/ambiguous origin still allows
notifications, without suppression or navigation.
[test](native_notify::concurrent_launches_at_shared_origin_have_independent_lifetimes)
[system](verify:notifications.attention-navigation)

Run foreground commands that remain alive for the agent's entire lifetime; do
not daemonize or `exec` over the Wrix supervisor. Normal exit releases
registration. If the supervisor is killed, its lease is released by the OS, so
leftover registrations cannot suppress, enqueue or navigate. They may remain on
disk until runtime-directory cleanup; stale queue entries are discarded by
existing navigation. An agent surviving its supervisor can still notify but
loses routing. Targets are not persistent agent identities.
[system](verify:notifications.attention-navigation)

`wrix-notify-host TITLE MESSAGE [SOUND]` copies the exported target and sends
through the host-local Unix socket
`${XDG_RUNTIME_DIR:-$HOME/.local/share}/wrix/notify.sock` on both platforms. It
uses the same envelope and best-effort transport as `wrix-notify`, but does
**not** use `WRIX_NOTIFY_TCP` or the container gateway. The container-only
`wrix-notify` continues using the mounted socket on Linux and TCP on Darwin; do
not use it for native host callbacks.

Minimal consumer-owned native wrapper:

```sh
#!/bin/sh
exec wrix notify run -- codex "$@"
```

Configure Codex's notify command to invoke a callback such as:

```sh
#!/bin/sh
# Codex supplies one JSON argument; other event types are not attention.
if printf '%s\n' "${1:-}" |
    jq -e '.type == "agent-turn-complete"' >/dev/null 2>&1; then
    wrix-notify-host "Codex" "Turn complete" || :
fi
exit 0
```

The callback only reads the inherited target; it does not discover focus,
register an origin, or provide navigation commands. Wrapper/callback
installation and Codex configuration belong to the consumer, not Wrix. Missing
daemon/socket and transport failures are best-effort and diagnosed, never agent
failures. Linux suppression/navigation requires niri and provable process/client
ancestry. Darwin can use consumer-provided OmniWM IPC and captures the focused
managed launch window with proven caller/client ancestry. Both backends support
attention and `--return`; desktop bindings remain consumer-owned. See
[Darwin setup and validation](../docs/darwin-attention.md).
[system](verify:notifications.native-host)

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
same runtime-file naming as the daemon. Linux/niri records its proven
originating window ID and PID; Darwin's OmniWM route records the session-scoped
opaque window ID and PID in `omniwm_route`. The host daemon suppresses only when
that terminal window is focused and, for a tmux origin, the registered pane and
tmux window are active in an attached session. An inactive pane or an active
pane in a background terminal window still notifies. Missing origin metadata,
ambiguous windows, malformed focus responses, or failed focus queries leave
notification delivery unchanged.
[system](verify:notifications.focus-target-registration)
[system](verify:notifications.attention-navigation)

Linux focus suppression currently requires niri IPC and a provable origin; other
compositors/desktops still receive notifications normally. Darwin's optional
OmniWM backend compares the focused-window query's exact ID and PID. Legacy
title-only registrations and unavailable IPC do not suppress. Launch from the
intended focused managed terminal; titles are not routing inputs. Live macOS
focus and permissions require validation on the consuming desktop; portable
tests mock OmniWM's external commands and responses.
[system](verify:notifications.focus-target-registration)
[system](verify:notifications.attention-navigation)

Suppression is host-owned; no downstream notification hooks or dotfile focus
checks are needed. Update and restart `wrix-notifyd` to use the packaged helper.
Restart sandboxes to refresh registrations that lack origin metadata.

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

Linux registration never uses desktop focus. It maps the originating stable tmux
pane ID (`TMUX_PANE`) to a single session and exactly one attached client in
that session, checks the client's PID/TTY, follows that client's process
ancestry (not the tmux server ancestry of the launching shell), and matches a
unique ancestor PID against niri's window list. Without tmux it follows the
launcher ancestry. Missing process/window information, multiple windows sharing
a terminal PID, linked panes in multiple sessions, and multi-client sessions
omit navigation routing while retaining notification delivery. Clients in other
sessions do not make an otherwise unique originating client ambiguous.
[test](command::launch::linux_terminal::tests::changing_focus_and_unrelated_application_do_not_change_origin)
[test](command::launch::linux_terminal::tests::multiple_terminal_clients_are_resolved_by_session)
[test](command::launch::linux_terminal::tests::multi_client_session_is_ambiguous)
[test](command::launch::linux_terminal::tests::linked_pane_sessions_are_ambiguous)

Automatic Linux focus keys include the tmux socket and stable pane ID, since
pane IDs are unique only within one server; consumer overrides remain opaque.
[test](command::launch::linux_terminal::tests::pane_routing_keys_are_scoped_to_the_tmux_socket)

Linux routes carry `linux_route` provenance (terminal PID and, for tmux, client
name/PID/TTY/session ID). Additional registrations preserve an already proven
route rather than overwriting it with current focus. Navigation validates the
window PID, recorded client ancestry, uniquely attached session client, socket,
stable pane ID, session membership and live pane before selecting the window and
pane and focusing niri. Renaming/reindexing does not retarget a pane. A detached
client, a client switched to another session, or a newly ambiguous session
invalidates navigation; Wrix never switches an arbitrary client.
`select-window`/`select-pane` do not switch clients between sessions.
[system](verify:notifications.attention-navigation)
[system](verify:notifications.focus-target-registration)

Old Linux registrations lacking provenance are ignored by navigation and focus
suppression, including previously queued routes. A later registration clears an
unproven window and installs a proven route only if origin can be resolved;
overlap reference counts remain intact. Existing sandboxes are not refreshed by
rebuilding: update the host launcher and focus package, restart the consuming
desktop's notification daemon (and reload its keybinding/package reference),
then restart sandboxes from their intended host terminals to recreate routes.
Old index-keyed registrations can remain until their launcher exits, but cannot
navigate under the updated helper. No queue/database migration or destructive
session-directory cleanup is required.
[system](verify:notifications.attention-navigation)
[system](verify:notifications.focus-target-registration)

Darwin can validate a consumer-provided OmniWM opaque window ID/PID against
current managed windows, select the proven host tmux position if present, and
invoke `omniwmctl window navigate <id> --json` across workspaces. Focus is
confirmed through the focused-window query after the layout transition.
IPC-session restarts invalidate IDs; closed windows or PID mismatches do not
retarget an application. There is no System Events/title fallback. Wrix does not
package, pin or install OmniWM; provision and permissions belong to the
consumer.
[test](darwin::opaque_window_navigation_and_return_ignore_titles_and_restore_origin)
[test](darwin::closed_reused_or_previous_ipc_session_id_is_discarded_without_focus)

Darwin tmux routes use the explicit socket, stable pane ID and proven attached
client. Launch checks the focused terminal's caller/client ancestry; linked
panes, multi-client sessions and ambiguous shared terminal PIDs omit routing.
Overlapping registrations preserve proven identities. Unavailable IPC or origin
metadata leaves notification delivery intact. Update all host packages, restart
the daemon and relaunch sandboxes/native wrappers to replace legacy routes.
[test](darwin::tmux_launch_captures_stable_pane_and_proven_client_not_server_ancestry)
[test](command::launch::tests::darwin_identity_persists_and_is_immutable_across_overlapping_launches)

Darwin launch, daemon lookup/enqueue, and shortcut invocation share
`$HOME/.local/share/wrix/sessions`, independent of shell-only XDG settings.
Warnings/errors, stale targets and empty queues are diagnosed without debug
logging. Retryable navigation/return failures retain state and exit nonzero; the
packaged Darwin helper also attempts a native failure alert.
[test](gui_and_shell_use_the_same_darwin_session_directory)
[test](darwin::failed_or_malformed_queries_retain_attention_and_saved_origin)

[OmniWM setup, permissions, compatibility and live validation](../docs/darwin-attention.md)
describe the required same-user IPC access, bundled CLI discovery and optional
explicit path/socket overrides. OmniWM owns Accessibility/Input Monitoring; Wrix
does not use System Events Automation. Portable tests are mocked desktop
validation, not evidence of live macOS workspace switching or TCC behavior.

Navigation happens only on explicit host invocation, never on delivery. Unrouted
notifications still dispatch normally. Queue storage failures are diagnosed
without failing notification delivery. Native dispatch failures do not enqueue
attention and do not stop later notifications.
[system](verify:notifications.attention-navigation)
[system](verify:notifications.focus-target-registration)

### Return to the previous position

With Linux/niri or Darwin/OmniWM, `wrix-focus-notification --return` restores
the position before the last successful attention jump. Desktop bindings remain
consumer-owned: invoke the ordinary command for attention and invoke it with
`--return` for return. The host remembers the originating desktop window ID and
PID; when tmux is present it also records the explicitly resolved client
name/PID/TTY, socket, stable session/window IDs and stable pane ID. Capture
resolves host-owned tmux listeners and client ancestry using the same route
validation as navigation (Linux procfs; Darwin lsof/ps); the originating pane
need not itself have an agent registration.
[system](verify:notifications.attention-navigation)

Every successful jump replaces the single saved origin, including moves within
the same workspace, terminal window, tmux window, or pane. In particular, a
cross-workspace jump followed by a pane-only jump returns to the pane before the
latter jump, not to the earlier workspace. Empty queues and unsuccessful
navigation leave the saved position unchanged. If a successful jump cannot
resolve an unambiguous origin, it clears the old return position rather than
leaving a misleading destination.
[system](verify:notifications.attention-navigation)

Return validates the saved window/PID, client identity and ancestry, session,
window and live pane, then restores only that explicitly identified client to
the saved session, selects the saved window/pane and focuses the saved desktop
window. It does not switch any other client. Ambiguous shared session state,
closed windows, exited/replaced clients, missing sessions/windows, and
dead/missing/moved panes are discarded safely. Successful returns consume the
saved position; transient navigation failures retain it for retry. Return
neither consumes attention entries nor saves a new origin.
[system](verify:notifications.attention-navigation)

Attention navigation, return, and saved-position updates serialize under the
pending queue's SQLite transaction. The return table is added automatically to
the existing host-local database; there is no manual database migration.
Darwin's legacy registrations need relaunching as described above. Update the
consuming desktop's focus package and reload its command references to enable
return. Delivery still never captures or changes desktop focus. Darwin's return
uses the same queue/return transaction and origin replacement/consumption
semantics as Linux. [system](verify:notifications.attention-navigation)
[test](darwin::latest_success_replaces_origin_even_within_the_same_workspace)
[test](darwin::tmux_return_restores_only_the_identified_client_session_window_and_pane)
[test](darwin::return_capture_discovers_unregistered_custom_tmux_server_with_lsof)
[test](darwin::stale_return_does_not_consume_attention_or_save_another_origin)

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
  agree on the opaque host target and suppress only an active origin pane in its
  focused terminal window; inactive panes, background windows (including another
  window of the same macOS app), and unavailable origin/focus information still
  notify, without substituting an execution ID, agent conversation ID, or
  in-container debugging pane ID
  [system](verify:notifications.focus-target-registration)
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
