//! Isolated native factory acceptance; public Run/Spawn admission remains independently gated.
mod boundary;
mod fault;
mod observation;
mod worker;

use super::*;
use std::{os::unix::fs::PermissionsExt, process::Command};
use wrix_sandbox::nix_store::{Attempt, Factory};
use wrix_service::nix_store::worker::Connected;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Platform {
    Apple,
    Krun,
    Podman,
}
impl Platform {
    fn prerequisite(self) {
        let supported = match self {
            Self::Apple => cfg!(all(target_os = "macos", target_arch = "aarch64")),
            Self::Krun | Self::Podman => cfg!(target_os = "linux"),
        };
        assert!(
            supported,
            "UNAVAILABLE: {self:?} requires its actual native host; no emulation or runtime substitution"
        );
        for variable in INPUTS {
            let path = PathBuf::from(
                std::env::var_os(variable)
                    .unwrap_or_else(|| panic!("UNAVAILABLE: package fixture input {variable}")),
            );
            assert!(path.is_file(), "UNAVAILABLE: {variable}={path:?}");
        }
    }
    fn prepare(self, attempt: Attempt, factory: &Connected) -> wrix_sandbox::nix_store::Prepared {
        let result = match self {
            Self::Apple => attempt.prepare_apple(factory),
            Self::Krun => attempt.prepare_krun(factory),
            Self::Podman => attempt.prepare(factory),
        };
        result.unwrap_or_else(|error| panic!("UNAVAILABLE: actual {self:?} factory: {error}"))
    }
    fn admit(self, factory: &Connected) {
        let result = match self {
            Self::Apple => factory.admit_apple(),
            Self::Krun => factory.admit_krun(),
            Self::Podman => factory.admit(),
        };
        result.unwrap_or_else(|error| {
            panic!("UNAVAILABLE: actual {self:?} host prerequisite: {error}")
        });
    }
}
#[derive(Clone, Copy)]
struct Harness<'a> {
    platform: Platform,
    factory: &'a Connected,
    selection: &'a Selection,
    linked: &'a Selection,
    peer: &'a Selection,
    host: &'a wrix_state::host::Host,
    runtime: &'a tokio::runtime::Runtime,
    client: &'a Client,
}
const INPUTS: &[&str] = &[
    "WRIX_NIX_POOL_PACKAGE_BINDING",
    "WRIX_NIX_POOL_HOST_TOOLS",
    "WRIX_CONTROLLER_PACKAGE_BINDING",
    "WRIX_TEST_POOL_CONTROLLER",
    "WRIX_TEST_POOL_CONTROLLER_BINDING",
    "WRIX_TEST_POOL_WORKER_IMAGE",
    "WRIX_TEST_POOL_PROFILE",
    "WRIX_TEST_POOL_NATIVE_INPUTS",
    "WRIX_TEST_POOL_UNSIGNED",
    "WRIX_TEST_POOL_NIX",
    "WRIX_TEST_POOL_NIX_STORE",
];
const APPLE: &str = "nix_store::admission::request::native::platform::apple_native_workers_reuse_installed_store_across_independent_vms";
const KRUN: &str = "nix_store::admission::request::native::platform::krun_native_workers_reuse_installed_store_across_independent_vms";
const PODMAN: &str =
    "nix_store::admission::request::native::platform::podman_native_workers_reuse_installed_store";

