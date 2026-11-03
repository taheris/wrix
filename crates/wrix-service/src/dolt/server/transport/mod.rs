//! Creation-time socket placement is independent of native TCP role publication.

use crate::dolt::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Transport {
    Unix,
    Tcp,
    NativeUnix,
}

impl std::str::FromStr for Transport {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "unix" => Ok(Self::Unix),
            "tcp" => Ok(Self::Tcp),
            "native-unix" => Ok(Self::NativeUnix),
            _ => Err(Error::UnsafePath),
        }
    }
}

impl Transport {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unix => "unix",
            Self::Tcp => "tcp",
            Self::NativeUnix => "native-unix",
        }
    }

    pub(super) const fn mounted_sockets(self) -> bool {
        matches!(self, Self::Unix | Self::NativeUnix)
    }

    pub(crate) const fn tcp_sync(self) -> bool {
        matches!(self, Self::Tcp | Self::NativeUnix)
    }
}
