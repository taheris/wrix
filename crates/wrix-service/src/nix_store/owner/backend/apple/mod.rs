//! Apple native ownership; runtime configuration and supervised process exit are independent evidence.

mod process;

use super::{
    Attempt, COMMAND_BUDGET, Error, NativeId, Owner, Program, State, Status, Target, Tools,
    bootstrap, command, private_file, text,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File},
    io::{self, Read, Write},
    num::NonZeroU16,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
use wrix_sandbox::image::{ImageId, ImageRef};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Binding {
    configuration: Value,
    port: NonZeroU16,
    uid: u32,
    gid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    process: Option<process::Witness>,
}

impl Binding {
    pub(super) const fn matches_port(&self, port: NonZeroU16) -> bool {
        self.port.get() == port.get()
    }
}

#[derive(Deserialize)]
#[serde(try_from = "String")]
struct Name(String);
impl TryFrom<String> for Name {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Error> {
        if !(2..=63).contains(&value.len())
            || !value.as_bytes()[0].is_ascii_alphanumeric()
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(Error::Invalid);
        }
        Ok(Self(value))
    }
}

#[derive(Deserialize)]
struct Snapshot {
    id: Name,
    configuration: Value,
    status: NativeStatus,
}
#[derive(Deserialize)]
struct NativeStatus {
    state: RuntimeState,
}
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum RuntimeState {
    Running,
    Stopped,
    #[serde(other)]
    Other,
}

fn invoke(tools: &Tools, args: &[&str]) -> Result<Vec<u8>, Error> {
    command(
        tools,
        Program::Container,
        args.iter().map(OsString::from),
        Some(COMMAND_BUDGET),
    )
}

fn runtime_check(tools: &Tools) -> Result<(), Error> {
    let executable = fs::canonicalize(tools.command(Program::Container).get_program())?;
    let root = executable
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or(Error::Invalid)?;
    runtime_status(
        &invoke(tools, &["system", "status", "--format", "json"])?,
        root,
    )
}

fn runtime_status(bytes: &[u8], root: &std::path::Path) -> Result<(), Error> {
    let status: Value = serde_json::from_slice(bytes)?;
    if status["status"] != "running"
        || status["client"]["appName"] != "container"
        || status["server"]["appName"] != "container-apiserver"
        || status["paths"]["installRoot"]
            .as_str()
            .map(std::path::Path::new)
            != Some(root)
    {
        return Err(Error::Unsupported);
    }
    for field in ["version", "build", "commit"] {
        let client = status["client"][field].as_str().ok_or(Error::Invalid)?;
        if client.is_empty() || status["server"][field] != client {
            return Err(Error::Unsupported);
        }
    }
    Ok(())
}

pub(super) fn available(tools: &Tools) -> Result<(), Error> {
    runtime_check(tools)?;
    let _: Vec<Snapshot> =
        serde_json::from_slice(&invoke(tools, &["list", "--all", "--format", "json"])?)?;
    for path in [
        tools.attach(),
        PathBuf::from(tools.command(Program::Launchctl).get_program()),
    ] {
        let metadata = fs::metadata(path)?;
        if !metadata.is_file() || metadata.mode() & 0o111 == 0 || metadata.mode() & 0o022 != 0 {
            return Err(Error::Unsupported);
        }
    }
    Ok(())
}

fn snapshot(tools: &Tools, id: &NativeId) -> Result<Snapshot, Error> {
    runtime_check(tools)?;
    let mut entries: Vec<Snapshot> = serde_json::from_slice(&invoke(tools, &["inspect", &id.0])?)?;
    if entries.len() != 1 {
        return Err(Error::Invalid);
    }
    let entry = entries.pop().ok_or(Error::Invalid)?;
    if entry.id.0 != id.0 || entry.configuration["id"] != id.0 {
        return Err(Error::Invalid);
    }
    Ok(entry)
}

fn labels(owner: &Owner, attempt: &Attempt) -> BTreeMap<String, String> {
    [
        ("wrix.managed", "true".to_owned()),
        ("wrix.kind", "nix-pool".to_owned()),
        ("wrix.deployment", owner.record.deployment.to_string()),
        (
            "wrix.nix.repository",
            owner.record.pool.repository.to_string(),
        ),
        (
            "wrix.nix.system",
            owner.record.pool.system.as_str().to_owned(),
        ),
        ("wrix.nix.attempt", attempt.nonce.as_str().to_owned()),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value))
    .collect()
}