#[test]
#[ignore = "UNRUN: requires native Apple container, existing macFUSE, launchd GUI session and package-bound images; not public CLI acceptance"]
fn apple_native_workers_reuse_installed_store_across_independent_vms() {
    run(Platform::Apple, APPLE);
}
#[test]
#[ignore = "UNRUN: requires native Linux KVM/krun, rootless Podman, FUSE and package-bound images; not public CLI acceptance"]
fn krun_native_workers_reuse_installed_store_across_independent_vms() {
    run(Platform::Krun, KRUN);
}
#[test]
#[ignore = "UNRUN: requires native rootless Podman/FUSE and a separate KVM pool VM; not public CLI acceptance"]
fn podman_native_workers_reuse_installed_store() {
    run(Platform::Podman, PODMAN);
}
#[test]
fn package_native_selectors_resolve_to_compiled_tests() {
    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--list")
        .output()
        .unwrap();
    assert!(output.status.success());
    let listed = String::from_utf8(output.stdout).unwrap();
    for name in [
        APPLE,
        KRUN,
        PODMAN,
        "nix_store::admission::request::native::platform::fault::apple_native_daemon_death_retains_roots_and_refuses_replay",
        "nix_store::admission::request::native::platform::fault::krun_native_daemon_death_retains_roots_and_refuses_replay",
    ] {
        assert!(
            listed.lines().any(|line| line == format!("{name}: test")),
            "native selector would run zero tests: {name}"
        );
    }
}
fn run(platform: Platform, name: &str) {
    run_case(platform, name, exercise, false);
}
fn run_case(platform: Platform, name: &str, case: fn(Platform, &Path), retain: bool) {
    platform.prerequisite();
    if let Some(caller) = child() {
        case(platform, &caller);
        return;
    }
    assert!(
        !Path::new("/etc/wrix/image-agent").exists(),
        "UNAVAILABLE: native host required, not a worker/chroot"
    );
    let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let mut fixture = Fixture {
        root,
        isolated: false,
    };
    fixture.root.disable_cleanup(true);
    fixture.initialize();
    #[cfg(target_os = "linux")]
    let fixture = NativeFixture::new(fixture);
    assert!(
        fixture
            .run(&["acknowledge-scope", "--version", "1"])
            .status
            .success()
    );
    let checkout = fixture.repository("checkout");
    eprintln!(
        "native fixture retained on failure: {}",
        fixture.root.path().display()
    );
    assert!(
        fixture
            .git(
                &checkout,
                &[
                    "-c",
                    "user.name=Native",
                    "-c",
                    "user.email=native@example.test",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "native fixture"
                ]
            )
            .status
            .success()
    );
    let linked = fixture.inside("linked");
    assert!(
        fixture
            .git(
                &checkout,
                &["worktree", "add", "--detach", linked.to_str().unwrap()]
            )
            .status
            .success()
    );
    deployment(&fixture);
    let first = success(&fixture, &checkout, &["enable"]);
    let second = success(&fixture, &linked, &["enable"]);
    assert_eq!(
        super::super::super::super::repository(&first),
        super::super::super::super::repository(&second)
    );
    let peer = fixture.repository("peer");
    success(&fixture, &peer, &["enable"]);
    public_gate(&fixture, &checkout);
    let mut command = isolation::command(
        fixture.root.path(),
        false,
        &std::env::current_exe().unwrap(),
        &["--ignored", "--exact", name, "--nocapture"],
    );
    let runtime_directory = fixture.root.path().join("runtime");
    fs::create_dir_all(&runtime_directory).unwrap();
    fs::set_permissions(&runtime_directory, fs::Permissions::from_mode(0o700)).unwrap();
    command
        .env(CALLER, &checkout)
        .env("XDG_RUNTIME_DIR", &runtime_directory);
    for variable in INPUTS {
        command.env(variable, std::env::var_os(variable).unwrap());
    }
    for variable in ["USER", "LOGNAME", "DBUS_SESSION_BUS_ADDRESS"] {
        if let Some(value) = std::env::var_os(variable) {
            command.env(variable, value);
        }
    }
    Running(command.spawn().unwrap()).finish();
    let report = fs::read_to_string(fixture.root.path().join("native-measurements.json")).unwrap();
    eprintln!("isolated native capability observations (NOT public dispatch acceptance): {report}");
    if retain {
        eprintln!(
            "negative recovery control retains fenced native data and roots: {}",
            fixture.root.path().display()
        );
    } else {
        #[cfg(target_os = "linux")]
        fixture.finish();
        #[cfg(not(target_os = "linux"))]
        fixture.root.disable_cleanup(false);
    }
}
fn public_spawn(checkout: &Path) -> serde_json::Value {
    serde_json::json!({"workspace": checkout, "env": [], "agent_args": [], "nix_store": {"mode": "shared"}})
}

#[test]
fn public_dispatch_fixture_uses_a_valid_production_spawn_config() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("spawn.json");
    fs::write(
        &path,
        serde_json::to_vec(&public_spawn(root.path())).unwrap(),
    )
    .unwrap();
    let config = wrix_sandbox::command::config::load_spawn_config(
        &path,
        wrix_sandbox::command::config::Platform::CURRENT,
    )
    .unwrap();
    assert_eq!(config.workspace, root.path().to_str().unwrap());
}

