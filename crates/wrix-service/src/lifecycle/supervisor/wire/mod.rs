use crate::control::Namespace;
use serde::{Deserialize, Serialize};

pub(super) const VERSION: u16 = 1;
pub(super) const LIMIT: usize = 1024;
pub(super) const SOCKET: &str = "component.sock";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Inspect,
    Retire,
    Resume,
}

impl Action {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inspect => "inspect",
            Self::Retire => "retire",
            Self::Resume => "resume",
        }
    }
}

/// Component observation, never authenticated readiness or whole-backend release evidence.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Observation {
    Active,
    Retiring,
    Retired,
    Busy,
    Rejected,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub version: u16,
    pub namespace: Namespace,
    pub action: Action,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reply {
    pub version: u16,
    pub observation: Observation,
}
