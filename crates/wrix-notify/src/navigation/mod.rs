use std::{
    collections::{BTreeSet, HashSet},
    fs,
    num::NonZeroU32,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
};

use serde::Deserialize;
use serde_json::Value;

use crate::{
    process::Runner,
    route::{
        Client, ClientName, PaneId, Platform, Route, SessionId, Socket, Tmux, TmuxWindow, Tty,
        WindowId,
    },
};

/// Printable framing survives tmux's control-character sanitization.
pub const CLIENT_FORMAT: &str =
    "#{client_name}|#{client_pid}|#{client_tty}|#{session_id}|#{window_id}|#{pane_id}";

mod omniwm;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Success,
    Stale,
    Retry,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Attached {
    pub identity: Client,
    pub window: TmuxWindow,
    pub pane: PaneId,
}

impl Attached {
    fn parse(line: &str) -> Result<Self, crate::route::Error> {
        let fields: Vec<_> = line.split('|').collect();
        let [name, pid, tty, session, window, pane] = fields.as_slice() else {
            return Err(crate::route::Error::Invalid("tmux client fields"));
        };
        Ok(Self {
            identity: Client {
                name: ClientName::try_from((*name).to_owned())?,
                pid: pid
                    .parse()
                    .map_err(|_| crate::route::Error::Invalid("tmux client PID"))?,
                tty: Tty::try_from((*tty).to_owned())?,
                session: SessionId::try_from((*session).to_owned())?,
            },
            window: TmuxWindow::try_from((*window).to_owned())?,
            pane: PaneId::try_from((*pane).to_owned())?,
        })
    }
}

#[derive(Deserialize)]
struct DesktopWindow {
    id: Value,
    pid: Option<NonZeroU32>,
    #[serde(default)]
    is_focused: bool,
}

impl DesktopWindow {
    fn id(&self) -> Option<WindowId> {
        let text = match &self.id {
            Value::String(text) => text.clone(),
            Value::Number(number) => number.to_string(),
            _ => return None,
        };
        match text.try_into() {
            Ok(id) => Some(id),
            Err(error) => {
                tracing::debug!(%error, "invalid desktop window identity");
                None
            }
        }
    }
}

pub struct Navigator<R> {
    runner: R,
    proc_root: PathBuf,
}

impl<R: Runner> Navigator<R> {
    pub const fn new(runner: R, proc_root: PathBuf) -> Self {
        Self { runner, proc_root }
    }

    fn run(&self, program: &str, args: &[&str]) -> Option<String> {
        match self.runner.run(program, args) {
            Ok(text) => Some(text),
            Err(error) => {
                tracing::warn!(%error, program, "navigation unavailable");
                None
            }
        }
    }

    fn tmux(&self, socket: Option<&Socket>, args: &[&str]) -> Option<String> {
        let mut arguments = Vec::new();
        if let Some(socket) = socket {
            arguments.extend(["-S", socket.as_str()]);
        }
        arguments.extend_from_slice(args);
        self.run("tmux", &arguments)
    }

    pub fn clients(&self, socket: &Socket) -> Option<Vec<Attached>> {
        let text = self.tmux(Some(socket), &["list-clients", "-F", CLIENT_FORMAT])?;
        match text.lines().map(Attached::parse).collect() {
            Ok(clients) => Some(clients),
            Err(error) => {
                tracing::warn!(%error, socket = socket.as_str(), "invalid tmux client response");
                None
            }
        }
    }

    fn windows(&self) -> Option<Vec<DesktopWindow>> {
        let text = self.run("niri", &["msg", "-j", "windows"])?;
        match serde_json::from_str(&text) {
            Ok(windows) => Some(windows),
            Err(error) => {
                tracing::warn!(%error, "invalid desktop window response");
                None
            }
        }
    }