fn public_gate(fixture: &Fixture, checkout: &Path) {
    let profile = PathBuf::from(std::env::var_os("WRIX_TEST_POOL_PROFILE").unwrap());
    let spawn = fixture.root.path().join("public-spawn.json");
    fs::write(&spawn, serde_json::to_vec(&public_spawn(checkout)).unwrap()).unwrap();
    let before = crate::fixture::snapshot(&fixture.state());
    let profile = profile.to_str().unwrap();
    let run = fixture.application(
        checkout,
        &["--profile-config", profile, "run", "--nix-store", "shared"],
    );
    let spawn = fixture.application(
        checkout,
        &[
            "--profile-config",
            profile,
            "spawn",
            "--spawn-config",
            spawn.to_str().unwrap(),
        ],
    );
    for output in [run, spawn] {
        assert!(
            !output.status.success(),
            "unproved public dispatch was enabled by isolated native acceptance"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("Shared installed Nix execution is unavailable"),
            "public dispatch failed for an unrelated reason: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(crate::fixture::snapshot(&fixture.state()), before);
    assert!(!checkout.join(".wrix").exists());
}
fn controller(host: &wrix_state::host::Host) -> Running {
    let mut command = Command::new(std::env::var_os("WRIX_TEST_POOL_CONTROLLER").unwrap());
    command.arg(host.roots().state().join("control"));
    let log = fs::File::create(host.roots().state().join("native-controller.log")).unwrap();
    command.stdout(log.try_clone().unwrap()).stderr(log);
    let mut process = Running(command.spawn().unwrap());
    let deadline = Instant::now() + BUDGET;
    while !host
        .roots()
        .state()
        .join("control/authority/control.sock")
        .exists()
    {
        assert!(
            process.0.try_wait().unwrap().is_none(),
            "native controller exited; inspect native-controller.log"
        );
        assert!(
            Instant::now() < deadline,
            "UNAVAILABLE: native controller readiness"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    process
}
fn stop_controller(controller: &mut Running) {
    let pid = rustix::process::Pid::from_raw(i32::try_from(controller.0.id()).unwrap()).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();
    assert!(controller.0.wait().unwrap().success());
}
fn local_owner(
    runtime: &tokio::runtime::Runtime,
    host: &wrix_state::host::Host,
    selection: &Selection,
) -> (Controller, wrix_service::nix_store::owner::backend::Owner) {
    let local = {
        let _entered = runtime.enter();
        Controller::bind(&host.roots().state().join("control/authority")).unwrap()
    };
    let deadline = Instant::now() + BUDGET;
    let owner = loop {
        match local.ensure_nix_pool(selection, system()) {
            Ok(owner) => break owner,
            Err(wrix_service::control::Error::PoolOwner(
                wrix_service::nix_store::owner::backend::Error::Busy,
            )) => {
                assert!(
                    Instant::now() < deadline,
                    "recorded native owner lock did not drain"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => panic!("native recorded owner unavailable: {error}"),
        }
    };
    (local, owner)
}
fn exercise(platform: Platform, caller: &Path) {
    let selection = Selection::inspect(caller).unwrap();
    let linked = Selection::inspect(&caller.parent().unwrap().join("linked")).unwrap();
    let peer = Selection::inspect(&caller.parent().unwrap().join("peer")).unwrap();
    let host = wrix_state::host::Host::discover().unwrap();
    let first_pool = Admission::acquire(&selection, system())
        .unwrap()
        .pool()
        .clone();
    assert_eq!(
        first_pool,
        *Admission::acquire(&linked, system()).unwrap().pool()
    );
    assert_ne!(
        first_pool,
        *Admission::acquire(&peer, system()).unwrap().pool()
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut daemon = controller(&host);
    let client = Client::open(&host.roots().state().join("control/authority")).unwrap();
    for selected in [&selection, &linked, &peer] {
        assert_eq!(
            runtime
                .block_on(client.request(Operation::Register {
                    namespace: Namespace::from(selected.context().unwrap().key()),
                    policy: Policy {
                        dolt: Access::Denied,
                        private_cache_read: Access::Denied,
                        common_cache_read: Access::Denied,
                        egress: Access::Denied,
                        compiler: Access::Denied
                    },
                }))
                .unwrap(),
            Reply::Applied
        );
    }
    let factory =
        Connected::new(Client::open(&host.roots().state().join("control/authority")).unwrap())
            .unwrap();
    platform.admit(&factory);
    let binding = super::seed::binding();
    worker::exercise(
        &Harness {
            platform,
            factory: &factory,
            selection: &selection,
            linked: &linked,
            peer: &peer,
            host: &host,
            runtime: &runtime,
            client: &client,
        },
        &mut daemon,
        &binding,
    );
    stop_controller(&mut daemon);
    let (local, mut owner) = local_owner(&runtime, &host, &selection);
    owner.collect().unwrap();
    super::seed::retained(&owner, &selection, &host);
    let original = owner.runtime_identity().unwrap().to_owned();
    owner.shutdown().unwrap();
    owner.resume().unwrap();
    assert_ne!(owner.runtime_identity().unwrap(), original);
    super::seed::retained(&owner, &selection, &host);
    owner.collect().unwrap();
    owner.shutdown().unwrap();
    drop(owner);
    let mut peer_owner = local.ensure_nix_pool(&peer, system()).unwrap();
    peer_owner.collect().unwrap();
    peer_owner.shutdown().unwrap();
    drop(peer_owner);
    local.close().unwrap();
    observation::verify_resources(&caller.join("native-control"));
}
