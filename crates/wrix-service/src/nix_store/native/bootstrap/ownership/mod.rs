//! Numeric ownership applies only to the selected host data mount, never to guest root or Nix state.

use crate::nix_store::native::{DATA, Error};
use std::{
    fs::{self, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

const BINDING: &str = "/run/wrix-nix-pool-data-owner";

pub const fn validate(uid: u32, gid: u32) -> Result<(), Error> {
    if matches!(uid, 74 | 2000 | 2001 | 30000..=30099 | u32::MAX) || gid == u32::MAX {
        return Err(Error::Invalid);
    }
    Ok(())
}

fn parse(value: &str) -> Result<(u32, u32), Error> {
    let (uid, gid) = value
        .strip_suffix('\n')
        .ok_or(Error::Invalid)?
        .split_once(':')
        .ok_or(Error::Invalid)?;
    let uid: u32 = uid.parse().map_err(|_| Error::Invalid)?;
    let gid: u32 = gid.parse().map_err(|_| Error::Invalid)?;
    if value != format!("{uid}:{gid}\n") {
        return Err(Error::Invalid);
    }
    validate(uid, gid)?;
    Ok((uid, gid))
}

pub fn owner() -> Result<(u32, u32), Error> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(BINDING)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.gid() != 0
        || metadata.nlink() != 1
        || metadata.mode() & 0o777 != 0o600
        || metadata.len() > 32
    {
        return Err(Error::Invalid);
    }
    let mut bytes = String::new();
    file.take(33).read_to_string(&mut bytes)?;
    let owner = parse(&bytes)?;
    let metadata = fs::symlink_metadata(DATA)?;
    if !metadata.is_dir()
        || metadata.uid() != owner.0
        || metadata.gid() != owner.1
        || metadata.mode() & 0o777 != 0o700
        || Path::new(DATA).canonicalize()? != Path::new(DATA)
    {
        return Err(Error::Invalid);
    }
    if owner != (0, 0) {
        let mounts = fs::read_to_string("/proc/self/mountinfo")?;
        if !mounts.lines().any(|line| {
            line.split_whitespace().nth(4) == Some(DATA)
                && line
                    .split_once(" - ")
                    .is_some_and(|(_, tail)| tail.starts_with("virtiofs "))
        }) {
            return Err(Error::Invalid);
        }
    }
    Ok(owner)
}

pub fn uid(path: &Path) -> Result<u32, Error> {
    if path.starts_with(DATA) {
        Ok(owner()?.0)
    } else {
        Ok(0)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn data_identity_is_canonical_and_cannot_select_guest_root_roles() {
        assert_eq!(parse("501:20\n").unwrap(), (501, 20));
        assert_eq!(parse("0:0\n").unwrap(), (0, 0));
        for invalid in [
            "0501:20\n",
            "+501:20\n",
            "501:20",
            "501:20\n0:0\n",
            "4294967295:20\n",
            "2000:20\n",
            "2001:20\n",
            "30001:20\n",
            "74:20\n",
        ] {
            assert!(parse(invalid).is_err());
        }
        assert_eq!(uid(Path::new("/nix/var/nix/gc.lock")).unwrap(), 0);
        assert_eq!(
            uid(Path::new("/run/wrix-nix-pool/control.sock")).unwrap(),
            0
        );
    }
}
