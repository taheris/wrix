use super::newtype_id;

newtype_id!(RequestId);

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
