//! Checked host issuance, not production native endpoints or installed-store runtime proof.

#[path = "admission/cleanup.rs"]
mod cleanup;
#[path = "admission/interrupt.rs"]
mod interrupt;
#[path = "admission/request/mod.rs"]
mod request;
use cleanup::NativeFixture;

use super::{deployment, setup, success};
use crate::fixture::{Fixture, isolation};
use fs2::FileExt;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Stdio},
    time::{Duration, Instant},
};
use wrix_core::nix_store::pool::System;
use wrix_state::{context::Selection, nix_store::admission::Guard as Admission};

const CALLER: &str = "WRIX_TEST_POOL_ADMISSION_CALLER";
const READY: &str = "WRIX_TEST_POOL_ADMISSION_READY";
const RELEASE: &str = "WRIX_TEST_POOL_ADMISSION_RELEASE";
const BUDGET: Duration = Duration::from_secs(60);

struct Running(Child);

impl Running {
    fn finish(mut self) {
        let output = self.0.wait().unwrap();
        assert!(output.success(), "admission subprocess: {output}");
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            #[cfg(target_os = "linux")]
            {
                let pid = rustix::process::Pid::from_raw(self.0.id().try_into().unwrap()).unwrap();
                // Only an explicitly isolated child group belongs to this guard.
                // Keep its unreaped leader until after signaling, preventing PID reuse.
                if rustix::process::getpgid(Some(pid)).unwrap() == pid {
                    match rustix::process::kill_process_group(pid, rustix::process::Signal::KILL) {
                        Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                        Err(error) => {
                            panic!("could not stop native assertion helper group: {error}")
                        }
                    }
                    self.0.wait().unwrap();
                    return;
                }
            }
            self.0.kill().unwrap();
            self.0.wait().unwrap();
        }
    }
}

fn child() -> Option<PathBuf> {
    std::env::var_os(CALLER).map(PathBuf::from)
}

