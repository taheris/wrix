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
            complete: "RALPH_COMPLETE".to_string(),
            blocked: "RALPH_BLOCKED:".to_string(),
            clarify: "RALPH_CLARIFY:".to_string(),
        }
    }
}
