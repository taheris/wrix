use std::{
    collections::BTreeSet,
    fs,
    num::NonZeroU32,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
};

use serde::Deserialize;
use serde_json::Value;

use super::{Navigator, Outcome};
use crate::{
    process::{Error, Runner},
    route::{Client, OmniId, OmniWindow, PaneId, Platform, Route, Socket, Tmux},
};

#[derive(Deserialize)]
struct Window {
    id: OmniId,
    pid: NonZeroU32,
}

impl Window {
    fn origin(&self, client: Option<Client>) -> OmniWindow {
        OmniWindow {
            window: self.id.clone(),
            terminal_pid: self.pid,
            client,
        }
    }
}

#[derive(Clone, Copy)]
enum Request<'a> {
    Windows,
    Focused,
    Navigate(&'a OmniId),
}

impl Request<'_> {
    const fn kind(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Focused => "focused-window",
            Self::Navigate(_) => "navigate",
        }
    }

    fn args(&self) -> Vec<&str> {
        match self {
            Self::Windows => vec!["query", "windows", "--fields", "id,pid", "--json"],
            Self::Focused => vec!["query", "focused-window", "--json"],
            Self::Navigate(id) => vec!["window", "navigate", id.as_str(), "--json"],
        }
    }
}

impl<R: Runner> Navigator<R> {
    fn omni(&self, request: Request<'_>) -> Result<Value, Outcome> {
        let kind = request.kind();
        let (text, successful) = match self.runner.run("omniwmctl", &request.args()) {
            Ok(text) => (text, true),
            Err(Error::Status(_, text)) if !text.is_empty() => (text, false),
            Err(error) => {
                tracing::warn!(%error, operation = kind,
                    "OmniWM unavailable: enable IPC, use its bundled CLI (WRIX_OMNIWMCTL), and check same-user socket access");
                return Err(Outcome::Retry);
            }
        };
        let value: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(%error, operation = kind, response = text,
                    "invalid OmniWM JSON; use the CLI bundled with the running app");
                return Err(Outcome::Retry);
            }
        };
        if successful
            && value["ok"] == true
            && value["status"] == "success"
            && value["result"]["kind"] == kind
        {
            return Ok(value["result"]["payload"].clone());
        }
        if successful
            && matches!(request, Request::Navigate(_))
            && value["ok"] == true
            && value["status"] == "executed"
            && value["kind"] == "window"
        {
            return Ok(Value::Null);
        }
        let code = value["code"].as_str().unwrap_or("unexpected_response");
        tracing::warn!(
            code,
            operation = kind,
            "OmniWM rejected navigation/query; check Enable IPC, app/CLI versions, Accessibility permission and disabled/Overview state"
        );
        Err(if matches!(code, "stale_window_id" | "not_found") {
            Outcome::Stale
        } else {
            Outcome::Retry
        })
    }

    fn omni_windows(&self) -> Option<Vec<Window>> {
        let Ok(value) = self.omni(Request::Windows) else {
            return None;
        };
        match serde_json::from_value(value["windows"].clone()) {
            Ok(windows) => Some(windows),
            Err(error) => {
                tracing::warn!(%error, "OmniWM windows lack stable IDs/PIDs; update the app and bundled CLI");
                None
            }
        }
    }

    fn omni_focused(&self) -> Option<Window> {
        let Ok(value) = self.omni(Request::Focused) else {
            return None;
        };
        if value["window"].is_null() {
            return None;
        }
        match serde_json::from_value(value["window"].clone()) {
            Ok(window) => Some(window),
            Err(error) => {
                tracing::warn!(%error, "OmniWM focused window lacks a stable ID/PID");
                None
            }
        }
    }

    pub(super) fn validate_omni(&self, origin: &OmniWindow) -> Outcome {
        let Some(windows) = self.omni_windows() else {
            return Outcome::Retry;
        };
        if windows
            .iter()
            .filter(|window| window.id == origin.window && window.pid == origin.terminal_pid)
            .count()
            == 1
        {
            Outcome::Success
        } else {
            tracing::warn!(
                window = origin.window.as_str(),
                "stale OmniWM window; restart the sandbox after closing its window or restarting OmniWM"
            );
            Outcome::Stale
        }
    }

    pub(super) fn focus_omni(&self, origin: &OmniWindow) -> Outcome {
        if let Err(outcome) = self.omni(Request::Navigate(&origin.window)) {
            return outcome;
        }
        // OmniWM commits keyboard focus after its workspace layout transition.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            if self.focused_omni(origin) {
                return Outcome::Success;
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        tracing::warn!(
            window = origin.window.as_str(),
            "OmniWM accepted navigation but focus was not confirmed; retry after the workspace transition"
        );
        Outcome::Retry
    }

    pub(super) fn focused_omni(&self, origin: &OmniWindow) -> bool {
        self.omni_focused()
            .is_some_and(|window| window.id == origin.window && window.pid == origin.terminal_pid)
    }

    pub(super) fn darwin_ancestors(&self, mut pid: u32) -> Option<std::collections::HashSet<u32>> {
        let mut ancestors = std::collections::HashSet::new();
        while pid > 1 {
            if !ancestors.insert(pid) || ancestors.len() > 128 {
                return None;
            }
            let parent = self.run("ps", &["-p", &pid.to_string(), "-o", "ppid="])?;
            pid = match parent.trim().parse() {
                Ok(parent) => parent,
                Err(error) => {
                    tracing::warn!(%error, pid, "invalid process ancestry response");
                    return None;
                }
            };
        }
        Some(ancestors)
    }

    pub(super) fn darwin_client_matches(&self, client: &Client, terminal: NonZeroU32) -> bool {
        let Some(expected) = client.tty.as_str().strip_prefix("/dev/") else {
            return false;
        };
        let Some(tty) = self.run("ps", &["-p", &client.pid.to_string(), "-o", "tty="]) else {
            return false;
        };
        tty.trim() == expected
            && self
                .darwin_ancestors(client.pid.get())
                .is_some_and(|ancestors| ancestors.contains(&terminal.get()))
    }

    /// Capture only the focused managed launch window with proven caller/client ancestry.
    pub fn capture_omni_launch(
        &self,
        pane: Option<&PaneId>,
        socket: Option<&Socket>,
        launcher: u32,
    ) -> Option<Route> {
        let window = self.omni_focused()?;
        let tmux = if let Some(pane) = pane {
            let socket = socket?;
            let sessions = self.tmux(
                Some(socket),
                &["list-panes", "-a", "-F", "#{pane_id}|#{session_id}"],
            )?;
            let sessions: Vec<_> = sessions
                .lines()
                .filter_map(|line| line.split_once('|'))
                .filter(|(id, _)| *id == pane.as_str())
                .map(|(_, session)| session)
                .collect();
            let [session] = sessions.as_slice() else {
                return None;
            };
            let clients = self.clients(socket)?;
            let clients: Vec<_> = clients
                .iter()
                .filter(|client| client.identity.session.as_str() == *session)
                .collect();
            let [client] = clients.as_slice() else {
                return None;
            };
            if self
                .omni_windows()?
                .iter()
                .filter(|other| other.pid == window.pid)
                .count()
                != 1
            {
                return None;
            }
            Some(Tmux {
                socket: socket.clone(),
                pane: pane.clone(),
                client: client.identity.clone(),
                window: None,
            })
        } else {
            if socket.is_some() || !self.darwin_ancestors(launcher)?.contains(&window.pid.get()) {
                return None;
            }
            None
        };
        let route = Route::Darwin {
            origin: window.origin(tmux.as_ref().map(|t| t.client.clone())),
            tmux,
        };
        (self.validate(&route, false) == Outcome::Success).then_some(route)
    }

    fn omni_sockets(&self, directory: &Path) -> Option<Vec<Socket>> {
        let mut sockets = BTreeSet::new();
        let uid = match fs::metadata(directory) {
            Ok(metadata) => metadata.uid(),
            Err(error) => {
                tracing::warn!(%error, "return socket ownership unavailable");
                return None;
            }
        };
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!(%error, path = %directory.display(), "return socket discovery unavailable");
                return None;
            }
        };
        for entry in entries {
            let path = match entry {
                Ok(entry) => entry.path(),
                Err(error) => {
                    tracing::warn!(%error, "return socket discovery unavailable");
                    return None;
                }
            };
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            match fs::read_to_string(&path).and_then(|text| {
                serde_json::from_str::<Value>(&text).map_err(std::io::Error::other)
            }) {
                Ok(value) => {
                    if let Some(socket) = value["tmux_socket"].as_str() {
                        sockets.insert(socket.to_owned());
                    }
                }
                Err(error) => {
                    tracing::debug!(%error, "obsolete registration is not socket discovery evidence");
                }
            }
        }
        let output = match self
            .runner
            .run("lsof", &["-n", "-P", "-a", "-U", "-c", "tmux", "-Fn"])
        {
            Ok(output) => output,
            Err(Error::Status(status, message))
                if status.code() == Some(1) && message.trim().is_empty() =>
            {
                String::new()
            }
            Err(error) => {
                tracing::warn!(%error, "cannot discover host tmux sockets; return position will not be saved");
                return None;
            }
        };
        for line in output.lines() {
            if let Some(path) = line.strip_prefix('n').filter(|path| path.starts_with('/')) {
                sockets.insert(path.to_owned());
            }
        }
        sockets
            .into_iter()
            .filter(|socket| {
                Path::new(socket)
                    .metadata()
                    .is_ok_and(|m| m.file_type().is_socket() && m.uid() == uid)
            })
            .map(|socket| match Socket::try_from(socket) {
                Ok(socket) => Some(socket),
                Err(error) => {
                    tracing::warn!(%error, "invalid discovered tmux socket");
                    None
                }
            })
            .collect()
    }

    pub(super) fn capture_omni_origin(&self, directory: &Path) -> Option<Route> {
        let window = self.omni_focused()?;
        let mut matches = Vec::new();
        for socket in self.omni_sockets(directory)? {
            for client in self.clients(&socket)? {
                if self
                    .darwin_ancestors(client.identity.pid.get())?
                    .contains(&window.pid.get())
                {
                    matches.push((socket.clone(), client));
                }
            }
        }
        let tmux = match matches.as_slice() {
            [] => None,
            [(socket, client)] => {
                if self
                    .omni_windows()?
                    .iter()
                    .filter(|other| other.pid == window.pid)
                    .count()
                    != 1
                {
                    return None;
                }
                Some(Tmux {
                    socket: socket.clone(),
                    pane: client.pane.clone(),
                    client: client.identity.clone(),
                    window: Some(client.window.clone()),
                })
            }
            _ => return None,
        };
        let route = Route::Darwin {
            origin: window.origin(tmux.as_ref().map(|t| t.client.clone())),
            tmux,
        };
        (self.validate(&route, true) == Outcome::Success).then_some(route)
    }

    pub fn capture_position(&self, directory: &Path, platform: Platform) -> Option<Route> {
        match platform {
            Platform::Linux => self.capture_origin(directory),
            Platform::Darwin => self.capture_omni_origin(directory),
        }
    }
}
