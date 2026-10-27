//! Shared physical deployment registration and namespace cutover.

use super::{
    DoltEndpoint, DoltSocketPath, DoltTransport, Error, Plan, Result, Runtime, RuntimeStatus,
    coordination,
};
use crate::{
    control::Authorization,
    dolt::{Credentials, Manager, broker::Endpoint},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read},
    num::NonZeroU16,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[cfg(test)]
mod test;

pub(super) const LABEL: &str = "wrix.dolt.shared";
pub(super) const VERSION: &str = "native-v1";

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    version: u8,
    tcp: bool,
    sql_port: Option<NonZeroU16>,
    sync_port: Option<NonZeroU16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    native: Option<Native>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Native {
    sql: NonZeroU16,
    cache: NonZeroU16,
    compiler: NonZeroU16,
}

impl Record {
    pub fn read(paths: &super::Paths) -> Result<Option<Self>> {
        let Some(root) = paths.host_lifecycle.as_deref() else {
            return Ok(None);
        };
        Self::at(root)
    }

    pub fn at(root: &Path) -> Result<Option<Self>> {
        let path = root.join("shared.json");
        let file = match fs::File::options()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let metadata = file.metadata()?;
        if metadata.uid() != fs::metadata(root)?.uid()
            || !metadata.is_file()
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
            || metadata.len() > 4096
        {
            return Err(super::owner::Error::Invalid.into());
        }
        let mut bytes = Vec::new();
        file.take(4097).read_to_end(&mut bytes)?;
        if bytes.len() > 4096 {
            return Err(super::owner::Error::Invalid.into());
        }
        let record: Self = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        let transport_valid = match (record.tcp, record.sql_port, record.sync_port) {
            (false, None, None) => true,
            (true, Some(sql), Some(sync)) => sql != sync,
            _ => false,
        };
        if record.version != 1 || !transport_valid {
            return Err(super::owner::Error::Invalid.into());
        }
        Ok(Some(record))
    }

    fn prepare(
        guard: &coordination::Guard,
        public_sql: NonZeroU16,
        transport: DoltTransport,
    ) -> Result<Self> {
        if let Some(record) = Self::at(guard.root())? {
            return Ok(record);
        }
        let tcp = transport == DoltTransport::Tcp;
        let mut ports = (super::DOLT_PORT_START..super::DOLT_PORT_START + super::DOLT_PORT_WIDTH)
            .filter(|port| *port != public_sql.get() && super::is_loopback_port_available(*port));
        let mut next = || {
            ports
                .next()
                .and_then(NonZeroU16::new)
                .ok_or(Error::NoAvailableLoopbackPort {
                    start: super::DOLT_PORT_START,
                    end: super::DOLT_PORT_START + super::DOLT_PORT_WIDTH - 1,
                })
        };
        let record = Self {
            version: 1,
            tcp,
            sql_port: if tcp { Some(public_sql) } else { None },
            sync_port: if tcp { Some(next()?) } else { None },
            native: None,
        };
        super::managed::atomic_write(
            &guard.root().join("shared.json"),
            &serde_json::to_vec(&record).map_err(io::Error::other)?,
        )?;
        Ok(record)
    }

    pub fn prepare_native(guard: &coordination::Guard, public_sql: NonZeroU16) -> Result<Self> {
        Self::prepare_native_for(guard, public_sql, DoltTransport::from_env()?)
    }

    fn prepare_native_for(
        guard: &coordination::Guard,
        public_sql: NonZeroU16,
        transport: DoltTransport,
    ) -> Result<Self> {
        let mut record = Self::prepare(guard, public_sql, transport)?;
        if record.sql_port.is_some_and(|port| port != public_sql) {
            return Err(super::owner::Error::Conflict.into());
        }
        if record.native.is_none() {
            let listeners = [
                std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?,
                std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?,
                std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?,
            ];
            let port = |index: usize| {
                NonZeroU16::new(listeners[index].local_addr()?.port())
                    .ok_or(super::Error::ZeroDoltPort)
            };
            record.native = Some(Native {
                sql: port(0)?,
                cache: port(1)?,
                compiler: port(2)?,
            });
            super::managed::atomic_write(
                &guard.root().join("shared.json"),
                &serde_json::to_vec(&record).map_err(io::Error::other)?,
            )?;
        }
        Ok(record)
    }

