use super::newtype_id;

newtype_id!(SpecLabel);

#[cfg(test)]
mod tests {
    use super::SpecLabel;
    use anyhow::Result;

    #[test]
    fn display_round_trips_with_as_str() {
        let label = SpecLabel::new("loom-harness");
        assert_eq!(label.as_str(), "loom-harness");
        assert_eq!(label.to_string(), "loom-harness");
    }

    #[test]
    fn serde_round_trips_as_plain_string() -> Result<()> {
        let label = SpecLabel::new("ralph-loop");
        let json = serde_json::to_string(&label)?;
        assert_eq!(json, "\"ralph-loop\"");
        let back: SpecLabel = serde_json::from_str(&json)?;
        assert_eq!(back, label);
        Ok(())
    }
}