pub(super) fn environment(value: &Value) -> Result<BTreeMap<String, String>, Error> {
    let mut environment = BTreeMap::new();
    if value.is_null() {
        return Ok(environment);
    }
    for entry in value.as_array().ok_or(Error::Invalid)? {
        let (key, value) = entry
            .as_str()
            .and_then(|value| value.split_once('='))
            .ok_or(Error::Invalid)?;
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || environment
                .insert(key.to_owned(), value.to_owned())
                .is_some()
        {
            return Err(Error::Invalid);
        }
    }
    Ok(environment)
}

fn validate(
    owner: &Owner,
    attempt: &Attempt,
    config: &Value,
    port: NonZeroU16,
    uid: u32,
    gid: u32,
) -> Result<(), Error> {
    bootstrap::ownership::validate(uid, gid).map_err(|_| Error::Invalid)?;
    let birth = config["creationDate"].as_str().ok_or(Error::Invalid)?;
    birth_date(birth)?;
    let data = owner.prepared.data_root();
    let metadata = fs::symlink_metadata(data)?;
    let source = data.to_str().ok_or(Error::Invalid)?;
    let expected_mount = json!({"type":{"virtiofs":{}}, "source":source, "destination":"/var/lib/wrix/nix-pool", "options":["rw"]});
    let process = &config["initProcess"];
    let mut expected_env = attempt.software.environment.clone().ok_or(Error::Invalid)?;
    expected_env.insert(
        "WRIX_NIX_POOL_DATA_OWNER".to_owned(),
        format!("{uid}:{gid}"),
    );
    let env = environment(&process["environment"])?;
    let architecture = match owner.pool().system {
        wrix_core::nix_store::pool::System::X86_64Linux => "amd64",
        wrix_core::nix_store::pool::System::Aarch64Linux => "arm64",
    };
    if config["id"] != attempt.name()
        || config["image"]["descriptor"]["digest"]
            .as_str()
            .ok_or(Error::Invalid)?
            .strip_prefix("sha256:")
            .ok_or(Error::Invalid)?
            != attempt
                .software
                .image
                .as_str()
                .trim_start_matches("sha256:")
        || config["image"]["reference"].as_str()
            != Some(
                attempt
                    .software
                    .reference
                    .as_ref()
                    .ok_or(Error::Invalid)?
                    .as_str(),
            )
        || config["labels"] != serde_json::to_value(labels(owner, attempt))?
        || config["resources"]["cpus"] != 2
        || config["resources"]["cpuOverhead"] != 1
        || config["resources"]["memoryInBytes"] != 4_294_967_296_u64
        || config["mounts"] != json!([expected_mount])
        || config["publishedPorts"]
            != json!([{"hostAddress":"127.0.0.1", "hostPort":port, "containerPort":2222, "proto":"tcp", "count":1}])
        || config["publishedSockets"] != json!([])
        || config["platform"]["os"] != "linux"
        || config["platform"]["architecture"] != architecture
        || config["runtimeHandler"] != "container-runtime-linux"
        || config["rosetta"] != false
        || config["ssh"] != false
        || config["virtualization"] != false
        || config["readOnly"] != false
        || config["useInit"] != false
        || config["capAdd"] != json!(["CAP_NET_ADMIN", "CAP_SYS_ADMIN", "CAP_SYS_RESOURCE"])
        || config["capDrop"] != json!([])
        || process["executable"] != "/bin/wrix-nix-pool"
        || process["arguments"] != json!([])
        || process["workingDirectory"] != "/"
        || process["terminal"] != false
        || process["user"] != json!({"id":{"uid":0,"gid":0}})
        || process["supplementalGroups"] != json!([])
        || process["rlimits"] != json!([])
        || config["sysctls"] != json!({})
        || env != expected_env
        || !metadata.is_dir()
        || metadata.uid() != uid
        || metadata.gid() != gid
        || metadata.mode() & 0o777 != 0o700
        || data.canonicalize()? != data
    {
        return Err(Error::Invalid);
    }
    Ok(())
}

