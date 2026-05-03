use super::newtype_id;

newtype_id!(MoleculeId);

#[cfg(test)]
mod tests {
    use super::MoleculeId;
    use anyhow::Result;

    #[test]
    fn display_round_trips_with_as_str() {
        let id = MoleculeId::new("wx-3hhwq");
        assert_eq!(id.as_str(), "wx-3hhwq");
        assert_eq!(id.to_string(), "wx-3hhwq");
    }

    #[test]
    fn serde_round_trips_as_plain_string() -> Result<()> {
        let id = MoleculeId::new("wx-mol42");
        let json = serde_json::to_string(&id)?;
        assert_eq!(json, "\"wx-mol42\"");
        let back: MoleculeId = serde_json::from_str(&json)?;
        assert_eq!(back, id);
        Ok(())
    }
}
