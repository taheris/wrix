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
    #[cfg(test)]
    pub(crate) fn loopback_for_test() -> Self {
        Self {
            gateway: Ipv4Addr::LOCALHOST,
            network: u32::from(Ipv4Addr::new(127, 0, 0, 0)),
            mask: 0xffff_ff00,
        }
    }

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Interface {
    address: Address,
    name: String,
}

impl Interface {
    pub const fn address(&self) -> Address {
        self.address
    }

    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn bind(
        &self,
        ports: [std::num::NonZeroU16; 5],
    ) -> io::Result<crate::route::Listeners> {
        use socket2::{Domain, Protocol, Socket, Type};
        use std::num::NonZeroU32;
        let index = NonZeroU32::new(
            nix::net::if_::if_nametoindex(self.name.as_str()).map_err(io::Error::other)?,
        )
        .ok_or_else(invalid)?;
        let bind = |port: std::num::NonZeroU16| -> io::Result<tokio::net::TcpListener> {
            let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?;
            socket.bind_device_by_index_v4(Some(index))?;
            socket.set_nonblocking(true)?;
            socket
                .bind(&std::net::SocketAddr::from((self.address.gateway(), port.get())).into())?;
            socket.listen(128)?;
            tokio::net::TcpListener::from_std(socket.into())
        };
        let listeners = [
            bind(ports[0])?,
            bind(ports[1])?,
            bind(ports[2])?,
            bind(ports[3])?,
            bind(ports[4])?,
        ];
        if nix::net::if_::if_nametoindex(self.name.as_str()).map_err(io::Error::other)?
            != index.get()
        {
            return Err(invalid());
        }
        Ok(crate::route::Listeners::from_native(listeners))
    }
}

pub fn discover(tools: &crate::nix_store::owner::backend::tools::Tools) -> io::Result<Interface> {
    let observed = tools.apple_network().map_err(io::Error::other)?;
    observation(&observed.network, &observed.interfaces, &observed.release)
}

#[cfg(test)]
fn address(network: &[u8], interfaces: &[u8], darwin: &[u8]) -> io::Result<Address> {
    observation(network, interfaces, darwin).map(|interface| interface.address)
}

/// Inputs must come from the selected native `container network inspect default`,
/// `/sbin/ifconfig -a` and `/usr/bin/uname -r`, not caller configuration.
/// Container 1.5 uses reserved vmnet by default on Darwin25/macOS26+, and allocationOnly
/// earlier. The latter loses its host interface with a VM and cannot be a stable front door.
fn observation(network: &[u8], interfaces: &[u8], darwin: &[u8]) -> io::Result<Interface> {
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
    if address.network & mask != address.network
        || !Ipv4Addr::from(address.network | !mask).is_private()
        || !address.accepts(address.gateway)
    {
        return Err(invalid());
    }
    let text = std::str::from_utf8(interfaces).map_err(io::Error::other)?;
    let mut found = None;
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
            if found.is_some()
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
            found = Some(name.to_owned());
        } else if addresses.iter().any(|ip| address.accepts(*ip)) && !interface_name(name, "vmenet")
        {
            return Err(invalid());
        }
    }
    Ok(Interface {
        address,
        name: found.ok_or_else(invalid)?,
    })
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