    pub fn ancestors(&self, mut pid: u32) -> Option<HashSet<u32>> {
        let mut ancestors = HashSet::new();
        while pid > 1 {
            if !ancestors.insert(pid) || ancestors.len() > 128 {
                return None;
            }
            let path = self.proc_root.join(pid.to_string()).join("status");
            let status = match fs::read_to_string(&path) {
                Ok(status) => status,
                Err(error) => {
                    tracing::debug!(%error, %pid, "process ancestry unavailable");
                    return None;
                }
            };
            let Some(parent) = status.lines().find_map(|line| line.strip_prefix("PPid:")) else {
                tracing::debug!(%pid, "missing process parent");
                return None;
            };
            pid = match parent.trim().parse() {
                Ok(parent) => parent,
                Err(error) => {
                    tracing::debug!(%error, %pid, "invalid process parent");
                    return None;
                }
            };
        }
        Some(ancestors)
    }

    pub fn client_origin_matches(&self, client: &Client, terminal: NonZeroU32) -> bool {
        let path = self.proc_root.join(client.pid.to_string()).join("fd/0");
        match fs::read_link(path) {
            Ok(tty) if tty == Path::new(client.tty.as_str()) => {}
            Ok(_) => return false,
            Err(error) => {
                tracing::debug!(%error, pid = %client.pid, "client TTY unavailable");
                return false;
            }
        }
        self.ancestors(client.pid.get())
            .is_some_and(|ancestors| ancestors.contains(&terminal.get()))
    }

    fn validate_tmux(
        &self,
        tmux: &Tmux,
        terminal: NonZeroU32,
        returning: bool,
        platform: Platform,
    ) -> Outcome {
        let Some(clients) = self.clients(&tmux.socket) else {
            return Outcome::Retry;
        };
        let identified: Vec<_> = clients
            .iter()
            .filter(|other| {
                other.identity.name == tmux.client.name
                    && other.identity.pid == tmux.client.pid
                    && other.identity.tty == tmux.client.tty
            })
            .collect();
        let [identified] = identified.as_slice() else {
            return Outcome::Stale;
        };
        let attached: Vec<_> = clients
            .iter()
            .filter(|other| other.identity.session == tmux.client.session)
            .collect();
        if if returning {
            attached
                .iter()
                .any(|other| !std::ptr::eq(*other, *identified))
        } else {
            attached.len() != 1
                || !std::ptr::eq(attached[0], *identified)
                || identified.identity.session != tmux.client.session
        } {
            return Outcome::Stale;
        }
        if !(match platform {
            Platform::Linux => self.client_origin_matches(&tmux.client, terminal),
            Platform::Darwin => self.darwin_client_matches(&tmux.client, terminal),
        }) {
            return Outcome::Stale;
        }
        let Some(panes) = self.tmux(
            Some(&tmux.socket),
            &["list-panes", "-a", "-F", "#{pane_id}|#{session_id}"],
        ) else {
            return Outcome::Retry;
        };
        let sessions: Vec<_> = panes
            .lines()
            .filter_map(|line| {
                let (pane, session) = line.split_once('|')?;
                (pane == tmux.pane.as_str()).then_some(session)
            })
            .collect();
        if sessions != [tmux.client.session.as_str()] {
            return Outcome::Stale;
        }
        let mut format = "#{pane_id} #{pane_dead} #{session_id}".to_owned();
        let mut expected = format!("{} 0 {}", tmux.pane.as_str(), tmux.client.session.as_str());
        if returning {
            let Some(window) = &tmux.window else {
                return Outcome::Stale;
            };
            format.push_str(" #{window_id}");
            expected.push(' ');
            expected.push_str(window.as_str());
        }
        let state = self.tmux(
            Some(&tmux.socket),
            &["display-message", "-p", "-t", tmux.pane.as_str(), &format],
        );
        let Some(state) = state else {
            return Outcome::Retry;
        };
        if state == expected {
            Outcome::Success
        } else {
            Outcome::Stale
        }
    }

