use std::{
    ffi::OsString,
    fs::{self, File},
    io::{self, Read, Write},
    path::Path,
    process::Command,
};
#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// usage: wrix-worker-uts --unshare ABSOLUTE-EXECUTABLE | inside
    Usage,
    /// worker UTS probe requires rootless UID zero, seccomp filtering and absent `SYS_ADMIN/NET_ADMIN` capabilities
    Boundary,
    /// worker namespace identity unexpectedly changed or aliases its host
    Namespace,
    /// worker identity syscall did not retain its required success or EPERM result
    Identity,
    /// private user+UTS namespace subprocess failed
    Child,
    /// worker UTS observation failed: {0}
    Io(#[from] io::Error),
    /// worker UTS syscall failed: {0}
    Syscall(#[from] rustix::io::Errno),
}
fn read(path: &Path) -> Result<String, Error> {
    let mut bytes = Vec::new();
    File::open(path)?.take(8193).read_to_end(&mut bytes)?;
    if bytes.len() > 8192 {
        return Err(Error::Boundary);
    }
    String::from_utf8(bytes).map_err(|_| Error::Boundary)
}
fn field<'a>(status: &'a str, name: &str) -> Result<&'a str, Error> {
    let mut values = status
        .lines()
        .filter_map(|line| line.split_once(':'))
        .filter(|(key, _)| *key == name)
        .map(|(_, value)| value.trim());
    let value = values.next().ok_or(Error::Boundary)?;
    if values.next().is_some() {
        return Err(Error::Boundary);
    }
    Ok(value)
}
fn mappings(bytes: &str) -> Result<Vec<[u64; 3]>, Error> {
    bytes
        .lines()
        .map(|line| {
            let values = line
                .split_whitespace()
                .map(str::parse::<u64>)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| Error::Boundary)?;
            let [inside, outside, count] = values.as_slice() else {
                return Err(Error::Boundary);
            };
            if *count == 0 {
                return Err(Error::Boundary);
            }
            Ok([*inside, *outside, *count])
        })
        .collect()
}
fn boundary(status: &str, maps: &[[u64; 3]], uid: u32) -> Result<(), Error> {
    if uid != 0
        || field(status, "Seccomp")? != "2"
        || maps.first().is_none_or(|row| row[0] != 0)
        || maps.iter().any(|row| row[1] == 0)
    {
        return Err(Error::Boundary);
    }
    for name in ["CapInh", "CapPrm", "CapEff", "CapBnd", "CapAmb"] {
        let bits = u64::from_str_radix(field(status, name)?, 16).map_err(|_| Error::Boundary)?;
        if bits & ((1 << 21) | (1 << 12)) != 0 {
            return Err(Error::Boundary);
        }
    }
    Ok(())
}
#[derive(Clone, Copy)]
enum Permission {
    Allowed,
    Denied,
}
fn identity(permission: Permission) -> Result<(), Error> {
    for result in [
        rustix::system::sethostname(b"localhost"),
        rustix::system::setdomainname(b"(none)"),
    ] {
        match (permission, result) {
            (Permission::Allowed, Ok(())) | (Permission::Denied, Err(rustix::io::Errno::PERM)) => {}
            _ => return Err(Error::Identity),
        }
    }
    Ok(())
}
fn snapshot() -> [Vec<u8>; 5] {
    let identity = rustix::system::uname();
    [
        identity.sysname(),
        identity.nodename(),
        identity.release(),
        identity.version(),
        identity.machine(),
    ]
    .map(|value| value.to_bytes().to_vec())
}
fn inside() -> Result<(), Error> {
    if rustix::process::getuid().as_raw() != 0
        || mappings(&read(Path::new("/proc/self/uid_map"))?)? != [[0, 0, 1]]
    {
        return Err(Error::Boundary);
    }
    identity(Permission::Allowed)?;
    let observed = rustix::system::uname();
    if observed.nodename().to_bytes() != b"localhost"
        || observed.domainname().to_bytes() != b"(none)"
    {
        return Err(Error::Identity);
    }
    writeln!(
        io::stdout().lock(),
        "private user+UTS: both identity syscalls succeeded"
    )?;
    Ok(())
}
pub fn run(args: &[OsString]) -> Result<(), Error> {
    if args == [OsString::from("inside")] {
        return inside();
    }
    let [flag, unshare] = args else {
        return Err(Error::Usage);
    };
    if flag != "--unshare" || !Path::new(unshare).is_absolute() {
        return Err(Error::Usage);
    }
    let maps = mappings(&read(Path::new("/proc/self/uid_map"))?)?;
    boundary(
        &read(Path::new("/proc/self/status"))?,
        &maps,
        rustix::process::getuid().as_raw(),
    )?;
    let uts = fs::read_link("/proc/self/ns/uts")?;
    if Some(uts.as_os_str()) == std::env::var_os("TEST_HOST_UTS").as_deref()
        || std::env::var_os("TEST_HOST_UTS").is_none()
    {
        return Err(Error::Namespace);
    }
    writeln!(io::stdout().lock(), "rootless UID mapping: {maps:?}")?;
    let before = snapshot();
    identity(Permission::Denied)?;
    if rustix::system::init_module(&[], c"") != Err(rustix::io::Errno::PERM) {
        return Err(Error::Boundary);
    }
    if !rustix::process::getgroups()?.is_empty() {
        if fs::read_dir("/proc/self/task")?.count() != 1 {
            return Err(Error::Boundary);
        }
        rustix::thread::set_thread_groups(&[])?;
    }
    if !Command::new(unshare)
        .args(["--user", "--map-root-user", "--uts"])
        .arg(std::env::current_exe()?)
        .arg("inside")
        .status()?
        .success()
    {
        return Err(Error::Child);
    }
    if snapshot() != before || fs::read_link("/proc/self/ns/uts")? != uts {
        return Err(Error::Namespace);
    }
    identity(Permission::Denied)?;
    writeln!(
        io::stdout().lock(),
        "worker UTS unchanged; seccomp active; SYS_ADMIN/NET_ADMIN absent from all capability sets"
    )?;
    Ok(())
}
#[cfg(test)]
mod test;
