use super::newtype_id;

newtype_id!(ProfileName);

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