    pub fn validate(&self, route: &Route, returning: bool) -> Outcome {
        match route {
            Route::Linux {
                window,
                terminal,
                tmux,
            } => {
                let Some(windows) = self.windows() else {
                    return Outcome::Retry;
                };
                if !windows.iter().any(|other| {
                    other.id().as_ref() == Some(window) && other.pid == Some(*terminal)
                }) {
                    return Outcome::Stale;
                }
                tmux.as_ref().map_or(Outcome::Success, |tmux| {
                    self.validate_tmux(tmux, *terminal, returning, Platform::Linux)
                })
            }
            Route::Darwin { origin, tmux } => {
                let outcome = self.validate_omni(origin);
                if outcome != Outcome::Success {
                    return outcome;
                }
                tmux.as_ref().map_or(Outcome::Success, |tmux| {
                    self.validate_tmux(tmux, origin.terminal_pid, returning, Platform::Darwin)
                })
            }
        }
    }

    pub fn navigate(&self, route: &Route, returning: bool) -> Outcome {
        let result = self.validate(route, returning);
        if result != Outcome::Success {
            return result;
        }
        let tmux = match route {
            Route::Linux { tmux, .. } | Route::Darwin { tmux, .. } => tmux.as_ref(),
        };
        if let Some(tmux) = tmux.filter(|_| returning)
            && self
                .tmux(
                    Some(&tmux.socket),
                    &[
                        "switch-client",
                        "-c",
                        tmux.client.name.as_str(),
                        "-t",
                        tmux.client.session.as_str(),
                    ],
                )
                .is_none()
        {
            return Outcome::Retry;
        }
        let socket = tmux.map(|t| &t.socket);
        let pane = tmux.map(|t| t.pane.as_str());
        if let Some(pane) = pane {
            for command in ["select-window", "select-pane"] {
                if self.tmux(socket, &[command, "-t", pane]).is_none() {
                    return Outcome::Retry;
                }
            }
        }
        if let Route::Linux { window, .. } = route
            && self
                .run(
                    "niri",
                    &["msg", "action", "focus-window", "--id", window.as_str()],
                )
                .is_none()
        {
            return Outcome::Retry;
        }
        if let Route::Darwin { origin, .. } = route {
            return self.focus_omni(origin);
        }
        Outcome::Success
    }

    pub fn is_focused(&self, route: &Route) -> bool {
        if self.validate(route, false) != Outcome::Success {
            return false;
        }
        let (focused, socket, pane) = match route {
            Route::Darwin { origin, tmux } => (
                self.focused_omni(origin),
                tmux.as_ref().map(|t| &t.socket),
                tmux.as_ref().map(|t| t.pane.as_str()),
            ),
            Route::Linux {
                window,
                terminal,
                tmux,
            } => {
                let Some(windows) = self.windows() else {
                    return false;
                };
                let focused: Vec<_> = windows.iter().filter(|w| w.is_focused).collect();
                (
                    focused.len() == 1
                        && focused[0].id().as_ref() == Some(window)
                        && focused[0].pid == Some(*terminal),
                    tmux.as_ref().map(|t| &t.socket),
                    tmux.as_ref().map(|t| t.pane.as_str()),
                )
            }
        };
        focused
            && pane.is_none_or(|pane| {
                socket.is_some()
                    && self
                        .tmux(
                            socket,
                            &[
                                "display-message",
                                "-p",
                                "-t",
                                pane,
                                "#{window_active} #{pane_active} #{?session_attached,1,0}",
                            ],
                        )
                        .as_deref()
                        == Some("1 1 1")
            })
    }

