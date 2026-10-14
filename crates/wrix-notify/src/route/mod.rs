use std::{num::NonZeroU32, path::Path};

use displaydoc::Display;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

#[derive(Debug, Display, Error)]
pub enum Error {
    /// invalid attention route: {0}
    Invalid(&'static str),
    /// invalid registration JSON: {0}
    Json(#[from] serde_json::Error),
}

macro_rules! identifier {
    ($name:ident, $predicate:expr) => {
        #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl TryFrom<String> for $name {
            type Error = Error;
            fn try_from(value: String) -> Result<Self, Error> {
                if ($predicate)(&value) {
                    Ok(Self(value))
                } else {
                    Err(Error::Invalid(stringify!($name)))
                }
            }
        }
        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

const fn nonempty(value: &str) -> bool {
    !value.is_empty()
}
fn stable(value: &str, prefix: char) -> bool {
    value.starts_with(prefix)
        && value.len() > 1
        && value.as_bytes()[1..].iter().all(u8::is_ascii_digit)
}

identifier!(Target, nonempty);
identifier!(WindowId, nonempty);
identifier!(ClientName, nonempty);
identifier!(Tty, nonempty);
identifier!(Socket, nonempty);
identifier!(OmniId, |v: &str| v.starts_with("ow_")
    && v.len() > 3
    && v.as_bytes()[3..]
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')));
identifier!(PaneId, |v: &str| stable(v, '%'));
identifier!(TmuxWindow, |v: &str| stable(v, '@'));
identifier!(SessionId, |v: &str| stable(v, '$'));

impl Target {
    pub fn filename(&self) -> String {
        let mut name: String = self
            .0
            .bytes()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-' {
                    char::from(byte)
                } else {
                    '-'
                }
            })
            .collect();
        name.push_str(".json");
        name
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Client {
    pub name: ClientName,
    pub pid: NonZeroU32,
    pub tty: Tty,
    pub session: SessionId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tmux {
    pub socket: Socket,
    pub pane: PaneId,
    pub client: Client,
    pub window: Option<TmuxWindow>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OmniWindow {
    pub window: OmniId,
    pub terminal_pid: NonZeroU32,
    pub client: Option<Client>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Route {
    Linux {
        window: WindowId,
        terminal: NonZeroU32,
        tmux: Option<Tmux>,
    },
    Darwin {
        origin: OmniWindow,
        tmux: Option<Tmux>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Platform {
    Linux,
    Darwin,
}

impl Platform {
    pub const fn host() -> Self {
        if cfg!(target_os = "macos") {
            Self::Darwin
        } else {
            Self::Linux
        }
    }
}

#[derive(Deserialize)]
struct RawLinux {
    terminal_pid: NonZeroU32,
    client: Option<Client>,
}

#[derive(Deserialize)]
struct Raw {
    window_id: Option<WindowId>,
    omniwm_route: Option<OmniWindow>,
    tmux_target: Option<String>,
    tmux_socket: Option<String>,
    tmux_window: Option<TmuxWindow>,
    linux_route: Option<RawLinux>,
}

impl Route {
    pub fn parse(value: &Value, platform: Platform) -> Result<Self, Error> {
        let raw: Raw = serde_json::from_value(value.clone())?;
        match platform {
            Platform::Linux => {
                let linux = raw
                    .linux_route
                    .ok_or(Error::Invalid("missing Linux provenance"))?;
                let tmux = match raw.tmux_target.filter(|v| !v.is_empty()) {
                    Some(pane) => Some(Tmux {
                        socket: raw
                            .tmux_socket
                            .ok_or(Error::Invalid("missing tmux socket"))?
                            .try_into()?,
                        pane: pane.try_into()?,
                        client: linux.client.ok_or(Error::Invalid("missing tmux client"))?,
                        window: raw.tmux_window,
                    }),
                    None => None,
                };
                Ok(Self::Linux {
                    window: raw.window_id.ok_or(Error::Invalid("missing window"))?,
                    terminal: linux.terminal_pid,
                    tmux,
                })
            }
            Platform::Darwin => {
                let origin = raw.omniwm_route.ok_or(Error::Invalid(
                    "missing OmniWM identity; enable OmniWM IPC and restart the sandbox",
                ))?;
                let tmux = match raw.tmux_target.filter(|v| !v.is_empty()) {
                    Some(pane) => Some(Tmux {
                        pane: pane.try_into()?,
                        socket: raw
                            .tmux_socket
                            .ok_or(Error::Invalid("missing tmux socket"))?
                            .try_into()?,
                        client: origin
                            .client
                            .clone()
                            .ok_or(Error::Invalid("missing tmux client"))?,
                        window: raw.tmux_window,
                    }),
                    None if origin.client.is_none() => None,
                    None => return Err(Error::Invalid("missing tmux pane")),
                };
                Ok(Self::Darwin { origin, tmux })
            }
        }
    }

    pub fn saved(&self) -> Value {
        match self {
            Self::Linux {
                window,
                terminal,
                tmux,
            } => {
                let mut value = json!({"window_id": window, "linux_route": {
                    "terminal_pid": terminal, "client": tmux.as_ref().map(|t| &t.client)
                }});
                if let Some(tmux) = tmux {
                    value["tmux_target"] = json!(tmux.pane);
                    value["tmux_socket"] = json!(tmux.socket);
                    value["tmux_window"] = json!(tmux.window);
                }
                value
            }
            Self::Darwin { origin, tmux } => json!({
                "omniwm_route": origin,
                "tmux_target": tmux.as_ref().map(|t| &t.pane),
                "tmux_socket": tmux.as_ref().map(|t| &t.socket),
                "tmux_window": tmux.as_ref().and_then(|t| t.window.as_ref()),
            }),
        }
    }
}

fn lease_active(path: &Path) -> bool {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) => {
            tracing::debug!(%error, path = %path.display(), "registration lease unavailable");
            return false;
        }
    };
    match file.try_lock() {
        Ok(()) => false,
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(std::fs::TryLockError::Error(error)) => {
            tracing::debug!(%error, path = %path.display(), "registration lease unavailable");
            false
        }
    }
}

pub struct Registration {
    pub route: Route,
    pub identity: Value,
}

pub fn registration(directory: &Path, target: &Target, platform: Platform) -> Option<Registration> {
    let path = directory.join(target.filename());
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            tracing::debug!(%error, path = %path.display(), "registration unavailable");
            return None;
        }
    };
    let value: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(error) => {
            tracing::debug!(%error, path = %path.display(), "malformed registration");
            return None;
        }
    };
    if value.get("focus_target").and_then(Value::as_str) != Some(target.as_str()) {
        return None;
    }
    if value.get("leased") == Some(&Value::Bool(true))
        && !lease_active(&path.with_extension("lease"))
    {
        return None;
    }
    let route = match Route::parse(&value, platform) {
        Ok(route) => route,
        Err(error) => {
            tracing::debug!(%error, path = %path.display(), "unusable registration");
            return None;
        }
    };
    let mut identity = json!({});
    let keys: &[&str] = match platform {
        Platform::Linux => &[
            "focus_target",
            "window_id",
            "terminal_app",
            "terminal_window_title",
            "tmux_target",
            "tmux_socket",
        ],
        Platform::Darwin => &["focus_target", "tmux_target", "tmux_socket"],
    };
    for &key in keys {
        identity[key] = value.get(key).cloned().unwrap_or(Value::Null);
    }
    let provenance = if platform == Platform::Linux {
        "linux_route"
    } else {
        "omniwm_route"
    };
    identity[provenance] = value.get(provenance).cloned().unwrap_or(Value::Null);
    Some(Registration { route, identity })
}
