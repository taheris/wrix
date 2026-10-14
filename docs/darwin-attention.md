# Darwin attention with OmniWM

## Supported backend and evidence

Darwin navigation requires OmniWM IPC. The inspected upstream release is
[OmniWM v0.7.6](https://github.com/OmniNull/OmniWM/tree/v0.7.6). Its
[CLI reference](https://github.com/OmniNull/OmniWM/tree/v0.7.6/website/src/content/docs/reference/cli),
`IPCQueryModels.swift`, `IPCWindowRequestExecutor.swift`, and
`WindowActionHandler+Navigation.swift` establish these interfaces:

```sh
/Applications/OmniWM.app/Contents/MacOS/omniwmctl query focused-window --json
/Applications/OmniWM.app/Contents/MacOS/omniwmctl query windows --fields id,pid --json
/Applications/OmniWM.app/Contents/MacOS/omniwmctl window navigate 'ow_...' --json
```

Queries return an envelope with `ok`, `status`, and `result.kind` /
`result.payload`. The focused payload contains `window`; the windows payload
contains `windows`. Each window includes opaque `id` and process `pid`.
`window navigate`, unlike simply raising an app, follows the target across
OmniWM workspaces and monitors. Its successful response is `kind: window`,
`ok: true`, `status: executed`. Keyboard focus commits after layout; Wrix checks
the focused identity after requesting navigation, with a bounded retry window.

Use the opaque **`id`**, not `windowId` (raw CGWindowID). The opaque ID includes
an IPC-session token and is invalidated when IPC restarts, including toggling
IPC off/on. Wrix persists it with the PID, re-queries the managed windows before
each jump/return, and does not substitute a window with a matching title or app.
After restarting OmniWM/IPC, restart the relevant sandboxes/native wrappers to
register new identities. Closed/stale identities are discarded safely.

This is source/API verification, **not live macOS validation**. OmniWM is an
optional external integration: Wrix has no OmniWM package/flake/Cargo dependency
and does not pin or install it. The consumer supplies the app and bundled CLI.
Older versions are usable only if they implement these same query/action shapes;
a protocol mismatch needs the CLI bundled with the running app. There is no
System Events/title-based fallback. Without a usable OmniWM route, notification
delivery continues but navigation/suppression does not. Legacy title-only
registrations are ignored; no destructive database cleanup or migration is
needed.

## Setup and permissions

- Start OmniWM as the logged-in desktop user and enable **Enable IPC** in its
  status menu (disabled by default).
- Grant **OmniWM** Accessibility and Input Monitoring in macOS Privacy &
  Security. These are OmniWM's required startup permissions. Its documented
  prerequisites include Apple Silicon, macOS 26+, and **Displays have separate
  Spaces**. Screen Recording is optional for capture-derived visuals, not
  required by Wrix's navigation calls.
- The CLI and Wrix run as the same user. OmniWM's default socket is
  `~/Library/Application Support/com.barut.OmniWM/ipc.sock`; its adjacent
  `.secret` file is read automatically by the bundled CLI. Do not copy the
  secret into config or logs. No System Events Automation grant to Wrix,
  Karabiner, or a terminal is used by this backend.
- Wrix finds the bundled CLI in `/Applications/OmniWM.app`, then
  `~/Applications/OmniWM.app`, then PATH. An unusual installation can use
  `WRIX_OMNIWMCTL=/absolute/path/to/omniwmctl`. A custom `OMNIWM_SOCKET` or CLI
  override must be identical for launcher, daemon, and shortcut; configure GUI
  jobs explicitly rather than only in `.zshrc`.
- Launch from the intended, focused, managed terminal window. Wrix also checks
  caller ancestry; an unrelated foreground app is not accepted as the origin.
  With tmux, use stable `TMUX_PANE` and its explicit `TMUX` socket. Linked
  panes, multiple clients in the same session, and multiple windows sharing a
  terminal PID when resolving tmux are conservatively unroutable.

All Darwin components use **`$HOME/.local/share/wrix/sessions`**, ignoring
`XDG_DATA_HOME` and `XDG_RUNTIME_DIR` for session storage. Thus launchd,
Karabiner, and terminal invocations agree without shell initialization. Keep
HOME set to the same real user home. Existing registrations under a custom XDG
data root are not imported; restart the daemon and sandboxes after updating. The
host-local notification **socket** retains its existing runtime-directory
contract; it is distinct from session storage and OmniWM's IPC socket.

Return saves the position immediately before each successful jump. Host tmux
sockets are discovered from registrations and `/usr/sbin/lsof` Unix-socket
output; `/bin/ps` verifies the identified client's TTY and terminal ancestry.
The origin pane need not have an agent registration. An unresolvable/ambiguous
origin clears any older return position after a successful jump. Return only
switches the saved, still-live client, then selects the saved stable session,
window and pane; it never selects an arbitrary attached client. Missing/dead or
moved panes and shared target sessions invalidate return. Transient failures
retain attention/return for retry. Queue operations and saved-position updates
share the existing SQLite transaction semantics.

Navigation diagnostics are warnings/errors on stderr by default, including empty
queues, stale registrations, permission/IPC/version problems, and tmux command
failures. Retryable focus/return failures exit nonzero; the packaged Darwin
command additionally attempts a `terminal-notifier` alert. This alert requires
normal macOS notification authorization and is itself best-effort; stderr is the
authoritative diagnostic. Daemon enqueue failures remain best-effort and visible
in its log.

## Dotfiles integration

Retain Karabiner Option-P / Option-Shift-P. Moving the keys into OmniWM is not a
backend repair and is unnecessary with this IPC route. Invoke the same packaged
Wrix executable, with `--return` only for Option-Shift-P. The consumer's
`modules/aspects/input/_karabiner/karabiner.json` can keep these
`to[].shell_command` values:

```json
"'@homeProfile@/bin/wrix-focus-notification'"
"'@homeProfile@/bin/wrix-focus-notification' --return"
```

Its `karabiner.nix` substitutes `config.home.profileDirectory`, giving an
absolute path without an interactive shell. Alternatively generate a store path
with `${pkgs.wrix-focus-notification}/bin/wrix-focus-notification`. Keep the
Option-P reservation in OmniWM's settings; do not bind the same keys twice.

Install/update `wrix`, `wrix-notifyd`, and `wrix-focus-notification` from the
same Wrix revision, rebuild the consuming Darwin configuration, reload
Karabiner, and restart the user daemon:

```sh
launchctl kickstart -k "gui/$(id -u)/org.nix-community.home.wrix-notifyd"
```

That label is Home Manager's default for `launchd.agents.wrix-notifyd`; if the
consumer sets `Label` explicitly, use that label instead. Restart sandboxes or
native wrappers from their intended host terminals to replace old registrations.
No Alacritty title-setting change is needed. To keep shortcut stderr, append
`2>> "$HOME/Library/Logs/wrix-attention.log"` to both shell commands after
creating that directory. Leave launchd's existing stderr log configured.

## Validation

Portable Rust tests
(`cargo test -p wrix-notify --test attention --test command`) mock **external**
OmniWM, ps and lsof commands. They cover the documented JSON shapes/argv, launch
capture, navigation/return state, stale/reused IDs, tmux client/session/pane
safety, and command/transport failures. Launcher tests check persisted identity
and overlap handling. The standalone daemon/launcher tests also mock the desktop
backend; real isolated tmux tests do not imply macOS window-focus validation.

On the consuming Mac, separately validate:

1. Run the three CLI operations above with the app's own current ID. Confirm
   JSON shapes and navigation between two workspaces/monitors. Record the
   installed OmniWM version. Also run them with a minimal environment, e.g.
   `env -i HOME="$HOME" PATH=/usr/bin:/bin <absolute-command> ...`.
2. Start two Wrix sandboxes from distinct terminal windows (duplicate/dynamic
   titles are fine). Deliver attention with one in the background. Test
   Option-P, then Option-Shift-P, from another workspace. Check persisted
   `omniwm_route` in the canonical session directory and daemon logs.
3. Repeat within one terminal using separate host tmux panes/windows, then
   switch the originating tmux client to another session before returning.
   Verify only that client returns to its saved session/window/pane. Check
   custom sockets and an origin pane without an agent registration.
4. Close the saved window/pane, detach/replace a tmux client, add a second
   client in the target session, and restart OmniWM IPC. Confirm no unrelated
   target is selected and warnings explain discarded identities. Restart
   launches.
5. Disable IPC and test both shortcuts: confirm visible stderr/alert and
   retained retryable state. Re-enable/relaunch, then compare terminal versus
   Karabiner invocation. Record actual permissions if they differ; the previous
   Karabiner permission hypothesis is not an established failure cause.
