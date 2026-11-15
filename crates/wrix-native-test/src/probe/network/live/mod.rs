use super::{Hash, Private, Public, counter::Count, server::Server};
use crate::{native_fixture::command, probe::cgroup::Bootstrap};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
const NS: &str = "wrix-native-build";
const NETNS: &str = "/var/run/netns";
const HOST: &str = "wxpfhost";
const PEER: &str = "wxpfbuild";
const NAT: &str = "wrix_npfo4_nat";
const PARENT: &str = "10.231.47.1";
const PUBLIC: &str = "https://cache.nixos.org/nix-cache-info";
#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// network primitive requires a qualified idle root fixture and private mount/proc context
    Boundary,
    /// network primitive I/O failed: {0}
    Io(#[from] io::Error),
    /// network primitive descriptor failed: {0}
    Descriptor(#[from] rustix::io::Errno),
    /// network primitive command failed: {0}
    Command(#[from] command::Error),
    /// network primitive TLS observation failed: {0}
    Network(#[from] super::Error),
    /// network primitive packet observation failed: {0}
    Counter(#[from] super::counter::Error),
    /// network primitive JSON observation failed: {0}
    Json(#[from] serde_json::Error),
    /// network primitive resource ownership became uncertain
    Ownership,
    /// network fixture resource creation lacked confirmed ownership: {0}
    Unconfirmed(&'static str),
    /// network primitive positive or negative control failed
    Evidence,
    /// network primitive failed ({primary:?}) and cleanup remained uncertain: {issues:?}
    Cleanup {
        primary: Option<Box<Self>>,
        issues: Vec<String>,
    },
}
struct Identity(File);
impl Identity {
    fn pin(path: &Path) -> Result<Self, Error> {
        Ok(Self(
            OpenOptions::new()
                .read(true)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
                .open(path)?,
        ))
    }
    fn check(&self, path: &Path) -> Result<(), Error> {
        let pinned = self.0.metadata()?;
        let observed = fs::symlink_metadata(path)?;
        if (pinned.dev(), pinned.ino()) != (observed.dev(), observed.ino()) {
            return Err(Error::Ownership);
        }
        Ok(())
    }
}
struct Tools {
    ip: PathBuf,
    nft: PathBuf,
    setpriv: PathBuf,
    ca: PathBuf,
}
fn store_file(path: &Path) -> Result<PathBuf, Error> {
    let canonical = fs::canonicalize(path)?;
    let metadata = fs::metadata(&canonical)?;
    if !path.is_absolute()
        || !path.starts_with("/nix/store")
        || !canonical.starts_with("/nix/store")
        || !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
    {
        return Err(Error::Boundary);
    }
    Ok(canonical)
}
fn executable(path: &Path) -> Result<PathBuf, Error> {
    let canonical = store_file(path)?;
    if fs::metadata(&canonical)?.mode() & 0o111 == 0 {
        return Err(Error::Boundary);
    }
    Ok(canonical)
}
fn absent(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => Err(Error::Boundary),
    }
}
fn admit(proc: &Path) -> Result<(), Error> {
    if !rustix::process::geteuid().is_root()
        || !proc.is_absolute()
        || fs::canonicalize(proc)? != proc
        || rustix::fs::statfs(proc)?.f_type != 0x9fa0
        || fs::read_link("/proc/self/ns/mnt")? == fs::read_link("/proc/1/ns/mnt")?
    {
        return Err(Error::Boundary);
    }
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name
            .to_str()
            .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        else {
            continue;
        };
        match fs::read(entry.path().join("status")) {
            Ok(status) => {
                Bootstrap::parse(name, &status).map_err(|_| Error::Boundary)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    for path in [
        PathBuf::from("/run/netns").join(NS),
        PathBuf::from(NETNS).join(NS),
        PathBuf::from("/sys/class/net").join(HOST),
        PathBuf::from("/sys/class/net").join(PEER),
        PathBuf::from("/etc/netns").join(NS),
    ] {
        absent(&path)?;
    }
    Ok(())
}
enum Undo {
    Namespace(Identity),
    Link(u32),
    Nat(u64),
    Forward(Vec<u8>),
    Directory(PathBuf, Identity),
    File(PathBuf, Identity),
    Server(Server),
    Uncertain(&'static str),
}
struct Run {
    tools: Tools,
    proc: PathBuf,
    namespace: PathBuf,
    net_identity: PathBuf,
    files: tempfile::TempDir,
    undo: Vec<Undo>,
}
impl Run {
    fn command(binary: &Path, args: &[&std::ffi::OsStr]) -> Result<Vec<u8>, Error> {
        Ok(command::checked(
            Command::new(binary).args(args),
            Duration::from_secs(25),
        )?)
    }
    fn ip(&self, args: &[&str]) -> Result<Vec<u8>, Error> {
        let args: Vec<_> = args.iter().map(std::ffi::OsStr::new).collect();
        Self::command(&self.tools.ip, &args)
    }
    fn nft(&self, args: &[&str]) -> Result<Vec<u8>, Error> {
        let args: Vec<_> = args.iter().map(std::ffi::OsStr::new).collect();
        Self::command(&self.tools.nft, &args)
    }
    fn config(&self, name: &str, bytes: &[u8]) -> Result<PathBuf, Error> {
        let path = self.files.path().join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(path)
    }
    fn table_handle(&self) -> Result<u64, Error> {
        let bytes = self.nft(&["-a", "-j", "list", "table", "ip", NAT])?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        let records = value
            .get("nftables")
            .and_then(serde_json::Value::as_array)
            .ok_or(Error::Evidence)?;
        let mut handles = records
            .iter()
            .filter_map(|record| record.get("table"))
            .filter(|table| {
                table.get("family").and_then(serde_json::Value::as_str) == Some("ip")
                    && table.get("name").and_then(serde_json::Value::as_str) == Some(NAT)
            })
            .map(|table| {
                table
                    .get("handle")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or(Error::Evidence)
            });
        let handle = handles.next().ok_or(Error::Evidence)??;
        if handles.next().is_some() {
            return Err(Error::Evidence);
        }
        Ok(handle)
    }
    fn confirm(&mut self, confirmed: Undo) -> Result<(), Error> {
        let action = self.undo.last_mut().ok_or(Error::Ownership)?;
        if !matches!(action, Undo::Uncertain(_)) {
            return Err(Error::Ownership);
        }
        *action = confirmed;
        Ok(())
    }
    fn parent_directory(&mut self, path: &Path) -> Result<(), Error> {
        match fs::symlink_metadata(path) {
            Ok(metadata)
                if metadata.is_dir() && metadata.uid() == 0 && metadata.mode() & 0o022 == 0 =>
            {
                Ok(())
            }
            Ok(_) => Err(Error::Boundary),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.undo
                    .push(Undo::Uncertain("parent directory lacks pinned identity"));
                DirBuilder::new().mode(0o755).create(path)?;
                let identity = Identity::pin(path)?;
                self.confirm(Undo::Directory(path.to_owned(), identity))
            }
            Err(error) => Err(error.into()),
        }
    }
    fn prepare(&mut self) -> Result<(), Error> {
        self.parent_directory(Path::new(NETNS))?;
        self.undo
            .push(Undo::Uncertain("namespace creation lacks pinned receipt"));
        self.ip(&["netns", "add", NS])?;
        let identity = Identity::pin(&self.namespace)?;
        self.confirm(Undo::Namespace(identity))?;
        self.undo
            .push(Undo::Uncertain("veth creation lacks ifindex receipt"));
        self.ip(&["link", "add", HOST, "type", "veth", "peer", "name", PEER])?;
        let index = fs::read_to_string(Path::new("/sys/class/net").join(HOST).join("ifindex"))?
            .trim()
            .parse::<u32>()
            .map_err(|_| Error::Ownership)?;
        if index == 0 {
            return Err(Error::Ownership);
        }
        self.confirm(Undo::Link(index))?;
        for args in [
            vec!["link", "set", PEER, "netns", NS],
            vec!["addr", "add", "10.231.47.1/30", "dev", HOST],
            vec!["link", "set", HOST, "up"],
            vec!["-n", NS, "addr", "add", "10.231.47.2/30", "dev", PEER],
            vec!["-n", NS, "link", "set", PEER, "up"],
            vec!["-n", NS, "link", "set", "lo", "up"],
            vec!["-n", NS, "route", "add", "default", "via", PARENT],
        ] {
            self.ip(&args)?;
        }
        let forward = self.proc.join("sys/net/ipv4/ip_forward");
        let before = fs::read(&forward)?;
        if !matches!(before.as_slice(), b"0\n" | b"1\n") {
            return Err(Error::Boundary);
        }
        self.undo.push(Undo::Forward(before));
        fs::write(forward, b"1\n")?;
        self.undo
            .push(Undo::Uncertain("NAT creation lacks kernel handle"));
        self.nft(&["create", "table", "ip", NAT])?;
        let handle = self.table_handle()?;
        self.confirm(Undo::Nat(handle))?;
        let nat = self.config("nat.nft", include_bytes!("../nat.nft"))?;
        Self::command(&self.tools.nft, &["-f".as_ref(), nat.as_os_str()])?;
        let parent = Path::new("/etc/netns");
        self.parent_directory(parent)?;
        let resolv = parent.join(NS);
        self.undo
            .push(Undo::Uncertain("resolver directory lacks pinned identity"));
        DirBuilder::new().mode(0o755).create(&resolv)?;
        let identity = Identity::pin(&resolv)?;
        self.confirm(Undo::Directory(resolv.clone(), identity))?;
        let path = resolv.join("resolv.conf");
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&path)?;
        self.undo.push(Undo::File(path, Identity(file)));
        let Some(Undo::File(_, Identity(file))) = self.undo.last_mut() else {
            return Err(Error::Ownership);
        };
        file.write_all(b"nameserver 1.1.1.1\n")?;
        file.sync_all()?;
        let server = Server::start("10.231.47.1:24823".parse().map_err(|_| Error::Boundary)?)?;
        if server.address().ip().to_string() != PARENT || server.address().port() != 24823 {
            return Err(Error::Evidence);
        }
        self.undo.push(Undo::Server(server));
        Ok(())
    }
    fn observe(&self, expected: &Hash) -> Result<Count, Error> {
        let policy = self.config("egress.nft", include_bytes!("../egress.nft"))?;
        Self::command(
            &self.tools.ip,
            &[
                "netns".as_ref(),
                "exec".as_ref(),
                NS.as_ref(),
                self.tools.nft.as_os_str(),
                "-f".as_ref(),
                policy.as_os_str(),
            ],
        )?;
        let output = command::checked(
            Command::new(&self.tools.ip)
                .env_clear()
                .args(["netns", "exec", NS])
                .arg(&self.tools.setpriv)
                .args([
                    "--reuid=30001",
                    "--regid=30000",
                    "--clear-groups",
                    "--bounding-set=-all",
                    "--inh-caps=-all",
                    "--ambient-caps=-all",
                    "--no-new-privs",
                ])
                .arg(std::env::current_exe()?)
                .args(["operator-probe", "network-worker", "direct"])
                .arg(&self.tools.ca)
                .arg(PUBLIC)
                .arg("http://10.231.47.1:24823")
                .arg(&expected.0),
            Duration::from_secs(40),
        )?;
        if !output
            .windows(b"NATIVE_BUILD_NETWORK_PUBLIC_TLS_ALLOWED_PRIVATE_SERVICE_BLOCKED".len())
            .any(|part| part == b"NATIVE_BUILD_NETWORK_PUBLIC_TLS_ALLOWED_PRIVATE_SERVICE_BLOCKED")
        {
            return Err(Error::Evidence);
        }
        let rules = Self::command(
            &self.tools.ip,
            &[
                "netns".as_ref(),
                "exec".as_ref(),
                NS.as_ref(),
                self.tools.nft.as_os_str(),
                "-j".as_ref(),
                "list".as_ref(),
                "ruleset".as_ref(),
            ],
        )?;
        Ok(Count::parse(&rules)?)
    }
    fn cleanup(&mut self) -> Vec<String> {
        let mut issues = Vec::new();
        while let Some(action) = self.undo.pop() {
            let result = (|| {
                if fs::read_link("/proc/self/ns/net")? != self.net_identity {
                    return Err(Error::Ownership);
                }
                match action {
                    Undo::Namespace(identity) => {
                        identity.check(&self.namespace)?;
                        self.ip(&["netns", "delete", NS])?;
                    }
                    Undo::Link(expected) => {
                        let current = fs::read_to_string(
                            Path::new("/sys/class/net").join(HOST).join("ifindex"),
                        )?
                        .trim()
                        .parse::<u32>()
                        .map_err(|_| Error::Ownership)?;
                        if current != expected {
                            return Err(Error::Ownership);
                        }
                        self.ip(&["link", "delete", HOST])?;
                    }
                    Undo::Nat(expected) => {
                        if self.table_handle()? != expected {
                            return Err(Error::Ownership);
                        }
                        self.nft(&["delete", "table", "ip", NAT])?;
                    }
                    Undo::Forward(before) => {
                        let path = self.proc.join("sys/net/ipv4/ip_forward");
                        if fs::read(&path)? != b"1\n" {
                            return Err(Error::Ownership);
                        }
                        fs::write(path, before)?;
                    }
                    Undo::Directory(path, identity) => {
                        identity.check(&path)?;
                        fs::remove_dir(path)?;
                    }
                    Undo::File(path, identity) => {
                        identity.check(&path)?;
                        fs::remove_file(path)?;
                    }
                    Undo::Server(server) => server.finish()?,
                    Undo::Uncertain(description) => {
                        return Err(Error::Unconfirmed(description));
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                issues.push(error.to_string());
            }
        }
        issues
    }
}
#[derive(serde::Serialize)]
enum Scope {
    #[serde(
        rename = "native unprivileged build-network primitive, not a complete Nix FOD or unified-service acceptance"
    )]
    Primitive,
}
#[derive(serde::Serialize)]
pub struct Receipt {
    scope: Scope,
    uid: u32,
    public_payload_sha256: String,
    private_service_drop_packets: u64,
}
#[cfg(test)]
mod test;
pub fn run(
    ip: &Path,
    nft: &Path,
    setpriv: &Path,
    ca: &Path,
    proc: &Path,
) -> Result<Receipt, Error> {
    admit(proc)?;
    let ca = store_file(ca)?;
    let tools = Tools {
        ip: executable(ip)?,
        nft: executable(nft)?,
        setpriv: executable(setpriv)?,
        ca,
    };
    let public = Public::parse(PUBLIC)?;
    let client = super::client(&tools.ca)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let bytes = runtime.block_on(super::fetch(&client, &public.0, Duration::from_secs(20)))?;
    if !bytes
        .windows(b"StoreDir: /nix/store".len())
        .any(|part| part == b"StoreDir: /nix/store")
    {
        return Err(Error::Evidence);
    }
    let expected = Hash::parse(&format!("{:x}", Sha256::digest(bytes)))?;
    let files = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .prefix("wrix-network-primitive-")
        .tempdir()?;
    let mut run = Run {
        tools,
        proc: proc.to_owned(),
        namespace: Path::new(NETNS).join(NS),
        net_identity: fs::read_link("/proc/self/ns/net")?,
        files,
        undo: Vec::new(),
    };
    let result = (|| {
        run.prepare()?;
        let private = Private::parse("http://10.231.47.1:24823")?;
        if runtime.block_on(super::fetch(&client, &private.0, Duration::from_secs(2)))?
            != b"private-core-fixture"
        {
            return Err(Error::Evidence);
        }
        run.observe(&expected)
    })();
    let issues = run.cleanup();
    if !issues.is_empty() {
        return Err(Error::Cleanup {
            primary: result.err().map(Box::new),
            issues,
        });
    }
    let count = result?;
    Ok(Receipt {
        scope: Scope::Primitive,
        uid: 30_001,
        public_payload_sha256: expected.0,
        private_service_drop_packets: count.packets(),
    })
}