    pub const fn native_ports(&self) -> Option<(NonZeroU16, NonZeroU16, NonZeroU16)> {
        match &self.native {
            Some(native) => Some((native.sql, native.cache, native.compiler)),
            None => None,
        }
    }

    pub const fn public_sql_port(&self) -> Option<NonZeroU16> {
        self.sql_port
    }

    pub const fn cache_port(&self, selected: NonZeroU16) -> NonZeroU16 {
        match &self.native {
            Some(native) => native.cache,
            None => selected,
        }
    }

    pub fn endpoint(&self, root: &Path) -> DoltEndpoint {
        let socket_path = DoltSocketPath(root.join("service-data/dolt/run/dolt.sock"));
        match self.sql_port {
            Some(port) => DoltEndpoint::Tcp { socket_path, port },
            None => DoltEndpoint::UnixSocket { socket_path },
        }
    }

    pub fn arguments(&self, data: &Path) -> Vec<String> {
        let mut args = vec![
            "--label".into(),
            format!("{LABEL}={VERSION}"),
            "-v".into(),
            format!("{}:{}:rw", data.display(), data.display()),
        ];
        if let Some(native) = &self.native {
            args.extend([
                "-p".into(),
                format!("127.0.0.1:{}:3306", native.sql),
                "-p".into(),
                format!("127.0.0.1:{}:4226", native.compiler),
            ]);
        } else if let Some(sql) = self.sql_port {
            args.extend(["-p".into(), format!("127.0.0.1:{sql}:3306")]);
        }
        if let Some(sync) = self.sync_port {
            args.extend(["-p".into(), format!("127.0.0.1:{sync}:3307")]);
        }
        args
    }

    pub const fn transport(&self) -> &'static str {
        if self.tcp { "tcp" } else { "unix" }
    }
}

pub(super) fn host_root(plan: &Plan) -> Result<&Path> {
    plan.paths()
        .host_lifecycle
        .as_deref()
        .and_then(Path::parent)
        .ok_or_else(|| super::owner::Error::Invalid.into())
}

pub(super) fn is_managed(plan: &Plan) -> Result<bool> {
    Ok(Record::read(plan.paths())?.is_some())
}

pub(super) fn register(plan: &Plan, runtime: &Runtime) -> Result<()> {
    let selection = plan
        .selection
        .as_ref()
        .ok_or(super::owner::Error::Invalid)?;
    let authorization = Authorization::admit_host_sql(selection).map_err(io::Error::other)?;
    let manager = manager(plan, runtime, &authorization)?;
    let root = host_root(plan)?;
    let namespace = root
        .join("service-data/dolt/namespaces")
        .join(plan.workspace().hash().as_str());
    manager.prepare_workspace_source(plan.workspace().canonical_path())?;
    migrate_worktree(
        plan.workspace().canonical_path(),
        &plan.beads_sync_branch,
        &namespace,
        &root
            .join("host-control/beads-git")
            .join(plan.workspace().hash().as_str()),
    )?;
    let credentials = manager.register()?;
    super::managed::metadata::restore(plan.workspace().canonical_path(), credentials.database())?;
    fs::create_dir_all(plan.workspace().canonical_path().join(".beads/dolt"))?;
    super::managed::atomic_write(&plan.workspace().canonical_path().join(".beads/dolt/WRIX-MANAGED"), b"Host-managed shared Dolt namespace. Retained source is a recovery checkpoint, not a serving database.\n")?;
    Ok(())
}

fn manager<'a>(
    plan: &Plan,
    runtime: &Runtime,
    authorization: &'a Authorization<'a>,
) -> Result<Manager<'a>> {
    Ok(Manager::in_container(
        authorization,
        plan.beads_sync_branch.clone(),
        Path::new(&runtime.binary),
        plan.cache_container_name().as_str(),
    )?)
}

