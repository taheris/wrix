//! Optional repository cache overrides; omitted values preserve lower-priority settings.

use serde::{Deserialize, Deserializer, de};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub enable: Option<bool>,
    pub require_trusted_nix: Option<bool>,
    #[serde(default, deserialize_with = "super::deserialize_table")]
    pub publish: Roots,
    #[serde(default, deserialize_with = "super::deserialize_table")]
    pub warm: Roots,
    pub warn_size: Option<Bytes>,
    pub pending_ttl: Option<Seconds>,
    pub lock_timeout: Option<Milliseconds>,
    pub explicit_lock_timeout: Option<Milliseconds>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Roots {
    pub packages: Option<bool>,
    pub checks: Option<bool>,
    pub dev_shell: Option<bool>,
    pub include_roots: Option<Vec<String>>,
    pub exclude_roots: Option<Vec<String>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Bytes(u64);

impl Bytes {
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Seconds(u64);

impl Seconds {
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Milliseconds(u64);

impl Milliseconds {
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Quantity {
    Number(u64),
    Text(String),
}

fn scaled<'de, D>(deserializer: D, scales: &[(&str, u64)]) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    match Quantity::deserialize(deserializer)? {
        Quantity::Number(value) => Ok(value),
        Quantity::Text(text) => {
            let digits = text.bytes().take_while(u8::is_ascii_digit).count();
            let (number, suffix) = text.split_at(digits);
            let scale = scales
                .iter()
                .find_map(|(unit, scale)| (*unit == suffix).then_some(*scale))
                .ok_or_else(|| de::Error::custom(format!("invalid quantity {text:?}")))?;
            let value = number
                .parse::<u64>()
                .map_err(|_source| de::Error::custom(format!("invalid quantity {text:?}")))?;
            value.checked_mul(scale).ok_or_else(|| {
                de::Error::custom(format!("invalid or overflowing quantity {text:?}"))
            })
        }
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        scaled(
            deserializer,
            &[
                ("", 1),
                ("K", 1024),
                ("M", 1024 * 1024),
                ("G", 1024 * 1024 * 1024),
                ("T", 1024 * 1024 * 1024 * 1024),
            ],
        )
        .map(Self)
    }
}

impl<'de> Deserialize<'de> for Milliseconds {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match Quantity::deserialize(deserializer)? {
            Quantity::Number(value) => value
                .checked_mul(1000)
                .map(Self)
                .ok_or_else(|| de::Error::custom("duration overflows milliseconds")),
            Quantity::Text(text) => crate::duration::milliseconds(&text)
                .map(Self)
                .map_err(de::Error::custom),
        }
    }
}

impl<'de> Deserialize<'de> for Seconds {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match Quantity::deserialize(deserializer)? {
            Quantity::Number(value) => Ok(Self(value)),
            Quantity::Text(text) => crate::duration::seconds(&text)
                .map(Self)
                .map_err(de::Error::custom),
        }
    }
}
