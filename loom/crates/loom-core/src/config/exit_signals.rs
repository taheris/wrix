use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ExitSignalsConfig {
    pub complete: String,
    pub blocked: String,
    pub clarify: String,
}

impl Default for ExitSignalsConfig {
    fn default() -> Self {
        Self {
            complete: "LOOM_COMPLETE".to_string(),
            blocked: "LOOM_BLOCKED".to_string(),
            clarify: "LOOM_CLARIFY".to_string(),
        }
    }
}