fn migrate_worktree(
    repository: &Path,
    branch: &wrix_core::git::Branch,
    namespace: &Path,
    private: &Path,
) -> Result<()> {
    crate::dolt::private_directory(private)?;
    let branch = branch.as_str();
    let source = repository.join(".git/beads-worktrees").join(branch);
    let destination = namespace.join("beads-sync").join(branch);
    let host_git = private.join("repository");
    crate::dolt::check_directory(&host_git)?;
    if host_git.exists() {
        return prepare_guest_git(namespace, &host_git);
    }
    crate::dolt::check_directory(&source)?;
    crate::dolt::check_directory(&destination)?;
    fs::create_dir_all(destination.parent().ok_or(crate::dolt::Error::UnsafePath)?)?;
    if source.exists() && destination.exists() {
        return Err(crate::dolt::Error::Incomplete.into());
    }
    // Rename is a durable cutover. Every restart repairs/converts an interrupted
    // linked-worktree move, never treating destination.exists() as completion.
    if source.exists() {
        fs::rename(&source, &destination)?;
    }
    if fs::symlink_metadata(destination.join(".git"))
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(crate::dolt::Error::UnsafePath.into());
    }
    if destination.join(".git").is_dir() {
        return Err(crate::dolt::Error::Incomplete.into());
    }
    let temporary = private.join("git-import");
    if !temporary.exists() {
        // A killed clone must never be mistaken for a complete metadata import.
        let building = private.join("git-import-building");
        crate::dolt::check_directory(&building)?;
        if building.exists() {
            fs::remove_dir_all(&building)?;
        }
        let exists = Command::new("git")
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ])
            .current_dir(repository)
            .status()?
            .success();
        if exists {
            git(
                repository,
                &[
                    "clone",
                    "--no-local",
                    "--single-branch",
                    "--no-checkout",
                    "--branch",
                    branch,
                    &repository.to_string_lossy(),
                    &building.to_string_lossy(),
                ],
            )?;
        } else {
            fs::create_dir_all(&building)?;
            git_output(&building, &["init", "-q"])?;
            git_output(
                &building,
                &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")],
            )?;
        }
        fs::rename(&building, &temporary)?;
        fs::File::open(private)?.sync_all()?;
    }
    for (key, fallback) in [
        ("user.name", "Wrix Beads"),
        ("user.email", "beads@wrix.invalid"),
    ] {
        let configured = Command::new("git")
            .args(["config", key])
            .current_dir(repository)
            .output()?;
        let value = if configured.status.success() {
            String::from_utf8(configured.stdout)
                .map_err(io::Error::other)?
                .trim()
                .to_owned()
        } else {
            fallback.to_owned()
        };
        git_output(&temporary, &["config", key, &value])?;
    }
    // A moved linked worktree still resolves its original index, including staged
    // changes. Copy it before replacing the pointer, without checkout/reset.
    if destination.join(".git").is_file() {
        // Never open Git configuration/pointers in backend-writable data.
        let admin = linked_admin(repository, &source, &destination, branch)?
            .ok_or_else(|| Error::Operation {
                message: "Beads Git migration cannot match the retained worktree to its canonical Git administration record; native backup and private import retained".into(),
            })?;
        let index = admin.join("index");
        if index.is_file() {
            let imported_index = temporary.join(".git/index");
            fs::copy(index, &imported_index)?;
            // Resolve the legacy sharedindex while its original gitdir is still
            // available. The private index must not depend on unmounted metadata.
            let output = Command::new("git")
                .args(["update-index", "--no-split-index"])
                .env("GIT_INDEX_FILE", &imported_index)
                .env("GIT_DIR", &admin)
                .env("GIT_WORK_TREE", &destination)
                .current_dir(repository)
                .output()?;
            if !output.status.success() {
                return Err(Error::Operation {
                    message: String::from_utf8_lossy(&output.stderr).into_owned(),
                });
            }
            copy_index_objects(&admin, &imported_index, &temporary.join(".git"))?;
            fs::File::open(imported_index)?.sync_all()?;
        }
    }
    // The source's actual upstream, not the canonical local gitdir, remains the
    // fixed sync destination. Missing origin means a deliberately local repo.
    let remote = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(repository)
        .output()?;
    if remote.status.success() {
        let url = String::from_utf8(remote.stdout).map_err(io::Error::other)?;
        let has_origin = Command::new("git")
            .args(["remote", "get-url", "origin"])
            .current_dir(&temporary)
            .output()?
            .status
            .success();
        git_output(
            &temporary,
            &[
                "remote",
                if has_origin { "set-url" } else { "add" },
                "origin",
                url.trim(),
            ],
        )?;
        git_output(
            &temporary,
            &["config", &format!("branch.{branch}.remote"), "origin"],
        )?;
        git_output(
            &temporary,
            &[
                "config",
                &format!("branch.{branch}.merge"),
                &format!("refs/heads/{branch}"),
            ],
        )?;
    } else {
        let has_origin = Command::new("git")
            .args(["remote", "get-url", "origin"])
            .current_dir(&temporary)
            .output()?
            .status
            .success();
        if has_origin {
            git_output(&temporary, &["remote", "remove", "origin"])?;
        }
    }
    let fresh = private.join("fresh-worktree");
    if !destination.exists() {
        super::managed::atomic_write(
            &fresh,
            b"complete private checkout before publishing metadata\n",
        )?;
    }
    if fresh.exists() {
        let has_head = Command::new("git")
            .args(["rev-parse", "--verify", "HEAD"])
            .current_dir(&temporary)
            .output()?
            .status
            .success();
        if has_head {
            git_output(&temporary, &["checkout", "--force", branch])?;
        }
        fs::create_dir_all(&destination)?;
        for entry in fs::read_dir(&temporary)? {
            let entry = entry?;
            if entry.file_name() != ".git" {
                fs::rename(entry.path(), destination.join(entry.file_name()))?;
            }
        }
    }
    fs::create_dir_all(&destination)?;
    if destination.join(".git").is_file() {
        fs::remove_file(destination.join(".git"))?;
    }
    fs::rename(temporary.join(".git"), &host_git)?;
    fs::File::open(private)?.sync_all()?;
    prepare_guest_git(namespace, &host_git)
}

