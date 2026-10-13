//! Resolve the launching terminal without observing desktop focus.
use std::{fs, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{FocusTarget, run_output, tmux_socket_from_env, trim_stdout};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Route {
    pub terminal_pid: u32,
    pub client: Option<Client>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Client {
    pub name: String,
    pub pid: u32,
    pub tty: String,
    pub session: String,
}

pub(super) fn focus_key(pane: &FocusTarget, socket: &Path) -> Option<FocusTarget> {
    // Pane IDs are stable only within one tmux server.
    FocusTarget::parse(&format!("tmux:{}:{}", socket.to_str()?, pane.as_str())).ok()
}

fn output(args: &[&str]) -> Option<String> {
    let result = run_output("tmux", args).ok()?;
    result.status.success().then(|| trim_stdout(&result.stdout))
}

pub(super) fn discover(target: Option<&FocusTarget>) -> Option<(String, Route)> {
    let client = if let Some(target) = target {
        let socket = tmux_socket_from_env()?;
        let socket = socket.to_str()?;
        let panes = output(&[
            "-S",
            socket,
            "list-panes",
            "-a",
            "-F",
            "#{pane_id}\t#{session_id}",
        ])?;
        // A pane in a linked window may belong to several sessions. Its ID
        // alone cannot establish which client/session originated this launch.
        let session = unique_pane_session(&panes, target.as_str())?;
        let clients = output(&[
            "-S",
            socket,
            "list-clients",
            "-F",
            "#{client_name}\t#{client_pid}\t#{client_tty}\t#{session_id}",
        ])?;
        let client = unique_client(&clients, &session)?;
        // The tmux server is an ancestor of pane shells, not the terminal.
        // Start from the explicitly resolved attached client instead.
        if fs::read_link(format!("/proc/{}/fd/0", client.pid)).ok()? != Path::new(&client.tty) {
            return None;
        }
        Some(client)
    } else {
        // An unresolved tmux context must never fall back to server ancestry.
        if std::env::var_os("TMUX").is_some() {
            return None;
        }
        None
    };
    let pid = client
        .as_ref()
        .map_or_else(std::process::id, |client| client.pid);
    let windows = run_output("niri", &["msg", "-j", "windows"]).ok()?;
    if !windows.status.success() {
        return None;
    }
    let windows: Value = serde_json::from_slice(&windows.stdout).ok()?;
    let (window, terminal_pid) = match_window(pid, &windows, parent_pid)?;
    Some((
        window,
        Route {
            terminal_pid,
            client,
        },
    ))
}

fn unique_pane_session(text: &str, target: &str) -> Option<String> {
    let mut sessions = text.lines().filter_map(|line| {
        let (pane, session) = line.split_once('\t')?;
        (pane == target).then_some(session)
    });
    let session = sessions.next()?;
    if sessions.next().is_some() {
        return None;
    }
    Some(session.to_owned())
}

fn unique_client(text: &str, session: &str) -> Option<Client> {
    let mut matches = Vec::new();
    for line in text.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 4 {
            return None;
        }
        if fields[3] == session {
            matches.push(Client {
                name: fields[0].to_owned(),
                pid: fields[1].parse().ok()?,
                tty: fields[2].to_owned(),
                session: fields[3].to_owned(),
            });
        }
    }
    if matches.len() != 1 {
        return None;
    }
    matches.pop()
}

fn parent_pid(pid: u32) -> Option<u32> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:")?.trim().parse().ok())
}

fn match_window(
    mut pid: u32,
    windows: &Value,
    mut parent: impl FnMut(u32) -> Option<u32>,
) -> Option<(String, u32)> {
    let windows = windows.as_array()?;
    let mut ancestors = Vec::new();
    while pid > 1 {
        if ancestors.contains(&pid) || ancestors.len() >= 128 {
            return None;
        }
        ancestors.push(pid);
        pid = parent(pid)?;
    }
    let mut matches = windows.iter().filter_map(|window| {
        let pid = u32::try_from(window.get("pid")?.as_u64()?).ok()?;
        if !ancestors.contains(&pid) {
            return None;
        }
        Some((window.get("id")?.as_u64()?.to_string(), pid))
    });
    let result = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_focus_and_unrelated_application_do_not_change_origin() {
        for focused in [259, 270, 276] {
            let windows = serde_json::json!([
                {"id":270,"pid":10,"is_focused":focused == 270},
                {"id":259,"pid":20,"is_focused":focused == 259},
                {"id":276,"pid":30,"is_focused":focused == 276}
            ]);
            assert_eq!(
                match_window(12, &windows, |pid| Some(if pid == 12 { 10 } else { 1 })),
                Some(("270".into(), 10))
            );
        }
    }

    #[test]
    fn multiple_terminal_clients_are_resolved_by_session() {
        let clients = "/dev/pts/6\t12\t/dev/pts/6\t$30\n/dev/pts/7\t22\t/dev/pts/7\t$27";
        assert_eq!(unique_client(clients, "$30").unwrap().pid, 12);
        assert_eq!(unique_client(clients, "$27").unwrap().pid, 22);
    }

    #[test]
    fn pane_routing_keys_are_scoped_to_the_tmux_socket() {
        let pane = FocusTarget::parse_tmux("%1").unwrap();
        assert_ne!(
            focus_key(&pane, Path::new("/tmp/one")).unwrap().as_str(),
            focus_key(&pane, Path::new("/tmp/two")).unwrap().as_str()
        );
    }

    #[test]
    fn linked_pane_sessions_are_ambiguous() {
        assert!(unique_pane_session("%1\t$30\n%1\t$27", "%1").is_none());
        assert_eq!(
            unique_pane_session("%1\t$30\n%2\t$27", "%1"),
            Some("$30".into())
        );
    }

    #[test]
    fn multi_client_session_is_ambiguous() {
        assert!(unique_client("a\t12\t/dev/pts/6\t$30\nb\t22\t/dev/pts/7\t$30", "$30").is_none());
    }

    #[test]
    fn shared_terminal_pid_and_missing_ancestry_are_not_routes() {
        let windows = serde_json::json!([{"id":1,"pid":10},{"id":2,"pid":10}]);
        assert!(match_window(12, &windows, |pid| Some(if pid == 12 { 10 } else { 1 })).is_none());
        assert!(match_window(12, &windows, |_| None).is_none());
    }
}
