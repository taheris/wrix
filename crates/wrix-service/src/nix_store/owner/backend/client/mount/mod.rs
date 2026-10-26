//! Native mount identity, not a launcher exit or a mount-path description.
use super::{Error, lease};
use serde::{Deserialize, Serialize};
use std::{fs::File, io::Read, os::unix::ffi::OsStrExt, path::Path};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Identity {
    id: std::num::NonZeroU64,
    device: (u32, u32),
}

impl Identity {
    pub(super) fn same_filesystem(&self, expected: &Self) -> bool {
        self.device == expected.device
    }
}

fn decode(value: &[u8]) -> Result<Vec<u8>, Error> {
    let mut result = Vec::new();
    let mut cursor = 0;
    while cursor < value.len() {
        if value[cursor] == b'\\' {
            let octal = value.get(cursor + 1..cursor + 4).ok_or(Error::Invalid)?;
            if !octal.iter().all(|byte| (b'0'..=b'7').contains(byte)) {
                return Err(Error::Invalid);
            }
            let number = u16::from(octal[0] - b'0') * 64
                + u16::from(octal[1] - b'0') * 8
                + u16::from(octal[2] - b'0');
            result.push(u8::try_from(number).map_err(|_| Error::Invalid)?);
            cursor += 4;
        } else {
            result.push(value[cursor]);
            cursor += 1;
        }
    }
    Ok(result)
}

fn parse(bytes: &[u8], path: &Path, lease: &lease::Id) -> Result<Option<Identity>, Error> {
    let mut identity = None;
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let fields: Vec<_> = line.split(|byte| *byte == b' ').collect();
        if fields.len() < 10 {
            return Err(Error::Invalid);
        }
        if decode(fields[4])? != path.as_os_str().as_bytes() {
            continue;
        }
        let separator = fields
            .iter()
            .position(|field| *field == b"-")
            .ok_or(Error::Invalid)?;
        if separator < 6
            || fields[3] != b"/"
            || fields.len() != separator + 4
            || fields[separator + 1] != b"fuse.sshfs"
            || fields[separator + 2] != format!("wrix-nix-{}", lease.as_str()).as_bytes()
            || ![b"ro".as_slice(), b"nodev".as_slice(), b"nosuid".as_slice()]
                .iter()
                .all(|required| {
                    fields[5]
                        .split(|byte| *byte == b',')
                        .any(|part| part == *required)
                })
            || identity.is_some()
        {
            return Err(Error::Invalid);
        }
        let id = std::str::from_utf8(fields[0])
            .map_err(|_| Error::Invalid)?
            .parse()
            .map_err(|_| Error::Invalid)?;
        let device = std::str::from_utf8(fields[2]).map_err(|_| Error::Invalid)?;
        let (major, minor) = device.split_once(':').ok_or(Error::Invalid)?;
        let device = (
            major.parse().map_err(|_| Error::Invalid)?,
            minor.parse().map_err(|_| Error::Invalid)?,
        );
        identity = Some(Identity { id, device });
    }
    Ok(identity)
}

pub(super) fn observe(
    plane: wrix_state::nix_store::admission::request::Plane,
    tools: &super::super::Tools,
    path: &Path,
    lease: &lease::Id,
) -> Result<Option<Identity>, Error> {
    let bytes = match plane {
        wrix_state::nix_store::admission::request::Plane::Host => {
            let mut bytes = Vec::new();
            File::open("/proc/self/mountinfo")?
                .take(4_194_305)
                .read_to_end(&mut bytes)?;
            bytes
        }
        wrix_state::nix_store::admission::request::Plane::Podman
        | wrix_state::nix_store::admission::request::Plane::Krun => {
            if observe(
                wrix_state::nix_store::admission::request::Plane::Host,
                tools,
                path,
                lease,
            )?
            .is_some()
            {
                return Err(Error::Retained);
            }
            let mut command = tools.view_command(plane, super::Program::Cat);
            command.arg("/proc/self/mountinfo");
            super::super::success(tools.run(command, None, Some(super::COMMAND_BUDGET))?)?
        }
    };
    if bytes.len() > 4_194_304 {
        return Err(Error::Invalid);
    }
    parse(&bytes, path, lease)
}

pub(super) fn readable(
    plane: wrix_state::nix_store::admission::request::Plane,
    tools: &super::super::Tools,
    path: &Path,
) -> Result<bool, Error> {
    match plane {
        wrix_state::nix_store::admission::request::Plane::Host => {
            Ok(std::fs::read_dir(path)?.next().transpose()?.is_some())
        }
        wrix_state::nix_store::admission::request::Plane::Podman
        | wrix_state::nix_store::admission::request::Plane::Krun => {
            let mut command = tools.view_command(plane, super::Program::Ls);
            command.args(["-A", "--"]).arg(path);
            Ok(
                !super::super::success(tools.run(command, None, Some(super::COMMAND_BUDGET))?)?
                    .is_empty(),
            )
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn retirement_identifies_only_full_native_references_to_the_same_export() {
        let lease = lease::Id::try_from("a".repeat(32)).unwrap();
        let line = format!(
            "90 2 0:99 / /view ro,nosuid,nodev - fuse.sshfs wrix-nix-{} ro\n",
            lease.as_str()
        );
        let original = parse(line.as_bytes(), Path::new("/view"), &lease)
            .unwrap()
            .unwrap();
        let replica = parse(
            line.replacen("90 ", "91 ", 1).as_bytes(),
            Path::new("/view"),
            &lease,
        )
        .unwrap()
        .unwrap();
        assert_ne!(original, replica);
        assert!(replica.same_filesystem(&original));
        let foreign = parse(
            line.replace("0:99", "0:100").as_bytes(),
            Path::new("/view"),
            &lease,
        )
        .unwrap()
        .unwrap();
        assert!(!foreign.same_filesystem(&original));
        assert!(
            parse(
                line.replace("0:99 / ", "0:99 /subdirectory ").as_bytes(),
                Path::new("/view"),
                &lease
            )
            .is_err()
        );
    }

    #[test]
    fn mount_identity_requires_the_exact_read_only_native_source_not_just_a_path() {
        let id = lease::Id::try_from("a".repeat(32)).unwrap();
        let line = format!(
            "90 2 0:99 / /view\\040space ro,nosuid,nodev - fuse.sshfs wrix-nix-{} ro\n",
            id.as_str()
        );
        assert!(
            parse(line.as_bytes(), Path::new("/view space"), &id)
                .unwrap()
                .is_some()
        );
        assert!(
            parse(
                line.replace(" ro,nosuid,nodev ", " rw,nosuid,nodev ")
                    .as_bytes(),
                Path::new("/view space"),
                &id
            )
            .is_err()
        );
        assert!(
            parse(
                line.replace("fuse.sshfs", "ext4").as_bytes(),
                Path::new("/view space"),
                &id
            )
            .is_err()
        );
        assert!(
            parse(
                line.replace("wrix-nix-", "peer-").as_bytes(),
                Path::new("/view space"),
                &id
            )
            .is_err()
        );
        assert!(
            parse(
                format!("{line}{line}").as_bytes(),
                Path::new("/view space"),
                &id
            )
            .is_err()
        );
    }
}