fn birth_date(value: &str) -> Result<(), Error> {
    let bytes = value.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return Err(Error::Invalid);
    }
    let number = |range: std::ops::Range<usize>| -> Result<u32, Error> {
        bytes[range].iter().try_fold(0, |value, byte| {
            if byte.is_ascii_digit() {
                Ok(value * 10 + u32::from(byte - b'0'))
            } else {
                Err(Error::Invalid)
            }
        })
    };
    let year = number(0..4)?;
    let month = number(5..7)?;
    let day = number(8..10)?;
    let days = match month {
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return Err(Error::Invalid),
    };
    if year < 2020
        || day == 0
        || day > days
        || number(11..13)? > 23
        || number(14..16)? > 59
        || number(17..19)? > 59
    {
        return Err(Error::Invalid);
    }
    Ok(())
}

pub(super) fn validate_binding(
    owner: &Owner,
    target: &Target,
    port: NonZeroU16,
) -> Result<(), Error> {
    let binding = target.apple.as_ref().ok_or(Error::Invalid)?;
    if target.id.0 != target.attempt.name() || !binding.matches_port(port) {
        return Err(Error::Invalid);
    }
    validate(
        owner,
        &target.attempt,
        &binding.configuration,
        binding.port,
        binding.uid,
        binding.gid,
    )
}

pub(super) fn observe(owner: &Owner, target: &Target) -> Result<Status, Error> {
    let binding = target.apple.as_ref().ok_or(Error::Invalid)?;
    let entry = snapshot(&target.attempt.software.tools, &target.id)?;
    validate(
        owner,
        &target.attempt,
        &entry.configuration,
        binding.port,
        binding.uid,
        binding.gid,
    )?;
    if entry.configuration != binding.configuration {
        return Err(Error::Invalid);
    }
    match entry.status.state {
        RuntimeState::Running => {
            binding
                .process
                .as_ref()
                .ok_or(Error::Retained)?
                .running(&target.attempt.software.tools, &target.id)?;
            if outcome(owner, target)?.is_some() {
                return Err(Error::Retained);
            }
            Ok(Status {
                running: true,
                exit_code: None,
            })
        }
        RuntimeState::Stopped => {
            let exit_code = outcome(owner, target)?.ok_or(Error::Retained)?;
            if exit_code == 0 {
                clean_exit(owner, target)?;
            }
            Ok(Status {
                running: false,
                exit_code: Some(exit_code),
            })
        }
        RuntimeState::Other => Err(Error::Retained),
    }
}

pub(super) fn absent(target: &Target) -> Result<bool, Error> {
    if target.apple.is_none() || target.id.0 != target.attempt.name() {
        return Err(Error::Invalid);
    }
    runtime_check(&target.attempt.software.tools)?;
    let entries: Vec<Snapshot> = serde_json::from_slice(&invoke(
        &target.attempt.software.tools,
        &["list", "--all", "--format", "json"],
    )?)?;
    Ok(!entries
        .iter()
        .any(|entry| entry.id.0 == target.id.0 || entry.configuration["id"] == target.id.0))
}

fn outcome_name(target: &Target) -> String {
    format!("apple-exit-{}", target.attempt.nonce.as_str())
}
fn outcome(owner: &Owner, target: &Target) -> Result<Option<i32>, Error> {
    let root = &owner.prepared.reservation.paths.control;
    match fs::symlink_metadata(root.join(outcome_name(target))) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(_) => (),
    }
    let mut bytes = Vec::new();
    private_file(root, &outcome_name(target), false)?
        .take(32)
        .read_to_end(&mut bytes)?;
    let value = std::str::from_utf8(&bytes).map_err(|_| Error::Invalid)?;
    let code: u8 = value
        .strip_suffix('\n')
        .ok_or(Error::Invalid)?
        .parse()
        .map_err(|_| Error::Invalid)?;
    if value != format!("{code}\n") {
        return Err(Error::Invalid);
    }
    Ok(Some(i32::from(code)))
}

fn native_gone(target: &Target) -> Result<bool, Error> {
    target
        .apple
        .as_ref()
        .and_then(|binding| binding.process.as_ref())
        .ok_or(Error::Retained)?
        .gone(&target.attempt.software.tools, &target.id)
}

pub(super) fn clean_exit(owner: &Owner, target: &Target) -> Result<(), Error> {
    if outcome(owner, target)? != Some(0) || !native_gone(target)? {
        return Err(Error::Retained);
    }
    Ok(())
}

