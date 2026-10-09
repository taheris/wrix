# Native tmux workflow

Use your shell tool inside a running sandbox. Upstream tmux is already
installed; no MCP server is needed. This guide is also available at
`/etc/wrix/tmux.md`.

## Create and inspect

Choose a private socket, not the unrelated default tmux server. Keep the socket
path and the returned pane ID for later shell calls (or rediscover them with
`list-panes`). Use a fresh session name for each workflow.

```bash
set -euo pipefail
workflow_dir=$(mktemp -d /tmp/wrix-tmux.XXXXXX)
socket="$workflow_dir/socket"
pane=$(tmux -S "$socket" new-session -d -s debug -P -F '#{pane_id}' 'bash --noprofile --norc')
tmux -S "$socket" list-sessions
tmux -S "$socket" list-panes -a -F '#{session_name} #{pane_id} dead=#{pane_dead} exit=#{pane_dead_status}'
```

Configure retention **before** starting any workload, including fast failures.
Replacing the initial shell after setting the window option avoids the race
where a command exits before retention is enabled.

```bash
tmux -S "$socket" set-option -w -t "$pane" remain-on-exit on
tmux -S "$socket" respawn-pane -k -t "$pane" 'python3 -u -m http.server 8765 --bind 127.0.0.1'
```

The detached server survives completion of this shell call. In another call,
reuse the socket and pane; make a bounded request from a separate process:

```bash
curl --fail --max-time 2 http://127.0.0.1:8765/
tmux -S "$socket" capture-pane -p -t "$pane" -S -1000
tmux -S "$socket" display-message -p -t "$pane" 'dead=#{pane_dead} exit=#{pane_dead_status}'
```

`dead=0` means running; `dead=1` means exited and `exit` gives its exit code.
For example, a fast failure remains inspectable after this command:

```bash
tmux -S "$socket" respawn-pane -k -t "$pane" "bash -c 'printf \"fast failure\\n\"; exit 7'"
```

Repeat capture/status in later calls, with a bounded wait if the process is
still running. Scrollback is finite and can be overwritten; it is not a durable
application log. Direct application logging to caller-managed files when you
need durable output, and manage those files' access, retention, and deletion.

## Literal text and special keys

To use an interactive shell, respawn it and wait for its prompt. `-l` sends
literal text (including words such as `Enter` and `C-c`); send special keys in a
separate command without `-l`:

```bash
tmux -S "$socket" respawn-pane -k -t "$pane" 'bash --noprofile --norc'
tmux -S "$socket" send-keys -t "$pane" -l -- "printf 'literal Enter C-c\\n'"
tmux -S "$socket" send-keys -t "$pane" Enter
tmux -S "$socket" send-keys -t "$pane" C-c
```

## Targeted cleanup

The caller owns the session lifecycle. Shell-tool or agent-conversation
completion does not destroy sessions. Remove only your selected session:

```bash
tmux -S "$socket" kill-session -t '=debug'
```

If no other sessions use **your private socket**, remove its directory with
`rm -rf "$workflow_dir"`. Do not use an unqualified `kill-server` against an
unrelated default server. Stopping the sandbox terminates its remaining tmux
processes; unrelated sandboxes are unaffected. No cross-container debugging,
persistence across container replacement, or automatic recovery is provided.
