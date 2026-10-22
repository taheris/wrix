//! Checked host issuance, not production native endpoints or installed-store runtime proof.

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
