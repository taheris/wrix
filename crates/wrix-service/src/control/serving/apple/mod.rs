//! Read-only evidence for a stable host vmnet listener; never configure host networking.
use serde::Deserialize;
use std::{collections::BTreeMap, io, net::Ipv4Addr};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Address {
    gateway: Ipv4Addr,
    network: u32,
    mask: u32,
}
impl Address {
    pub const fn gateway(self) -> Ipv4Addr {
        self.gateway
    }
    pub fn accepts(self, peer: Ipv4Addr) -> bool {
        let peer = u32::from(peer);
        peer & self.mask == self.network
            && peer != self.network
            && peer != (self.network | !self.mask)
    }
}
#[derive(Deserialize)]
struct Network {
    id: String,
    configuration: Configuration,
    status: Status,
}
#[derive(Deserialize)]
struct Configuration {
    name: String,
    mode: Mode,
    plugin: String,
    options: BTreeMap<String, String>,
}
#[derive(Deserialize, PartialEq)]
enum Mode {
    #[serde(rename = "nat")]
    Nat,
    #[serde(rename = "hostOnly")]
    HostOnly,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    ipv4_gateway: Ipv4Addr,
    ipv4_subnet: String,
}
fn invalid() -> io::Error {
    io::Error::other(
        "stable Apple serving requires an existing reserved default vmnet network and an isolated active native bridge; no host policy was changed",
    )
}
fn interface_name(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix).is_some_and(|number| {
        !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
    })
}

pub fn discover(tools: &crate::nix_store::owner::backend::tools::Tools) -> io::Result<Address> {
    let observed = tools.apple_network().map_err(io::Error::other)?;
    address(&observed.network, &observed.interfaces, &observed.release)
}

/// Inputs must come from the selected native `container network inspect default`,
/// `/sbin/ifconfig -a` and `/usr/bin/uname -r`, not caller configuration.
/// Container 1.5 uses reserved vmnet by default on Darwin25/macOS26+, and allocationOnly
/// earlier. The latter loses its host interface with a VM and cannot be a stable front door.
fn address(network: &[u8], interfaces: &[u8], darwin: &[u8]) -> io::Result<Address> {
    if network.len() > 65_536 || interfaces.len() > 1_048_576 || darwin.len() > 128 {
        return Err(invalid());
    }
    let networks: Vec<Network> = serde_json::from_slice(network).map_err(io::Error::other)?;
    let [network] = networks.as_slice() else {
        return Err(invalid());
    };
    let config = &network.configuration;
    let release = std::str::from_utf8(darwin)
        .map_err(io::Error::other)?
        .trim();
    let major: u32 = release
        .split('.')
        .next()
        .ok_or_else(invalid)?
        .parse()
        .map_err(io::Error::other)?;
    if network.id != "default"
        || config.name != "default"
        || config.plugin != "container-network-vmnet"
        || config.mode != Mode::Nat
        || major < 25
        || config
            .options
            .get("variant")
            .is_some_and(|variant| variant != "reserved")
    {
        return Err(invalid());
    }
    let (base, prefix) = network
        .status
        .ipv4_subnet
        .split_once('/')
        .ok_or_else(invalid)?;
    let base: Ipv4Addr = base.parse().map_err(io::Error::other)?;
    let prefix: u32 = prefix.parse().map_err(io::Error::other)?;
    if !(8..=30).contains(&prefix)
        || !base.is_private()
        || !network.status.ipv4_gateway.is_private()
    {
        return Err(invalid());
    }
    let mask = u32::MAX.checked_shl(32 - prefix).ok_or_else(invalid)?;
    let address = Address {
        gateway: network.status.ipv4_gateway,
        network: u32::from(base),
        mask,
    };
    if address.network & mask != address.network || !address.accepts(address.gateway) {
        return Err(invalid());
    }
    let text = std::str::from_utf8(interfaces).map_err(io::Error::other)?;
    let mut found = false;
    for section in sections(text) {
        let header = section.first().ok_or_else(invalid)?;
        let name = header.split_once(':').ok_or_else(invalid)?.0;
        let addresses = section
            .iter()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                (fields.next() == Some("inet"))
                    .then(|| fields.next())
                    .flatten()
            })
            .map(str::parse::<Ipv4Addr>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(io::Error::other)?;
        if addresses.contains(&address.gateway) {
            let flags = header
                .split_once('<')
                .and_then(|(_, flags)| flags.split_once('>').map(|(flags, _)| flags))
                .ok_or_else(invalid)?;
            if found
                || !interface_name(name, "bridge")
                || !flags.split(',').any(|flag| flag == "UP")
                || !flags.split(',').any(|flag| flag == "RUNNING")
            {
                return Err(invalid());
            }
            let members: Vec<_> = section
                .iter()
                .filter_map(|line| {
                    line.trim()
                        .strip_prefix("member: ")
                        .and_then(|line| line.split_whitespace().next())
                })
                .collect();
            if members
                .iter()
                .any(|member| !interface_name(member, "vmenet"))
            {
                return Err(invalid());
            }
            found = true;
        } else if addresses.iter().any(|ip| address.accepts(*ip)) && !interface_name(name, "vmenet")
        {
            return Err(invalid());
        }
    }
    if !found {
        return Err(invalid());
    }
    Ok(address)
}
fn sections(text: &str) -> Vec<Vec<&str>> {
    let mut result: Vec<Vec<&str>> = Vec::new();
    for line in text.lines().filter(|line| !line.is_empty()) {
        if !line.starts_with(char::is_whitespace) {
            result.push(vec![line]);
        } else if let Some(section) = result.last_mut() {
            section.push(line);
        }
    }
    result
}

#[cfg(test)]
mod test;