pub(super) fn wait(owner: &Owner, target: &Target) -> Result<(), Error> {
    let deadline = Instant::now() + COMMAND_BUDGET;
    loop {
        match outcome(owner, target)? {
            Some(0) if native_gone(target)? => return Ok(()),
            Some(0) | None if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            Some(_) | None => return Err(Error::Retained),
        }
    }
}

fn unit(target: &Target) -> String {
    format!("org.wrix.nix-pool.{}", target.attempt.nonce.as_str())
}
fn domain() -> String {
    format!("gui/{}", rustix::process::geteuid().as_raw())
}
fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn supervise(owner: &Owner, target: &Target) -> Result<(), Error> {
    let root = &owner.prepared.reservation.paths.control;
    let tools = &target.attempt.software.tools;
    let args = [
        tools.attach(),
        PathBuf::from(tools.command(Program::Container).get_program()),
        PathBuf::from(&target.id.0),
        root.join(outcome_name(target)),
    ];
    let mut array = String::new();
    for arg in args {
        let text = arg.to_str().ok_or(Error::Invalid)?;
        if text.chars().any(char::is_control) {
            return Err(Error::Invalid);
        }
        array.push_str("<string>");
        array.push_str(&xml(text));
        array.push_str("</string>");
    }
    let home = std::env::var("HOME").map_err(|_| Error::Invalid)?;
    if !std::path::Path::new(&home).is_absolute() || home.chars().any(char::is_control) {
        return Err(Error::Invalid);
    }
    let home = xml(&home);
    let name = format!("{}.plist", unit(target));
    let mut file = private_file(root, &name, true)?;
    write!(
        file,
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array>{array}</array><key>EnvironmentVariables</key><dict><key>HOME</key><string>{home}</string></dict><key>RunAtLoad</key><true/><key>KeepAlive</key><false/><key>ProcessType</key><string>Background</string><key>WorkingDirectory</key><string>/</string></dict></plist>",
        unit(target)
    )?;
    file.sync_all()?;
    File::open(root)?.sync_all()?;
    command(
        tools,
        Program::Launchctl,
        [
            OsString::from("bootstrap"),
            domain().into(),
            root.join(name).into_os_string(),
        ],
        Some(COMMAND_BUDGET),
    )?;
    Ok(())
}

pub(super) fn remove(owner: &Owner, target: &Target) -> Result<(), Error> {
    clean_exit(owner, target)?;
    let tools = &target.attempt.software.tools;
    invoke(tools, &["delete", &target.id.0])?;
    if !absent(target)? {
        return Err(Error::Retained);
    }
    // Never boot out a still-attached process; the immutable completion receipt precedes native deletion.
    command(
        tools,
        Program::Launchctl,
        [
            OsString::from("bootout"),
            format!("{}/{}", domain(), unit(target)).into(),
        ],
        Some(COMMAND_BUDGET),
    )?;
    Ok(())
}

