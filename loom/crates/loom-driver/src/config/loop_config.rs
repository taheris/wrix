use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct LoopConfig {
    pub max_iterations: u32,
    pub max_retries: u32,
    pub max_reviews: u32,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_iterations: 3,
            max_retries: 2,
            max_reviews: 2,
        }
    }
}
