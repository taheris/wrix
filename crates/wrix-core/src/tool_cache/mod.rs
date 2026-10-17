//! Launch tool-cache sharing preferences, independent of Nix-cache policy.

use std::fmt;

use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, Visitor},
};

/// An omitted choice inherits repository policy, then shared mode.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Preference {
    pub shared: Option<bool>,
}

impl<'de> Deserialize<'de> for Preference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PreferenceVisitor;

        impl<'de> Visitor<'de> for PreferenceVisitor {
            type Value = Preference;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object with optional boolean shared")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut shared = None;
                while let Some(key) = map.next_key::<String>()? {
                    if key != "shared" {
                        return Err(de::Error::unknown_field(&key, &["shared"]));
                    }
                    if shared.is_some() {
                        return Err(de::Error::duplicate_field("shared"));
                    }
                    shared = Some(map.next_value::<bool>()?);
                }
                Ok(Preference { shared })
            }
        }

        deserializer.deserialize_map(PreferenceVisitor)
    }
}

/// The effective all-cache mode for one launch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Shared,
    Private,
}

impl Mode {
    pub fn resolve(explicit: Preference, repository: Preference) -> Self {
        if explicit.shared.or(repository.shared).unwrap_or(true) {
            Self::Shared
        } else {
            Self::Private
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Private => "private",
        }
    }
}
