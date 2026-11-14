#[cfg(target_os = "linux")]
pub mod live;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// malformed cgroup controller list
    Controller,
    /// required cgroup controllers are unavailable
    Unavailable,
    /// malformed cgroup memory events
    Events,
    /// cgroup bootstrap process is not an owned supported root process
    Process,
}
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Controller {
    Cpu,
    Memory,
    Pids,
    Other(Name),
}
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Name(String);
impl Controller {
    fn parse(value: &str) -> Result<Self, Error> {
        if value.is_empty()
            || !value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(Error::Controller);
        }
        Ok(match value {
            "cpu" => Self::Cpu,
            "memory" => Self::Memory,
            "pids" => Self::Pids,
            _ => Self::Other(Name(value.into())),
        })
    }
    pub fn wire(&self) -> &str {
        match self {
            Self::Cpu => "cpu",
            Self::Memory => "memory",
            Self::Pids => "pids",
            Self::Other(value) => &value.0,
        }
    }
}
#[derive(Clone, Copy)]
pub enum Change {
    Enable,
    Restore,
}
pub struct Delegation {
    before: BTreeSet<Controller>,
    added: BTreeSet<Controller>,
}
impl Delegation {
    fn list(bytes: &[u8]) -> Result<BTreeSet<Controller>, Error> {
        let text = std::str::from_utf8(bytes).map_err(|_| Error::Controller)?;
        text.split_whitespace().map(Controller::parse).collect()
    }
    pub fn parse(available: &[u8], active: &[u8]) -> Result<Self, Error> {
        let available = Self::list(available)?;
        let before = Self::list(active)?;
        let required = BTreeSet::from([Controller::Cpu, Controller::Memory, Controller::Pids]);
        if !required.is_subset(&available) || !before.is_subset(&available) {
            return Err(Error::Unavailable);
        }
        let added = required.difference(&before).cloned().collect();
        Ok(Self { before, added })
    }
    pub fn command(&self, change: Change) -> String {
        let prefix = match change {
            Change::Enable => '+',
            Change::Restore => '-',
        };
        self.added
            .iter()
            .map(|value| format!("{prefix}{}", value.wire()))
            .collect::<Vec<_>>()
            .join(" ")
    }
    pub fn restored(&self, active: &[u8]) -> Result<bool, Error> {
        Ok(Self::list(active)? == self.before)
    }
}
#[derive(Debug)]
pub struct Events {
    pub oom_kill: u64,
}
impl Events {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let text = std::str::from_utf8(bytes).map_err(|_| Error::Events)?;
        let mut events = BTreeMap::new();
        for line in text.lines() {
            let words: Vec<_> = line.split_whitespace().collect();
            let [name, value] = words.as_slice() else {
                return Err(Error::Events);
            };
            let value = value.parse::<u64>().map_err(|_| Error::Events)?;
            if events.insert(*name, value).is_some() {
                return Err(Error::Events);
            }
        }
        let oom_kill = *events.get("oom_kill").ok_or(Error::Events)?;
        Ok(Self { oom_kill })
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Bootstrap(u32);
impl Bootstrap {
    pub fn parse(pid: &str, status: &[u8]) -> Result<Self, Error> {
        if pid.is_empty() || !pid.bytes().all(|value| value.is_ascii_digit()) {
            return Err(Error::Process);
        }
        let pid = pid.parse::<u32>().map_err(|_| Error::Process)?;
        if pid == 0 {
            return Err(Error::Process);
        }
        let text = std::str::from_utf8(status).map_err(|_| Error::Process)?;
        let mut fields = BTreeMap::new();
        for line in text.lines() {
            if let Some((name, value)) = line.split_once(':')
                && fields.insert(name, value.trim()).is_some()
            {
                return Err(Error::Process);
            }
        }
        let uid = fields
            .get("Uid")
            .ok_or(Error::Process)?
            .split_whitespace()
            .collect::<Vec<_>>();
        if uid.len() != 4 || uid.iter().any(|v| *v != "0") {
            return Err(Error::Process);
        }
        if !matches!(
            fields.get("Name").copied(),
            Some("sh" | "sleep" | "bash" | "wrix-native-tes")
        ) {
            return Err(Error::Process);
        }
        Ok(Self(pid))
    }
    pub const fn raw(self) -> u32 {
        self.0
    }
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn delegation_enables_and_restores_only_the_previously_missing_controllers() {
        let plan = Delegation::parse(b"cpu memory pids io\n", b"cpu io\n").unwrap();
        assert_eq!(plan.command(Change::Enable), "+memory +pids");
        assert_eq!(plan.command(Change::Restore), "-memory -pids");
        assert!(plan.restored(b"io cpu\n").unwrap());
        assert!(!plan.restored(b"cpu memory io\n").unwrap());
    }
    #[test]
    fn missing_controllers_fail_before_a_delegation_can_be_created() {
        assert!(Delegation::parse(b"cpu pids", b"").is_err());
        assert!(Delegation::parse(b"cpu memory pids", b"io").is_err());
    }
    #[test]
    fn existing_full_delegation_has_no_controller_writes_to_restore() {
        let plan = Delegation::parse(b"cpu memory pids", b"pids memory cpu").unwrap();
        assert_eq!(plan.command(Change::Enable), "");
        assert_eq!(plan.command(Change::Restore), "");
    }
    #[test]
    fn malformed_controller_commands_are_not_accepted_as_observations() {
        for value in [
            b"+cpu memory pids".as_slice(),
            b"cpu memory pids;",
            b"cpu memory pids\xff",
        ] {
            assert!(Delegation::parse(value, b"").is_err());
        }
    }
    #[test]
    fn events_keep_the_exact_oom_kill_counter() {
        assert_eq!(
            Events::parse(b"low 0\nhigh 1\noom 3\noom_kill 2\noom_group_kill 0\n")
                .unwrap()
                .oom_kill,
            2
        );
    }
    #[test]
    fn absent_duplicate_or_invalid_oom_counters_fail_closed() {
        for data in [
            b"oom 1\n".as_slice(),
            b"oom_kill -1\n",
            b"oom_kill 1\noom_kill 2\n",
            b"oom_kill 0 extra\n",
        ] {
            assert!(Events::parse(data).is_err());
        }
    }
    #[test]
    fn owned_bootstrap_requires_all_root_ids_and_a_supported_actor() {
        assert_eq!(
            Bootstrap::parse("42", b"Name:\twrix-native-tes\nUid:\t0\t0\t0\t0\n")
                .unwrap()
                .raw(),
            42
        );
        for data in [
            b"Name:\tbash\nUid:\t0 1 0 0\n".as_slice(),
            b"Name:\tagent\nUid:\t0 0 0 0\n",
            b"Name:\tbash\nUid:\t0\n",
            b"Name:\tbash\nName:\tsleep\nUid:\t0 0 0 0\n",
        ] {
            assert!(Bootstrap::parse("42", data).is_err());
        }
        assert!(Bootstrap::parse("0", b"Name: bash\nUid: 0 0 0 0\n").is_err());
    }
}
