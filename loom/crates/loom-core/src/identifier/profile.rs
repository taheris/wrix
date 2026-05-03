use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProfileName(String);

impl ProfileName {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProfileName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::ProfileName;
    use anyhow::Result;

    #[test]
    fn display_round_trips_with_as_str() {
        let p = ProfileName::new("rust");
        assert_eq!(p.as_str(), "rust");
        assert_eq!(p.to_string(), "rust");
    }

    #[test]
    fn serde_round_trips_as_plain_string() -> Result<()> {
        let p = ProfileName::new("python");
        let json = serde_json::to_string(&p)?;
        assert_eq!(json, "\"python\"");
        let back: ProfileName = serde_json::from_str(&json)?;
        assert_eq!(back, p);
        Ok(())
    }
}
