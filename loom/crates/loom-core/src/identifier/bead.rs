use super::newtype_id;

newtype_id!(BeadId);

#[cfg(test)]
mod tests {
    use super::BeadId;
    use anyhow::Result;

    #[test]
    fn display_round_trips_with_as_str() {
        let id = BeadId::new("wx-3hhwq.2");
        assert_eq!(id.as_str(), "wx-3hhwq.2");
        assert_eq!(id.to_string(), "wx-3hhwq.2");
    }

    #[test]
    fn serde_round_trips_as_plain_string() -> Result<()> {
        let id = BeadId::new("wx-abc123");
        let json = serde_json::to_string(&id)?;
        assert_eq!(json, "\"wx-abc123\"");
        let back: BeadId = serde_json::from_str(&json)?;
        assert_eq!(back, id);
        Ok(())
    }
}
