//! Exact host/port policy and the worker's protected-address baseline.

use super::Error;
use serde::{Deserialize, Serialize};
use std::{
    net::{Ipv4Addr, SocketAddr},
    num::NonZeroU16,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Host(String);
impl Host {
    pub fn parse(value: &str) -> Result<Self, Error> {
        let value = value.to_ascii_lowercase();
        if value.len() > 253
            || value.is_empty()
            || value.split('.').any(|part| {
                part.is_empty()
                    || part.len() > 63
                    || part.starts_with('-')
                    || part.ends_with('-')
                    || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        {
            return Err(Error::Invalid);
        }
        if value.bytes().all(|b| b.is_ascii_digit() || b == b'.')
            && value.parse::<Ipv4Addr>().is_err()
        {
            return Err(Error::Invalid);
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for Host {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Error> {
        Self::parse(&value)
    }
}
impl From<Host> for String {
    fn from(value: Host) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawRule", into = "RawRule")]
pub struct Rule {
    host: Host,
    ports: Vec<NonZeroU16>,
}
impl Rule {
    pub fn new(host: Host, ports: Vec<NonZeroU16>) -> Result<Self, Error> {
        if ports.is_empty() || ports.len() > 256 {
            return Err(Error::Invalid);
        }
        Ok(Self { host, ports })
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    host: Host,
    ports: Vec<NonZeroU16>,
}
impl TryFrom<RawRule> for Rule {
    type Error = Error;
    fn try_from(value: RawRule) -> Result<Self, Error> {
        Self::new(value.host, value.ports)
    }
}
impl From<Rule> for RawRule {
    fn from(value: Rule) -> Self {
        Self {
            host: value.host,
            ports: value.ports,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Vec<Rule>", into = "Vec<Rule>")]
pub struct Policy(Vec<Rule>);
impl Policy {
    pub fn new(rules: Vec<Rule>) -> Result<Self, Error> {
        if rules.len() > 256 {
            return Err(Error::Invalid);
        }
        Ok(Self(rules))
    }
    pub fn permits(&self, host: &Host, port: NonZeroU16) -> bool {
        self.0
            .iter()
            .any(|rule| rule.host == *host && rule.ports.contains(&port))
    }
}
impl TryFrom<Vec<Rule>> for Policy {
    type Error = Error;
    fn try_from(value: Vec<Rule>) -> Result<Self, Error> {
        Self::new(value)
    }
}
impl From<Policy> for Vec<Rule> {
    fn from(value: Policy) -> Self {
        value.0
    }
}

/// IPv6 is denied, matching the worker's current IPv6 default-drop contract.
#[derive(Clone)]
pub(super) struct Baseline(Vec<(u32, u32)>);
impl Baseline {
    pub fn load() -> Result<Self, Error> {
        let source = include_str!("../../../../../lib/sandbox/network-bootstrap.sh");
        let ranges = source
            .split("local blocked_cidrs=(")
            .nth(1)
            .and_then(|s| s.split(')').next())
            .ok_or(Error::Invalid)?;
        let ranges = ranges
            .split_whitespace()
            .map(|cidr| {
                let (ip, prefix) = cidr.split_once('/').ok_or(Error::Invalid)?;
                let ip = u32::from(ip.parse::<Ipv4Addr>().map_err(|_| Error::Invalid)?);
                let prefix = prefix.parse::<u32>().map_err(|_| Error::Invalid)?;
                if prefix > 32 {
                    return Err(Error::Invalid);
                }
                let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
                Ok((ip & mask, mask))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        if ranges.len() < 15 {
            return Err(Error::Invalid);
        }
        Ok(Self(ranges))
    }
    pub fn permits(&self, address: SocketAddr) -> bool {
        match address {
            SocketAddr::V4(address) => !self
                .0
                .iter()
                .any(|(network, mask)| u32::from(*address.ip()) & mask == *network),
            SocketAddr::V6(_) => false,
        }
    }
}