fn linked_admin(
    repository: &Path,
    source: &Path,
    destination: &Path,
    branch: &str,
) -> Result<Option<PathBuf>> {
    let base = repository.join(".git/worktrees");
    crate::dolt::check_directory(&base)?;
    if !base.exists() {
        return Ok(None);
    }
    // Old workers shared canonical Git administration through this fixed mount.
    // Recognize only the admitted branch's exact legacy path view, never a
    // caller-supplied alias or the moved backend-visible .git file.
    let guest = Path::new("/workspace/.git/beads-worktrees")
        .join(branch)
        .join(".git");
    let expected_head = format!("ref: refs/heads/{branch}");
    let mut found = None;
    for entry in fs::read_dir(base)? {
        let admin = entry?.path();
        crate::dolt::check_directory(&admin)?;
        let text = fs::read_to_string(admin.join("gitdir"))?;
        let pointer = Path::new(text.trim());
        let path = if pointer.is_absolute() {
            pointer.to_path_buf()
        } else {
            // Modern Git can store this back-pointer relative to its admin
            // directory. Resolve only that host-side record, never the moved
            // backend-visible .git file. The original target may no longer
            // exist after cutover, so filesystem canonicalization cannot work.
            admin.join(pointer)
        };
        let Some(path) = lexical_absolute(&path) else {
            continue;
        };
        if path == source.join(".git") || path == destination.join(".git") || path == guest {
            if fs::read_to_string(admin.join("HEAD"))?.trim() != expected_head {
                return Err(Error::Operation { message: "retained Beads Git administration branch conflicts with the admitted sync branch; native backup and private import retained".into() });
            }
            if found.replace(admin).is_some() {
                return Err(Error::Operation { message: "multiple retained Git administration records claim the Beads worktree; explicit host reconciliation required, no record selected".into() });
            }
        }
    }
    Ok(found)
}