pub(super) fn boot(owner: &mut Owner, attempt: &Attempt) -> Result<(Target, NonZeroU16), Error> {
    let tools = &attempt.software.tools;
    let data = owner.prepared.data_root();
    data.to_str().ok_or(Error::Invalid)?;
    let metadata = fs::symlink_metadata(data)?;
    let (uid, gid) = (metadata.uid(), metadata.gid());
    bootstrap::ownership::validate(uid, gid).map_err(|_| Error::Invalid)?;
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    let port = NonZeroU16::new(listener.local_addr()?.port()).ok_or(Error::Invalid)?;
    let mut args = [
        "create",
        "--name",
        &attempt.name(),
        "--cpus",
        "2",
        "--memory",
        "4G",
        "--uid",
        "0",
        "--gid",
        "0",
        "--entrypoint",
        "/bin/wrix-nix-pool",
        "--workdir",
        "/",
    ]
    .map(OsString::from)
    .to_vec();
    for cap in ["NET_ADMIN", "SYS_ADMIN", "SYS_RESOURCE"] {
        args.extend(["--cap-add".into(), cap.into()]);
    }
    args.extend([
        "--env".into(),
        format!("WRIX_NIX_POOL_DATA_OWNER={uid}:{gid}").into(),
        "--publish".into(),
        format!("127.0.0.1:{port}:2222/tcp").into(),
    ]);
    for (key, value) in labels(owner, attempt) {
        args.extend(["--label".into(), format!("{key}={value}").into()]);
    }
    let mut mount = data.as_os_str().to_owned();
    if data.as_os_str().as_bytes().contains(&b':') {
        return Err(Error::Invalid);
    }
    mount.push(":/var/lib/wrix/nix-pool:rw");
    let reference = attempt.software.reference.as_ref().ok_or(Error::Invalid)?;
    image_config(
        tools,
        &attempt.software.image,
        reference,
        owner.pool().system,
    )?;
    args.extend(["--volume".into(), mount, reference.as_str().into()]);
    drop(listener);
    let result = text(command(
        tools,
        Program::Container,
        args,
        Some(COMMAND_BUDGET),
    )?)?;
    let id: NativeId = attempt.name().try_into()?;
    if result != id.0 {
        return Err(Error::Retained);
    }
    let entry = snapshot(tools, &id)?;
    validate(owner, attempt, &entry.configuration, port, uid, gid)?;
    if !matches!(entry.status.state, RuntimeState::Stopped) {
        return Err(Error::Retained);
    }
    let mut target = Target {
        attempt: attempt.clone(),
        id,
        apple: Some(Binding {
            configuration: entry.configuration,
            port,
            uid,
            gid,
            process: None,
        }),
    };
    owner.record.state = State::Created {
        target: target.clone(),
    };
    owner
        .record
        .publish(&owner.prepared.reservation.paths.control)?;
    supervise(owner, &target)?;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if outcome(owner, &target)?.is_some() {
            return Err(Error::Retained);
        }
        let entry = snapshot(tools, &target.id)?;
        if entry.configuration != target.apple.as_ref().ok_or(Error::Invalid)?.configuration {
            return Err(Error::Invalid);
        }
        if matches!(entry.status.state, RuntimeState::Running) {
            break;
        }
        if Instant::now() >= deadline {
            return Err(Error::Retained);
        }
        thread::sleep(Duration::from_millis(50));
    }
    let witness = process::Witness::observe(tools, &target.id)?;
    target.apple.as_mut().ok_or(Error::Invalid)?.process = Some(witness);
    owner.record.state = State::Created {
        target: target.clone(),
    };
    owner
        .record
        .publish(&owner.prepared.reservation.paths.control)?;
    Ok((target, port))
}

pub(super) fn image_config(
    tools: &Tools,
    image: &ImageId,
    reference: &ImageRef,
    system: wrix_core::nix_store::pool::System,
) -> Result<(Value, ImageRef), Error> {
    let bytes = invoke(tools, &["image", "inspect", reference.as_str()])?;
    let entries: Vec<Value> = serde_json::from_slice(&bytes)?;
    if entries.len() != 1
        || entries[0]["configuration"]["descriptor"]["digest"]
            .as_str()
            .and_then(|digest| digest.strip_prefix("sha256:"))
            != Some(image.as_str().trim_start_matches("sha256:"))
    {
        return Err(Error::Invalid);
    }
    let reference = ImageRef::parse(
        entries[0]["configuration"]["name"]
            .as_str()
            .ok_or(Error::Invalid)?,
    )
    .map_err(wrix_sandbox::image::Error::from)?;
    Ok((parse_image(&bytes, system)?, reference))
}
fn parse_image(bytes: &[u8], system: wrix_core::nix_store::pool::System) -> Result<Value, Error> {
    let entries: Vec<Value> = serde_json::from_slice(bytes)?;
    if entries.len() != 1 {
        return Err(Error::Invalid);
    }
    let architecture = match system {
        wrix_core::nix_store::pool::System::X86_64Linux => "amd64",
        wrix_core::nix_store::pool::System::Aarch64Linux => "arm64",
    };
    let variants = entries[0]["variants"].as_array().ok_or(Error::Invalid)?;
    let configs = variants
        .iter()
        .filter(|variant| {
            variant["platform"]["os"] == "linux"
                && variant["platform"]["architecture"] == architecture
        })
        .collect::<Vec<_>>();
    if configs.len() != 1
        || configs[0]["config"]["architecture"] != architecture
        || configs[0]["config"]["os"] != "linux"
    {
        return Err(Error::Invalid);
    }
    Ok(configs[0]["config"].clone())
}

#[cfg(test)]
mod runtime_test;
#[cfg(test)]
mod test;
