use serde::{Deserialize, Serialize};

/// Package-selected backend VM limits. These are not workspace or guest inputs.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(try_from = "Wire")]
pub struct Resources {
    cpus: u16,
    memory_mib: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    cpus: u16,
    memory_mib: u32,
}

impl Resources {
    pub const fn cpus(&self) -> u16 {
        self.cpus
    }

    pub const fn memory_mib(&self) -> u32 {
        self.memory_mib
    }
}

impl TryFrom<Wire> for Resources {
    type Error = &'static str;

    fn try_from(wire: Wire) -> Result<Self, Self::Error> {
        if !(1..=256).contains(&wire.cpus) {
            return Err("backend CPUs must be between 1 and 256");
        }
        if !(512..=1_048_576).contains(&wire.memory_mib) {
            return Err("backend memory must be between 512 and 1048576 MiB");
        }
        Ok(Self {
            cpus: wire.cpus,
            memory_mib: wire.memory_mib,
        })
    }
}

#[cfg(test)]
mod test {
    use super::Resources;

    #[test]
    fn builder_sized_limits_round_trip() {
        let value = serde_json::json!({"cpus":8,"memory_mib":8192});
        let limits: Resources = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(limits).unwrap(), value);
    }

    #[test]
    fn service_only_limits_are_configurable() {
        let limits: Resources =
            serde_json::from_value(serde_json::json!({"cpus":4,"memory_mib":4096})).unwrap();
        assert_eq!((limits.cpus(), limits.memory_mib()), (4, 4096));
    }

    #[test]
    fn zero_cpus_are_rejected() {
        assert!(
            serde_json::from_value::<Resources>(serde_json::json!({"cpus":0,"memory_mib":8192}))
                .is_err()
        );
    }

    #[test]
    fn excessive_cpus_are_rejected() {
        assert!(
            serde_json::from_value::<Resources>(serde_json::json!({"cpus":257,"memory_mib":8192}))
                .is_err()
        );
    }

    #[test]
    fn undersized_memory_is_rejected() {
        assert!(
            serde_json::from_value::<Resources>(serde_json::json!({"cpus":8,"memory_mib":511}))
                .is_err()
        );
    }

    #[test]
    fn excessive_memory_is_rejected() {
        assert!(
            serde_json::from_value::<Resources>(
                serde_json::json!({"cpus":8,"memory_mib":1_048_577})
            )
            .is_err()
        );
    }

    #[test]
    fn fractional_cpus_are_rejected() {
        assert!(
            serde_json::from_value::<Resources>(serde_json::json!({"cpus":1.5,"memory_mib":8192}))
                .is_err()
        );
    }

    #[test]
    fn guest_configuration_is_not_a_resource_field() {
        assert!(
            serde_json::from_value::<Resources>(serde_json::json!({
                "cpus":8,"memory_mib":8192,"environment":{"NIX_CONFIG":"sandbox=false"}
            }))
            .is_err()
        );
    }
}
