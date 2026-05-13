use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(String);

impl RequestId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::RequestId;
    use anyhow::Result;

    #[test]
    fn display_round_trips_with_as_str() {
        let id = RequestId::new("req-1");
        assert_eq!(id.as_str(), "req-1");
        assert_eq!(id.to_string(), "req-1");
    }

    #[test]
    fn serde_round_trips_as_plain_string() -> Result<()> {
        let id = RequestId::new("req-42");
        let json = serde_json::to_string(&id)?;
        assert_eq!(json, "\"req-42\"");
        let back: RequestId = serde_json::from_str(&json)?;
        assert_eq!(back, id);
        Ok(())
    }
}
