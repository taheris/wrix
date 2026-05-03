use super::newtype_id;

newtype_id!(SessionId);

#[cfg(test)]
mod tests {
    use super::SessionId;
    use anyhow::Result;

    #[test]
    fn display_round_trips_with_as_str() {
        let id = SessionId::new("session-abc");
        assert_eq!(id.as_str(), "session-abc");
        assert_eq!(id.to_string(), "session-abc");
    }

    #[test]
    fn serde_round_trips_as_plain_string() -> Result<()> {
        let id = SessionId::new("sess-xyz");
        let json = serde_json::to_string(&id)?;
        assert_eq!(json, "\"sess-xyz\"");
        let back: SessionId = serde_json::from_str(&json)?;
        assert_eq!(back, id);
        Ok(())
    }
}