fn start(fixture: &Fixture, checkout: &Path, name: &str) -> Running {
    let current = std::env::current_exe().unwrap();
    let executable = if fixture.isolated {
        let destination = Path::new("/bin/admission-tests");
        isolation::install_executable(fixture.root.path(), &current, destination);
        destination
    } else {
        &current
    };
    let mut command = isolation::command(
        fixture.root.path(),
        fixture.isolated,
        executable,
        &["--include-ignored", "--exact", name, "--nocapture"],
    );
    command
        .env(CALLER, checkout)
        .env(READY, fixture.inside("ready"))
        .env(RELEASE, fixture.inside("release"));
    Running(command.spawn().unwrap())
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + BUDGET;
    while !path.exists() {
        assert!(Instant::now() < deadline, "timed out waiting for {path:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn ready_and_wait() {
    fs::write(std::env::var_os(READY).unwrap(), b"ready").unwrap();
    wait_for(Path::new(&std::env::var_os(RELEASE).unwrap()));
}

#[test]
fn pool_issuance_serializes_approval_withdrawal_until_its_guard_drops() {
    if let Some(caller) = child() {
        let selection = Selection::inspect(&caller).unwrap();
        let admission = Admission::acquire(&selection, System::X86_64Linux).unwrap();
        assert_eq!(admission.namespace(), selection.context().unwrap().key());
        ready_and_wait();
        admission.recheck().unwrap();
        return;
    }
    let (fixture, checkout) = setup();
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    let issuer = start(
        &fixture,
        &checkout,
        "nix_store::admission::pool_issuance_serializes_approval_withdrawal_until_its_guard_drops",
    );
    wait_for(&fixture.root.path().join("ready"));
    let lock = fs::File::open(fixture.state().join("state.lock")).unwrap();
    assert_eq!(
        lock.try_lock_exclusive().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let mut disable = fixture.application_command(&checkout, &["service", "nix-store", "disable"]);
    disable.stdout(Stdio::piped()).stderr(Stdio::piped());
    let disable = Running(disable.spawn().unwrap());
    fs::write(fixture.root.path().join("release"), b"release").unwrap();
    issuer.finish();
    disable.finish();
    assert!(success(&fixture, &checkout, &[]).contains("approval: disabled"));
}

#[test]
fn pool_issuance_rechecks_original_repository_administration_before_effects() {
    if let Some(caller) = child() {
        let selection = Selection::inspect(&caller).unwrap();
        let admission = Admission::acquire(&selection, System::X86_64Linux).unwrap();
        ready_and_wait();
        assert!(admission.recheck().is_err());
        return;
    }
    let (fixture, checkout) = setup();
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    let issuer = start(
        &fixture,
        &checkout,
        "nix_store::admission::pool_issuance_rechecks_original_repository_administration_before_effects",
    );
    wait_for(&fixture.root.path().join("ready"));
    let real_checkout = fixture.root.path().join("checkout");
    fs::rename(
        real_checkout.join(".git"),
        real_checkout.join(".git-retained"),
    )
    .unwrap();
    assert!(fixture.git(&checkout, &["init", "-q"]).status.success());
    fs::write(fixture.root.path().join("release"), b"release").unwrap();
    issuer.finish();
    assert!(!fixture.state().join("service-data/nix").exists());
}

#[test]
fn pool_issuance_rechecks_the_original_integrated_primary_attestation() {
    if let Some(caller) = child() {
        let selection = Selection::inspect(&caller).unwrap();
        assert!(selection.context().unwrap().integrated());
        let admission = Admission::acquire(&selection, System::X86_64Linux).unwrap();
        ready_and_wait();
        assert!(admission.recheck().is_err());
        return;
    }
    let (fixture, checkout) = setup();
    let primary = fixture.repository("primary");
    let replacement = fixture.repository("replacement");
    fixture.adapter(&checkout, &primary, "pool-member");
    assert!(
        fixture
            .admit(&checkout, "pool-member", &["--allocate"])
            .status
            .success()
    );
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    let issuer = start(
        &fixture,
        &checkout,
        "nix_store::admission::pool_issuance_rechecks_the_original_integrated_primary_attestation",
    );
    wait_for(&fixture.root.path().join("ready"));
    let response = serde_json::json!({
        "version": 1,
        "membership": "pool-member",
        "checkout_root_b64": wrix_state::resolver::encode(&checkout),
        "primary_root_b64": wrix_state::resolver::encode(&replacement),
    });
    let bash = fixture.install("bash");
    fs::write(fixture.root.path().join("adapter"), format!(
        "#!{}\nset -euo pipefail\nIFS= read -r request || [[ -n \"$request\" ]]\nprintf '%s' '{}'\n",
        bash.display(), response.to_string().replace('\'', "'\\''")
    )).unwrap();
    fs::write(fixture.root.path().join("release"), b"release").unwrap();
    issuer.finish();
    assert!(!fixture.state().join("service-data/nix").exists());
}

#[test]
fn controller_pool_issuance_requires_registration_and_keeps_repository_system_scopes_disjoint() {
    if let Some(caller) = child() {
        use wrix_service::control::{
            Access, Client, Controller, Error, Namespace, Operation, Policy, Reply,
        };
        let selection = Selection::inspect(&caller).unwrap();
        let host = wrix_state::host::Host::discover().unwrap();
        let root = host.roots().state().join("control/authority");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let controller = {
            let _entered = runtime.enter();
            Controller::bind(&root).unwrap()
        };
        assert!(matches!(
            controller.authorize_nix_pool(&selection, System::X86_64Linux),
            Err(Error::UnknownNamespace)
        ));
        let client = Client::open(&root).unwrap();
        let policy = Policy {
            dolt: Access::Denied,
            private_cache_read: Access::Denied,
            common_cache_read: Access::Denied,
            egress: Access::Denied,
            compiler: Access::Denied,
        };
        let namespace = Namespace::from(selection.context().unwrap().key());
        let (reply, serving) = runtime.block_on(async {
            tokio::join!(
                client.request(Operation::Register { namespace, policy }),
                controller.serve_one()
            )
        });
        assert_eq!(reply.unwrap(), Reply::Applied);
        serving.unwrap();
        let grant = controller
            .authorize_nix_pool(&selection, System::X86_64Linux)
            .unwrap();
        grant.recheck().unwrap();
        assert_eq!(grant.namespace(), selection.context().unwrap().key());
        let original = grant.pool().clone();
        drop(grant);
        let arm = controller
            .authorize_nix_pool(&selection, System::Aarch64Linux)
            .unwrap();
        assert_eq!(original.repository, arm.pool().repository);
        assert_ne!(original.system, arm.pool().system);
        drop(arm);
        let peer = Selection::inspect(&caller.parent().unwrap().join("peer")).unwrap();
        assert!(matches!(
            controller.authorize_nix_pool(&peer, System::X86_64Linux),
            Err(Error::UnknownNamespace)
        ));
        let namespace = Namespace::from(peer.context().unwrap().key());
        let (reply, serving) = runtime.block_on(async {
            tokio::join!(
                client.request(Operation::Register { namespace, policy }),
                controller.serve_one()
            )
        });
        assert_eq!(reply.unwrap(), Reply::Applied);
        serving.unwrap();
        let peer_grant = controller
            .authorize_nix_pool(&peer, System::X86_64Linux)
            .unwrap();
        assert_ne!(original.repository, peer_grant.pool().repository);
        drop(peer_grant);
        let unbound = Selection::inspect(&caller.parent().unwrap().join("unbound")).unwrap();
        assert!(matches!(
            controller.authorize_nix_pool(&unbound, System::X86_64Linux),
            Err(Error::PoolAdmission(
                wrix_state::nix_store::Error::ApprovalRequired
            ))
        ));
        controller.close().unwrap();
        fs::write(std::env::var_os(READY).unwrap(), b"done").unwrap();
        return;
    }
    let (fixture, checkout) = setup();
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    let peer = fixture.repository("peer");
    success(&fixture, &peer, &["enable"]);
    fixture.repository("unbound");
    start(&fixture, &checkout,
        "nix_store::admission::controller_pool_issuance_requires_registration_and_keeps_repository_system_scopes_disjoint").finish();
    assert!(fixture.root.path().join("ready").exists());
    assert!(!fixture.state().join("service-data/nix").exists());
}

#[test]
fn controller_reservation_requires_checked_registration_and_retains_uncertainty_after_restart() {
    if let Some(caller) = child() {
        use wrix_service::control::{
            Access, Client, Controller, Error, Namespace, Operation, Policy, Reply,
        };
        let selection = Selection::inspect(&caller).unwrap();
        let host = wrix_state::host::Host::discover().unwrap();
        let root = host.roots().state().join("control/authority");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let controller = {
            let _entered = runtime.enter();
            Controller::bind(&root).unwrap()
        };
        assert!(matches!(
            controller.reserve_nix_pool(&selection, System::X86_64Linux),
            Err(Error::UnknownNamespace)
        ));
        assert!(!host.roots().state().join("service-data/nix").exists());
        let client = Client::open(&root).unwrap();
        let policy = Policy {
            dolt: Access::Denied,
            private_cache_read: Access::Denied,
            common_cache_read: Access::Denied,
            egress: Access::Denied,
            compiler: Access::Denied,
        };
        let namespace = Namespace::from(selection.context().unwrap().key());
        let (reply, serving) = runtime.block_on(async {
            tokio::join!(
                client.request(Operation::Register { namespace, policy }),
                controller.serve_one()
            )
        });
        assert_eq!(reply.unwrap(), Reply::Applied);
        serving.unwrap();
        let reservation = controller
            .reserve_nix_pool(&selection, System::X86_64Linux)
            .unwrap();
        let pool = reservation.pool().clone();
        let control = host
            .roots()
            .state()
            .join("control/nix")
            .join(pool.repository.to_string())
            .join(pool.system.as_str());
        let before = fs::read(control.join("reservation.json")).unwrap();
        let lock = fs::File::open(control.join("ownership")).unwrap();
        assert_eq!(
            FileExt::try_lock_exclusive(&lock).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        wrix_state::nix_store::set(&host, selection.context().unwrap(), false).unwrap();
        assert!(matches!(
            controller.reserve_nix_pool(&selection, System::Aarch64Linux),
            Err(Error::PoolAdmission(_))
        ));
        wrix_state::nix_store::set(&host, selection.context().unwrap(), true).unwrap();
        drop(reservation);
        controller.close().unwrap();
        let controller = {
            let _entered = runtime.enter();
            Controller::bind(&root).unwrap()
        };
        assert!(matches!(
            controller.reserve_nix_pool(&selection, System::X86_64Linux),
            Err(Error::PoolStorage(
                wrix_service::nix_store::owner::Error::Retained
            ))
        ));
        assert_eq!(fs::read(control.join("reservation.json")).unwrap(), before);
        assert!(
            !host
                .roots()
                .state()
                .join("service-data/nix")
                .join(pool.repository.to_string())
                .join(pool.system.as_str())
                .join("store.ext4")
                .exists()
        );
        controller.close().unwrap();
        fs::write(std::env::var_os(READY).unwrap(), b"retained").unwrap();
        return;
    }
    let (fixture, checkout) = setup();
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    start(&fixture, &checkout, "nix_store::admission::controller_reservation_requires_checked_registration_and_retains_uncertainty_after_restart").finish();
    assert!(fixture.root.path().join("ready").exists());
    let before = super::snapshot(&fixture.state());
    let information = success(&fixture, &checkout, &[]);
    assert!(information.contains(
        "x86_64-linux: host storage reserved; native ownership/readiness not established"
    ));
    assert_eq!(super::snapshot(&fixture.state()), before);
}

#[test]
#[ignore = "requires rootless Podman, KVM, package-bound pool artifacts and nine GiB of free storage"]
fn native_host_owner_survives_controller_loss_collects_and_resumes_recorded_software() {
    let system = if cfg!(target_arch = "x86_64") {
        System::X86_64Linux
    } else {
        System::Aarch64Linux
    };
    if let Some(caller) = child() {
        use wrix_service::{
            control::{Access, Client, Controller, Namespace, Operation, Policy, Reply},
            nix_store::native,
        };
        let selection = Selection::inspect(&caller).unwrap();
        let host = wrix_state::host::Host::discover().unwrap();
        let root = host.roots().state().join("control/authority");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let controller = {
            let _entered = runtime.enter();
            Controller::bind(&root).unwrap()
        };
        let client = Client::open(&root).unwrap();
        let namespace = Namespace::from(selection.context().unwrap().key());
        let policy = Policy {
            dolt: Access::Denied,
            private_cache_read: Access::Denied,
            common_cache_read: Access::Denied,
            egress: Access::Denied,
            compiler: Access::Denied,
        };
        let (reply, serving) = runtime.block_on(async {
            tokio::join!(
                client.request(Operation::Register { namespace, policy }),
                controller.serve_one()
            )
        });
        assert_eq!(reply.unwrap(), Reply::Applied);
        serving.unwrap();
        let owner = controller.ensure_nix_pool(&selection, system).unwrap();
        assert!(matches!(
            owner.inspect().unwrap(),
            native::Reply::Ready { version: 1, .. }
        ));
        let original = owner.runtime_identity().unwrap().to_owned();
        drop(owner);
        controller.close().unwrap();
        ready_and_wait();
        let controller = {
            let _entered = runtime.enter();
            Controller::bind(&root).unwrap()
        };
        let mut owner = controller.ensure_nix_pool(&selection, system).unwrap();
        assert_eq!(owner.runtime_identity().unwrap(), original);
        owner.shutdown().unwrap();
        fs::write(caller.parent().unwrap().join("stopped"), b"released").unwrap();
        wait_for(&caller.parent().unwrap().join("resume"));
        owner.resume().unwrap();
        assert_ne!(owner.runtime_identity().unwrap(), original);
        assert!(matches!(
            owner.inspect().unwrap(),
            native::Reply::Ready { version: 1, .. }
        ));
        owner.collect().unwrap();
        owner.shutdown().unwrap();
        controller.close().unwrap();
        fs::write(caller.parent().unwrap().join("finished"), b"released").unwrap();
        return;
    }
    let (fixture, checkout) = setup();
    let fixture = NativeFixture::new(fixture);
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    let runtime_directory = fixture.root.path().join("runtime");
    fs::create_dir(&runtime_directory).unwrap();
    fs::set_permissions(
        &runtime_directory,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let mut command = isolation::command(
        fixture.root.path(),
        false,
        &std::env::current_exe().unwrap(),
        &[
            "--ignored",
            "--exact",
            "nix_store::admission::native_host_owner_survives_controller_loss_collects_and_resumes_recorded_software",
            "--nocapture",
        ],
    );
    command
        .env(CALLER, &checkout)
        .env(READY, fixture.root.path().join("ready"))
        .env(RELEASE, fixture.root.path().join("release"))
        .env("XDG_RUNTIME_DIR", &runtime_directory);
    if let Some(address) = std::env::var_os("DBUS_SESSION_BUS_ADDRESS") {
        command.env("DBUS_SESSION_BUS_ADDRESS", address);
    }
    for name in ["WRIX_NIX_POOL_PACKAGE_BINDING", "WRIX_NIX_POOL_HOST_TOOLS"] {
        command.env(
            name,
            std::env::var_os(name).expect("package-selected native pool inputs required"),
        );
    }
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut running = Running(command.spawn().unwrap());
    let deadline = Instant::now() + BUDGET;
    while !fixture.root.path().join("ready").exists() {
        if let Some(status) = running.0.try_wait().unwrap() {
            panic!(
                "native owner probe exited {status}; retained fixture: {:?}",
                fixture.root.path()
            );
        }
        assert!(
            Instant::now() < deadline,
            "native owner readiness deadline; retained fixture: {:?}",
            fixture.root.path()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let information = success(&fixture, &checkout, &[]);
    assert!(information.contains("recorded native owner serving; readiness not probed"));
    success(&fixture, &checkout, &["disable"]);
    let mut collect =
        fixture.application_command(&checkout, &["service", "nix-store", "gc", "--wait", "0s"]);
    collect.env("XDG_RUNTIME_DIR", &runtime_directory);
    let output = collect.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains(&format!("{}: collected", system.as_str()))
    );
    success(&fixture, &checkout, &["enable"]);
    fs::write(fixture.root.path().join("release"), b"release").unwrap();
    wait_for(&fixture.root.path().join("stopped"));
    let output = collect.output().unwrap();
    assert!(!output.status.success());
    fs::write(fixture.root.path().join("resume"), b"resume").unwrap();
    running.finish();
    assert!(fixture.root.path().join("finished").exists());
    fixture.finish();
}

#[test]
#[ignore = "requires the worker chroot fixture; run explicitly in the native guest"]
fn worker_pool_issuance_denies_library_access_despite_forged_home_and_existing_approval() {
    if let Some(caller) = child() {
        let selection = Selection::inspect(&caller).unwrap();
        assert!(selection.context().is_none());
        assert!(matches!(
            Admission::acquire(&selection, System::X86_64Linux),
            Err(wrix_state::nix_store::Error::Host(
                wrix_state::host::Error::Worker
            ))
        ));
        fs::write(std::env::var_os(READY).unwrap(), b"denied").unwrap();
        return;
    }
    let (fixture, checkout) = setup();
    assert!(fixture.isolated, "requires the worker chroot fixture");
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    let marker = fixture.root.path().join("etc/wrix");
    fs::create_dir_all(&marker).unwrap();
    fs::write(marker.join("image-agent"), b"direct\n").unwrap();
    let before = super::snapshot(&fixture.state());
    start(&fixture, &checkout,
        "nix_store::admission::worker_pool_issuance_denies_library_access_despite_forged_home_and_existing_approval").finish();
    assert!(fixture.root.path().join("ready").exists());
    assert_eq!(super::snapshot(&fixture.state()), before);
}
