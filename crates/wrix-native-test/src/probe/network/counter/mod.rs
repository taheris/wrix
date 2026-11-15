use std::num::NonZeroU64;
#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// owned nftables observation exceeded its bound or had malformed structure
    Structure,
    /// owned nftables JSON failed: {0}
    Json(#[from] serde_json::Error),
    /// owned private-service drop rule had no counter
    Missing,
    /// owned private-service drop counter did not observe any packets
    Zero,
    /// owned private-service drop counter overflowed
    Overflow,
    /// owned private-service drop counter did not increase
    Unchanged,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Count(NonZeroU64);
impl Count {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > 1024 * 1024 {
            return Err(Error::Structure);
        }
        let value: serde_json::Value = serde_json::from_slice(bytes)?;
        let records = value
            .get("nftables")
            .and_then(serde_json::Value::as_array)
            .ok_or(Error::Structure)?;
        let mut total = 0u64;
        let mut found = false;
        for record in records {
            let Some(rule) = record.get("rule") else {
                continue;
            };
            if rule.get("family").and_then(serde_json::Value::as_str) != Some("inet")
                || rule.get("table").and_then(serde_json::Value::as_str)
                    != Some("wrix_npfo4_egress")
                || rule.get("comment").and_then(serde_json::Value::as_str) != Some("core-probe")
            {
                continue;
            }
            for expression in rule
                .get("expr")
                .and_then(serde_json::Value::as_array)
                .ok_or(Error::Structure)?
            {
                let Some(counter) = expression.get("counter") else {
                    continue;
                };
                let packets = counter
                    .get("packets")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or(Error::Structure)?;
                total = total.checked_add(packets).ok_or(Error::Overflow)?;
                found = true;
            }
        }
        if !found {
            return Err(Error::Missing);
        }
        Ok(Self(NonZeroU64::new(total).ok_or(Error::Zero)?))
    }
    pub const fn packets(self) -> u64 {
        self.0.get()
    }
    pub fn increased_from(self, before: Self) -> Result<(), Error> {
        if self.0 > before.0 {
            Ok(())
        } else {
            Err(Error::Unchanged)
        }
    }
}
#[cfg(test)]
mod test;