    pub fn sockets(&self, directory: &Path) -> Option<Vec<Socket>> {
        let mut sockets = BTreeSet::new();
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!(%error, path = %directory.display(), "registrations unavailable");
                return None;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    tracing::debug!(%error, "obsolete registration");
                    continue;
                }
            };
            if entry.path().extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            match fs::read_to_string(entry.path()).and_then(|text| {
                serde_json::from_str::<Value>(&text).map_err(std::io::Error::other)
            }) {
                Ok(value) => {
                    if let Some(socket) = value.get("tmux_socket").and_then(Value::as_str)
                        && Path::new(socket)
                            .metadata()
                            .is_ok_and(|meta| meta.file_type().is_socket())
                    {
                        sockets.insert(socket.to_owned());
                    }
                }
                Err(error) => {
                    tracing::debug!(%error, "obsolete registration is not discovery evidence");
                }
            }
        }
        let unix = match fs::read_to_string(self.proc_root.join("net/unix")) {
            Ok(unix) => unix,
            Err(error) => {
                tracing::debug!(%error, "socket discovery unavailable");
                return None;
            }
        };
        let processes = match fs::read_dir(&self.proc_root) {
            Ok(processes) => processes,
            Err(error) => {
                tracing::debug!(%error, "process discovery unavailable");
                return None;
            }
        };
        let uid = match fs::metadata(self.proc_root.join("self")) {
            Ok(meta) => meta.uid(),
            Err(error) => {
                tracing::debug!(%error, "host user unavailable");
                return None;
            }
        };
        let mut unix_paths = std::collections::HashMap::new();
        for line in unix.lines().skip(1) {
            let fields: Vec<_> = line.split_whitespace().take(7).collect();
            if fields.len() != 7 {
                continue;
            }
            let mut position = 0;
            for _ in 0..7 {
                let rest = &line[position..];
                position += rest.len() - rest.trim_start().len();
                position += line[position..]
                    .find(char::is_whitespace)
                    .unwrap_or(line.len() - position);
            }
            let path = line[position..].trim();
            if path.starts_with('/') {
                unix_paths.insert(fields[6], path);
            }
        }
        for entry in processes {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    tracing::debug!(%error, "process exited during discovery");
                    continue;
                }
            };
            if entry
                .file_name()
                .to_str()
                .is_none_or(|name| !name.bytes().all(|b| b.is_ascii_digit()))
            {
                continue;
            }
            match discover_process(&entry.path(), uid, &unix_paths) {
                Ok(paths) => sockets.extend(paths),
                Err(error) => {
                    tracing::debug!(%error, "process descriptors changed during discovery");
                }
            }
        }
        match sockets.into_iter().map(Socket::try_from).collect() {
            Ok(sockets) => Some(sockets),
            Err(error) => {
                tracing::debug!(%error, "invalid discovered socket");
                None
            }
        }
    }

    pub fn capture_origin(&self, directory: &Path) -> Option<Route> {
        let windows = self.windows()?;
        let focused: Vec<_> = windows.iter().filter(|w| w.is_focused).collect();
        let [window] = focused.as_slice() else {
            return None;
        };
        let terminal = window.pid?;
        let mut matches = Vec::new();
        for socket in self.sockets(directory)? {
            for client in self.clients(&socket)? {
                if self
                    .ancestors(client.identity.pid.get())?
                    .contains(&terminal.get())
                {
                    if !self.client_origin_matches(&client.identity, terminal) {
                        return None;
                    }
                    matches.push((socket.clone(), client));
                }
            }
        }
        let tmux = match matches.as_slice() {
            [] => None,
            [(socket, client)] => {
                if windows.iter().filter(|w| w.pid == Some(terminal)).count() != 1 {
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
        let returning = tmux.is_some();
        let route = Route::Linux {
            window: window.id()?,
            terminal,
            tmux,
        };
        (self.validate(&route, returning) == Outcome::Success).then_some(route)
    }
}

fn discover_process(
    path: &Path,
    uid: u32,
    unix: &std::collections::HashMap<&str, &str>,
) -> std::io::Result<Vec<String>> {
    if path.metadata()?.uid() != uid
        || fs::read_to_string(path.join("comm"))?.trim() != "tmux: server"
    {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for fd in fs::read_dir(path.join("fd"))? {
        let link = fs::read_link(fd?.path())?;
        if let Some(inode) = link
            .to_str()
            .and_then(|s| s.strip_prefix("socket:["))
            .and_then(|s| s.strip_suffix(']'))
            && let Some(path) = unix.get(inode)
        {
            paths.push((*path).to_owned());
        }
    }
    Ok(paths)
}
