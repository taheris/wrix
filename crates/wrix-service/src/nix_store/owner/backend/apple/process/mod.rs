//! Native VM teardown requires deregistration and absence of its original host process.

use super::{COMMAND_BUDGET, Error, NativeId, Program, Tools};
use serde::{Deserialize, Serialize};
use std::{ffi::OsString, io, num::NonZeroU32};

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Witness {
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
            return Err(Error::Invalid);
        }
        Ok(Self(NonZeroU32::new(value).ok_or(Error::Invalid)?))
    }
}
impl From<Pid> for u32 {
    fn from(value: Pid) -> Self {
        value.0.get()
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Domain {
    Gui,
    User,
}
impl Domain {
    fn unit(self, id: &NativeId) -> String {
        let domain = match self {
            Self::Gui => "gui",
            Self::User => "user",
        };
        format!(
            "{domain}/{}/com.apple.container.container-runtime-linux.{}",
            rustix::process::geteuid().as_raw(),
            id.0
        )
    }
}

fn parse(bytes: &[u8]) -> Result<Pid, Error> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::Invalid)?;
    let mut values = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("pid = "));
    let pid = values
        .next()
        .ok_or(Error::Retained)?
        .parse::<u32>()
        .map_err(|_| Error::Invalid)?;
    if values.next().is_some() {
        return Err(Error::Invalid);
    }
    pid.try_into()
}

fn registered(tools: &Tools, domain: Domain, id: &NativeId) -> Result<Option<Pid>, Error> {
    let mut command = tools.command(Program::Launchctl);
    command.args([OsString::from("print"), domain.unit(id).into()]);
    let output = tools.run(command, None, Some(COMMAND_BUDGET))?;
    match output.status.code() {
        Some(0) => Ok(Some(parse(&output.stdout)?)),
        Some(113) if output.stdout.is_empty() => Ok(None),
        _ => Err(Error::Retained),
    }
}

impl Witness {
    pub(super) fn observe(tools: &Tools, id: &NativeId) -> Result<Self, Error> {
        let mut found = None;
        for domain in [Domain::Gui, Domain::User] {
            if let Some(pid) = registered(tools, domain, id)? {
                if found.is_some() {
                    return Err(Error::Retained);
                }
                found = Some(Self { pid, domain });
            }
        }
        found.ok_or(Error::Retained)
    }

    pub(super) fn running(&self, tools: &Tools, id: &NativeId) -> Result<(), Error> {
        if Self::observe(tools, id)? != *self {
            return Err(Error::Retained);
        }
        let pid = rustix::process::Pid::from_raw(
            i32::try_from(self.pid.0.get()).map_err(|_| Error::Invalid)?,
        )
        .ok_or(Error::Invalid)?;
        rustix::process::test_kill_process(pid).map_err(io::Error::from)?;
        Ok(())
    }

    pub(super) fn gone(&self, tools: &Tools, id: &NativeId) -> Result<bool, Error> {
        for domain in [Domain::Gui, Domain::User] {
            if registered(tools, domain, id)?.is_some() {
                return Ok(false);
            }
        }
        let pid = rustix::process::Pid::from_raw(
            i32::try_from(self.pid.0.get()).map_err(|_| Error::Invalid)?,
        )
        .ok_or(Error::Invalid)?;
        match rustix::process::test_kill_process(pid) {
            Err(rustix::io::Errno::SRCH) => Ok(true),
            Ok(()) => Ok(false),
            Err(error) => Err(io::Error::from(error).into()),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn runtime_pid_proof_rejects_missing_zero_negative_ambiguous_and_process_group_values() {
        assert_eq!(
            parse(b"gui/501/service = {\n pid = 123\n}")
                .unwrap()
                .0
                .get(),
            123
        );
        for bytes in [
            b"state = stopped".as_slice(),
            b"pid = 0",
            b"pid = -1",
            b"pid = 4294967295",
            b"pid = 123\npid = 456",
        ] {
            assert!(parse(bytes).is_err());
        }
    }
    #[test]
    fn durable_runtime_proof_cannot_name_system_domain_or_arbitrary_process_group() {
        let value = serde_json::json!({"pid":123,"domain":"gui"});
        serde_json::from_value::<Witness>(value.clone()).unwrap();
        for (field, replacement) in [
            ("pid", serde_json::json!(0)),
            ("pid", serde_json::json!(4_294_967_295_u32)),
            ("domain", serde_json::json!("system")),
            ("unit", serde_json::json!("unrelated-service")),
        ] {
            let mut invalid = value.clone();
            invalid[field] = replacement;
            assert!(serde_json::from_value::<Witness>(invalid).is_err());
        }
    }
}