fn lexical_absolute(path: &Path) -> Option<std::path::PathBuf> {
    use std::path::Component;
    if !path.is_absolute() {
        return None;
    }
    let mut result = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => (),
            Component::ParentDir => {
                if !result.pop() {
                    return None;
                }
            }
            _ => result.push(component.as_os_str()),
        }
    }
    Some(result)
}

fn prepare_guest_git(namespace: &Path, host: &Path) -> Result<()> {
    fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(namespace.join("git-sync.lock"))?;
    let guest = namespace.join("guest-git");
    crate::dolt::check_directory(&guest)?;
    if guest.exists() {
        return Ok(());
    }
    let temporary = host
        .parent()
        .ok_or(crate::dolt::Error::UnsafePath)?
        .join("guest-git-building");
    crate::dolt::check_directory(&temporary)?;
    if temporary.exists() {
        fs::remove_dir_all(&temporary)?;
    }
    git(
        namespace,
        &[
            "clone",
            "--no-local",
            "--single-branch",
            "--no-checkout",
            &host.to_string_lossy(),
            &temporary.to_string_lossy(),
        ],
    )?;
    // Preserve the fixed upstream in the guest's independent metadata too.
    let remote = Command::new("git")
        .arg("--git-dir")
        .arg(host)
        .args(["remote", "get-url", "origin"])
        .output()?;
    if remote.status.success() {
        let url = String::from_utf8(remote.stdout).map_err(io::Error::other)?;
        git_output(&temporary, &["remote", "set-url", "origin", url.trim()])?;
    } else {
        git_output(&temporary, &["remote", "remove", "origin"])?;
    }
    if host.join("index").is_file() {
        fs::copy(host.join("index"), temporary.join(".git/index"))?;
        copy_index_objects(host, &host.join("index"), &temporary.join(".git"))?;
    }
    fs::rename(temporary.join(".git"), &guest)?;
    fs::File::open(namespace)?.sync_all()?;
    Ok(())
}

/// Local clone optimization copies unrelated/unreachable canonical objects.
/// Transfer only index-referenced blobs explicitly after a transport clone.
fn copy_index_objects(source: &Path, index: &Path, target: &Path) -> Result<()> {
    use std::io::Write;
    let entries = Command::new("git")
        .args(["ls-files", "--stage", "-z"])
        .env("GIT_DIR", source)
        .env("GIT_INDEX_FILE", index)
        .current_dir(source)
        .output()?;
    if !entries.status.success() {
        return Err(crate::dolt::Error::Incomplete.into());
    }
    let mut hashes = Vec::new();
    for entry in entries
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let header = entry
            .split(|byte| *byte == b'\t')
            .next()
            .ok_or(crate::dolt::Error::Incomplete)?;
        let fields: Vec<_> = header.split(|byte| *byte == b' ').collect();
        if fields.len() != 3 {
            return Err(crate::dolt::Error::Incomplete.into());
        }
        if fields[0] == b"160000" {
            continue;
        } // gitlinks need not exist in this object store
        let hash = fields[1];
        if !matches!(hash.len(), 40 | 64) || !hash.iter().all(u8::is_ascii_hexdigit) {
            return Err(crate::dolt::Error::Incomplete.into());
        }
        hashes.extend_from_slice(hash);
        hashes.push(b'\n');
    }
    if hashes.is_empty() {
        return Ok(());
    }
    let directory = target.parent().ok_or(crate::dolt::Error::UnsafePath)?;
    let packed = tempfile::NamedTempFile::new_in(directory)?;
    let mut pack = Command::new("git")
        .args(["pack-objects", "--stdout", "--revs"])
        .env("GIT_DIR", source)
        .current_dir(source)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(packed.as_file().try_clone()?))
        .spawn()?;
    pack.stdin
        .take()
        .ok_or(crate::dolt::Error::Incomplete)?
        .write_all(&hashes)?;
    if !pack.wait()?.success() {
        return Err(crate::dolt::Error::Incomplete.into());
    }
    let output = Command::new("git")
        .args(["index-pack", "--stdin"])
        .env("GIT_DIR", target)
        .current_dir(target)
        .stdin(Stdio::from(packed.reopen()?))
        .output()?;
    if !output.status.success() {
        return Err(crate::dolt::Error::Incomplete.into());
    }
    Ok(())
}

