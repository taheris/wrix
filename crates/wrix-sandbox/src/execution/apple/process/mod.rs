//! Native runtime release requires both launchd deregistration and absence of the original host PID.
use super::{Error, Identity, run};
use serde::{Deserialize, Serialize};
use std::{num::NonZeroU32, process::Command};
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Process {
    identity: Identity,
    pid: Pid,
    domain: Domain,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(try_from = "u32", into = "u32")]
struct Pid(NonZeroU32);
impl TryFrom<u32> for Pid {
    type Error = Error;
    fn try_from(value: u32) -> Result<Self, Error> {
        if value > i32::MAX.cast_unsigned() {
            return Err(Error::Unknown);
        }
        Ok(Self(NonZeroU32::new(value).ok_or(Error::Unknown)?))
    }
}
impl From<Pid> for u32 {
    fn from(value: Pid) -> Self {
        value.0.get()
    }
}
impl Pid {
    const fn get(self) -> u32 {
        self.0.get()
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Domain {
    User,
    Gui,
}
impl Domain {
    fn unit(self, identity: &Identity) -> String {
        let domain = match self {
            Self::User => "user",
            Self::Gui => "gui",
        };
        format!(
            "{domain}/{}/com.apple.container.container-runtime-linux.{}",
            rustix::process::getuid().as_raw(),
            identity.name()
        )
    }
}
fn command() -> Command {
    let mut command = Command::new("/bin/launchctl");
    command.env_clear().current_dir("/");
    command
}
fn pid(bytes: &[u8]) -> Result<Pid, Error> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::Unknown)?;
    let mut values = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("pid = "));
    let pid: u32 = values
        .next()
        .ok_or(Error::Unknown)?
        .parse()
        .map_err(|_| Error::Unknown)?;
    if values.next().is_some() {
        return Err(Error::Unknown);
    }
    Pid::try_from(pid)
}
impl Process {
    pub const fn identity(&self) -> &Identity {
        &self.identity
    }
    pub(super) fn observe(identity: &Identity) -> Result<Self, Error> {
        if !cfg!(target_os = "macos") {
            return Err(Error::Unknown);
        }
        let mut found = None;
        for domain in [Domain::Gui, Domain::User] {
            let unit = domain.unit(identity);
            let output = run(&command(), &["print".as_ref(), unit.as_ref()])?;
            match output.status.code() {
                Some(0) if found.is_none() => {
                    found = Some(Self {
                        identity: identity.clone(),
                        pid: pid(&output.stdout)?,
                        domain,
                    });
                }
                Some(113) if output.stdout.is_empty() => (),
                _ => return Err(Error::Unknown),
            }
        }
        found.ok_or(Error::Unknown)
    }
    /// Missing, replaced or ambiguous native runtime ownership is never permission to exec again.
    pub fn recheck(&self) -> Result<(), Error> {
        if Self::observe(&self.identity)? != *self {
            return Err(Error::Unknown);
        }
        Ok(())
    }
    pub fn gone(&self) -> Result<bool, Error> {
        if !cfg!(target_os = "macos") {
            return Err(Error::Unknown);
        }
        let unit = self.domain.unit(&self.identity);
        let output = run(&command(), &["print".as_ref(), unit.as_ref()])?;
        match output.status.code() {
            Some(0) => return Ok(false),
            Some(113) if output.stdout.is_empty() => (),
            _ => return Err(Error::Unknown),
        }
        let raw = i32::try_from(self.pid.get()).map_err(|_| Error::Unknown)?;
        let pid = rustix::process::Pid::from_raw(raw).ok_or(Error::Unknown)?;
        match rustix::process::test_kill_process(pid) {
            Err(rustix::io::Errno::SRCH) => Ok(true),
            Ok(()) => Ok(false),
            Err(error) => Err(std::io::Error::from(error).into()),
        }
    }
}
#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn durable_native_process_evidence_cannot_select_system_domain_or_process_groups() {
        let original = serde_json::json!({"identity":{"name":"wrix-worker-native","born":800_000_001.5},"pid":123,"domain":"gui"});
        let parsed: Process = serde_json::from_value(original.clone()).unwrap();
        assert_eq!(parsed.identity().name(), "wrix-worker-native");
        for (field, replacement) in [
            ("pid", serde_json::json!(0)),
            ("pid", serde_json::json!(4_294_967_295_u32)),
            ("domain", serde_json::json!("system")),
        ] {
            let mut changed = original.clone();
            changed[field] = replacement;
            assert!(serde_json::from_value::<Process>(changed).is_err());
        }
    }
    #[test]
    fn native_launchd_process_parser_rejects_ambiguous_or_invalid_pids() {
        assert_eq!(
            pid(b"gui/501/unit = {\n pid = 123\n}\n").unwrap().get(),
            123
        );
        for bytes in [
            b"state = stopped".as_slice(),
            b"pid = 0",
            b"pid = 123\npid = 456",
            b"pid = -1",
            b"pid = 4294967295",
        ] {
            assert!(pid(bytes).is_err());
        }
    }
}
