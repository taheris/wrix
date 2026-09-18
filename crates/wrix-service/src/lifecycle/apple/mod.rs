use std::{collections::BTreeMap, num::NonZeroU16, ops::RangeInclusive};

use serde::{Deserialize, Deserializer, de};

use super::{ContainerInfo, RuntimeStatus};

#[derive(Debug, Deserialize)]
pub(super) struct Snapshot {
    pub configuration: Configuration,
    status: Status,
    #[serde(default, rename = "networks")]
    legacy_networks: Vec<Network>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Configuration {
    pub id: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    published_ports: Vec<PublishedPort>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Status {
    Nested {
        state: State,
        #[serde(default)]
        networks: Vec<Network>,
    },
    Legacy(State),
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum State {
    Running,
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct Network {
    #[serde(rename = "ipv4Address")]
    address: String,
}

#[derive(Debug)]
struct PublishedPort(RangeInclusive<u16>);

impl Snapshot {
    pub const fn runtime_status(&self) -> RuntimeStatus {
        let (Status::Nested { state, .. } | Status::Legacy(state)) = &self.status;
        match state {
            State::Running => RuntimeStatus::Running,
            State::Other => RuntimeStatus::Stopped,
        }
    }

    pub fn ipv4_address(&self) -> Option<std::net::Ipv4Addr> {
        let networks = match &self.status {
            Status::Nested { networks, .. } => networks,
            Status::Legacy(_) => &self.legacy_networks,
        };
        networks.iter().find_map(|network| {
            let address = network.address.split('/').next()?;
            match address.parse::<std::net::Ipv4Addr>() {
                Ok(address) if !address.is_loopback() && !address.is_unspecified() => Some(address),
                Ok(_) | Err(_) => None,
            }
        })
    }

    pub fn into_info(mut self) -> ContainerInfo {
        ContainerInfo {
            name: self.configuration.id,
            kind: self.configuration.labels.remove("wrix.kind"),
            workspace_hash: self.configuration.labels.remove("wrix.workspace.hash"),
            workspace_path: self.configuration.labels.remove("wrix.workspace"),
            published_ports: self
                .configuration
                .published_ports
                .into_iter()
                .flat_map(|port| port.0)
                .collect(),
        }
    }
}

impl<'de> Deserialize<'de> for PublishedPort {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire {
            host_port: NonZeroU16,
            count: Option<NonZeroU16>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let start = wire.host_port.get();
        let count = wire.count.unwrap_or(NonZeroU16::MIN).get();
        let end = start
            .checked_add(count - 1)
            .ok_or_else(|| de::Error::custom("published port range exceeds 65535"))?;
        Ok(Self(start..=end))
    }
}

#[cfg(test)]
mod test {
    use serde_json::json;

    use super::Snapshot;

    #[test]
    fn sandbox_address_uses_current_status_networks_not_stale_top_level_values() {
        let snapshot: Snapshot = serde_json::from_value(json!({
            "configuration": {"id": "service"},
            "status": {"state": "running", "networks": []},
            "networks": [{"ipv4Address": "192.168.64.99/24"}]
        }))
        .unwrap();
        assert_eq!(snapshot.ipv4_address(), None);
    }

    #[test]
    fn sandbox_address_skips_unusable_interfaces() {
        let snapshot: Snapshot = serde_json::from_value(json!({
            "configuration": {"id": "service"},
            "status": {
                "state": "running",
                "networks": [
                    {"ipv4Address": "127.0.0.1/8"},
                    {"ipv4Address": "0.0.0.0/0"},
                    {"ipv4Address": "invalid"},
                    {"ipv4Address": "192.168.64.12/24"}
                ]
            }
        }))
        .unwrap();
        assert_eq!(
            snapshot.ipv4_address(),
            Some(std::net::Ipv4Addr::new(192, 168, 64, 12))
        );
    }

    #[test]
    fn published_port_ranges_include_each_reserved_host_port() {
        let snapshot: Snapshot = serde_json::from_value(json!({
            "configuration": {
                "id": "service",
                "publishedPorts": [{"hostPort": 24000, "count": 3}, {"hostPort": 22000}]
            },
            "status": {"state": "running"}
        }))
        .unwrap();
        assert_eq!(
            snapshot.into_info().published_ports,
            [24000, 24001, 24002, 22000]
        );
    }

    #[test]
    fn invalid_published_port_ranges_are_rejected() {
        for port in [
            json!({"hostPort": 65535, "count": 2}),
            json!({"hostPort": 24000, "count": 0}),
            json!({"hostPort": 0}),
        ] {
            assert!(
                serde_json::from_value::<Snapshot>(json!({
                    "configuration": {"id": "service", "publishedPorts": [port]},
                    "status": {"state": "running"}
                }))
                .is_err()
            );
        }
    }
}