fn git_output(directory: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .env("PREK_SKIP", "1")
        .env("PREK_ALLOW_NO_CONFIG", "1")
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(Error::Operation {
            message: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8(output.stdout)
        .map_err(io::Error::other)?
        .trim()
        .to_owned())
}

fn git(repository: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .env("PREK_SKIP", "1")
        .env("PREK_ALLOW_NO_CONFIG", "1")
        .current_dir(repository)
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(Error::Operation {
            message: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(())
}

/// Private runtime projection; callers must not log this output or include it in diagnostics.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Client {
    pub credentials: Credentials,
    pub sync: Endpoint,
    pub worktree: PathBuf,
    pub git_common: PathBuf,
    pub git_admin: PathBuf,
    pub lock: PathBuf,
}

pub fn client(sandbox: bool) -> Result<Client> {
    let plan = Plan::for_current_dir(super::CacheMode::Disabled)?;
    plan.authorize()?;
    let runtime = Runtime::from_env()?;
    runtime.validate_selected_owner(&plan)?;
    let record = Record::read(plan.paths())?.ok_or(crate::dolt::Error::Incomplete)?;
    let selection = plan
        .selection
        .as_ref()
        .ok_or(super::owner::Error::Invalid)?;
    let authorization = Authorization::host_sql(selection).map_err(io::Error::other)?;
    let manager = manager(&plan, &runtime, &authorization)?;
    let sync = match record.sync_port {
        Some(port) => {
            let endpoint = if sandbox {
                runtime.sandbox_endpoint(&plan, super::SandboxService::Sync, port.get())?
            } else {
                std::net::SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, port.get())
            };
            Endpoint::Tcp {
                host: *endpoint.ip(),
                port: NonZeroU16::new(endpoint.port()).ok_or(Error::ZeroDoltPort)?,
            }
        }
        None => Endpoint::Unix {
            path: host_root(&plan)?.join("service-data/dolt/run/sync.sock"),
        },
    };
    let namespace = host_root(&plan)?
        .join("service-data/dolt/namespaces")
        .join(plan.workspace().hash().as_str());
    let worktree = namespace
        .join("beads-sync")
        .join(plan.beads_sync_branch.as_str());
    /* No native Git may open worker-writable metadata on the host. */
    let git_common = if sandbox {
        namespace.join("guest-git")
    } else {
        host_root(&plan)?
            .join("host-control/beads-git")
            .join(plan.workspace().hash().as_str())
            .join("repository")
    };
    crate::dolt::check_directory(&git_common)?;
    let git_admin = PathBuf::new();
    Ok(Client {
        credentials: manager.credentials()?,
        sync,
        worktree,
        git_common,
        git_admin,
        lock: namespace.join("git-sync.lock"),
    })
}

pub fn probe_client() -> Result<()> {
    let client = client(false)?;
    let plan = Plan::for_current_dir(super::CacheMode::Disabled)?;
    let mut command = Command::new("timeout");
    command
        .args([
            "--signal=KILL",
            "6",
            "bd",
            "sql",
            "--csv",
            "SELECT DATABASE() AS wrix_namespace",
        ])
        .current_dir(plan.workspace().canonical_path())
        .env("BEADS_DOLT_AUTO_START", "0")
        .env("BEADS_DOLT_SERVER_MODE", "1")
        .env("BEADS_DOLT_SHARED_SERVER", "1")
        .env("BEADS_DOLT_SERVER_USER", client.credentials.user())
        .env("BEADS_DOLT_PASSWORD", client.credentials.password())
        .env("BEADS_DOLT_SERVER_DATABASE", client.credentials.database())
        .env_remove("BEADS_DOLT_SERVER_SOCKET")
        .env_remove("BEADS_DOLT_SERVER_HOST")
        .env_remove("BEADS_DOLT_SERVER_PORT");
    match plan.dolt().ok_or(crate::dolt::Error::Incomplete)? {
        DoltEndpoint::UnixSocket { socket_path } => {
            command.env("BEADS_DOLT_SERVER_SOCKET", socket_path.as_path());
        }
        DoltEndpoint::Tcp { port, .. } => {
            command
                .env("BEADS_DOLT_SERVER_HOST", "127.0.0.1")
                .env("BEADS_DOLT_SERVER_PORT", port.to_string());
        }
    }
    let output = command.stdin(Stdio::null()).output()?;
    if !output.status.success()
        || String::from_utf8_lossy(&output.stdout)
            .lines()
            .collect::<Vec<_>>()
            != ["wrix_namespace", client.credentials.database()]
    {
        return Err(crate::dolt::Error::Unauthorized.into());
    }
    Ok(())
}

pub fn sync(operation: crate::dolt::Sync) -> Result<crate::dolt::broker::Reply> {
    let (credentials, endpoint) = match wrix_state::host::Host::discover() {
        Ok(_) => {
            // An inherited shell environment is not host namespace selection.
            let client = client(false)?;
            (client.credentials, client.sync)
        }
        Err(wrix_state::host::Error::Worker) => {
            let endpoint = std::env::var("WRIX_BEADS_SYNC_ENDPOINT")
                .map_err(|_| crate::dolt::Error::Unauthorized)?;
            (
                Credentials::from_environment()?,
                serde_json::from_str(&endpoint).map_err(io::Error::other)?,
            )
        }
        Err(error) => return Err(io::Error::other(error).into()),
    };
    Ok(crate::dolt::broker::request(
        &endpoint,
        &credentials,
        operation,
    )?)
}

pub(super) fn sql_active(plan: &Plan) -> Result<bool> {
    let selection = plan
        .selection
        .as_ref()
        .ok_or(super::owner::Error::Invalid)?;
    match Authorization::host_sql(selection) {
        Ok(_) => Ok(true),
        Err(crate::control::Error::PermissionDenied) => Ok(false),
        Err(error) => Err(io::Error::other(error).into()),
    }
}

pub(super) fn retire(plan: &Plan, runtime: &Runtime) -> Result<()> {
    let selection = plan
        .selection
        .as_ref()
        .ok_or(super::owner::Error::Invalid)?;
    let authorization = match Authorization::host_sql(selection) {
        Ok(authorization) => authorization,
        Err(crate::control::Error::PermissionDenied) => return Ok(()),
        Err(error) => return Err(io::Error::other(error).into()),
    };
    manager(plan, runtime, &authorization)?.retire()?;
    authorization.retire_sql().map_err(io::Error::other)?;
    Ok(())
}

pub(super) fn check_inventory(
    runtime: &Runtime,
    selected: Option<&super::owner::Selected>,
    legacy_namespace: Option<&wrix_core::path::WorkspaceHash>,
) -> Result<()> {
    for backend in [&runtime.alternate(), runtime] {
        if let Some(containers) = backend.available_container_infos()? {
            for container in containers {
                if container.kind.as_deref() != Some("service") {
                    continue;
                }
                if selected.is_some_and(|selected| {
                    selected.runtime == backend.kind
                        && selected.container.as_str() == container.name
                }) {
                    continue;
                }
                let roots = wrix_core::host_path::Roots::discover()?;
                if super::generation::owns(roots.state(), backend.kind, &container.name)? {
                    continue;
                }
                let name = wrix_core::path::ContainerName::from_persisted(container.name.clone())
                    .ok_or(super::owner::Error::Invalid)?;
                match backend.status(&name)? {
                    RuntimeStatus::Running => {
                        return Err(Error::IncompatibleService {
                            container: container.name,
                        });
                    }
                    RuntimeStatus::Stopped
                        if legacy_namespace.is_some_and(|namespace| {
                            container.workspace_hash.as_deref() == Some(namespace.as_str())
                        }) =>
                    {
                        return Err(Error::Operation {
                            message: format!(
                                "legacy workspace service {} is stopped; run `wrix service recover` explicitly before converting this namespace. Persistent data and signing state will be retained",
                                container.name
                            ),
                        });
                    }
                    RuntimeStatus::Stopped | RuntimeStatus::Missing => (),
                }
            }
        }
    }
    Ok(())
}
