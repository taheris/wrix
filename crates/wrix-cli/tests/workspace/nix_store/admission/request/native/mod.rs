//! Actual supervised controller, private IPC and caller-independent native provisioning.
mod client;
mod seed;
mod worker;

use super::*;
use wrix_service::control::{
    Access, Client, Controller, Namespace, Operation, Policy, PoolPreparation, Reply,
};
const REQUESTER: &str = "WRIX_TEST_POOL_REQUESTER";
const NAME: &str = "nix_store::admission::request::native::supervised_controller_prepares_after_requester_exit_and_preserves_native_owner";

fn system() -> System {
    match std::env::consts::ARCH {
        "x86_64" => System::X86_64Linux,
        "aarch64" => System::Aarch64Linux,
        value => panic!("unsupported native test architecture {value}"),
    }
}

#[test]
#[ignore = "requires package-bound native Linux KVM/krun/Podman and eight GiB; run the host-owner CI app"]
fn supervised_controller_prepares_after_requester_exit_and_preserves_native_owner() {
    if let Some(caller) = child() {
        let selection = Selection::inspect(&caller).unwrap();
        let host = wrix_state::host::Host::discover().unwrap();
        let authority = host.roots().state().join("control/authority");
        let directory = caller.parent().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        if std::env::var_os(REQUESTER).is_some() {
            let request = Admission::acquire(&selection, system())
                .unwrap()
                .request()
                .unwrap();
            fs::write(directory.join("intent-ready"), b"issued").unwrap();
            wait_for(&directory.join("send"));
            let client = Client::open(&authority).unwrap();
            let reply = runtime
                .block_on(client.request(Operation::PrepareNixPool {
                    id: request.id().clone(),
                }))
                .unwrap();
            let Reply::PoolQueued { id } = reply else {
                panic!("request was not queued: {reply:?}");
            };
            fs::write(
                directory.join("queued.json"),
                serde_json::to_vec(&id).unwrap(),
            )
            .unwrap();
            return;
        }
        let binary = std::env::var_os("WRIX_TEST_POOL_CONTROLLER")
            .expect("package-selected controller test binary");
        let mut controller = Running(
            std::process::Command::new(binary)
                .arg(authority.parent().unwrap())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + BUDGET;
        while !authority.join("control.sock").exists() {
            assert!(
                controller.0.try_wait().unwrap().is_none(),
                "host controller exited before readiness"
            );
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        let client = Client::open(&authority).unwrap();
        assert_eq!(
            runtime
                .block_on(client.request(Operation::InspectNixPool {}))
                .unwrap(),
            Reply::PoolInterface { version: 1 }
        );
        assert_eq!(
            runtime
                .block_on(client.request(Operation::Register {
                    namespace: Namespace::from(selection.context().unwrap().key()),
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
        let requester = Running(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--ignored", "--exact", NAME, "--nocapture"])
                .env(REQUESTER, "1")
                .spawn()
                .unwrap(),
        );
        wait_for(&directory.join("intent-ready"));
        let second = Admission::acquire(&selection, system())
            .unwrap()
            .request()
            .unwrap();
        // An actual host writer lock keeps the job before consumption/native effects until its caller exits.
        let writer = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(host.roots().state().join("state.lock"))
            .unwrap();
        writer.lock_exclusive().unwrap();
        fs::write(directory.join("send"), b"send").unwrap();
        requester.finish();
        let id: request::Id =
            serde_json::from_slice(&fs::read(directory.join("queued.json")).unwrap()).unwrap();
        assert_eq!(
            runtime
                .block_on(client.request(Operation::InspectNixPoolPreparation { id: id.clone() }))
                .unwrap(),
            Reply::PoolPreparation {
                status: PoolPreparation::Pending
            }
        );
        assert_eq!(
            runtime
                .block_on(client.request(Operation::InspectNixPool {}))
                .unwrap(),
            Reply::PoolInterface { version: 1 }
        );
        assert!(!host.roots().state().join("control/nix").exists());
        assert_eq!(
            runtime
                .block_on(client.request(Operation::PrepareNixPool {
                    id: second.id().clone()
                }))
                .unwrap(),
            Reply::PoolQueued {
                id: second.id().clone()
            }
        );
        drop(writer);
        let deadline = Instant::now() + Duration::from_secs(180);
        for id in [&id, second.id()] {
            loop {
                let reply = runtime
                    .block_on(
                        client.request(Operation::InspectNixPoolPreparation { id: id.clone() }),
                    )
                    .unwrap();
                match reply {
                    Reply::PoolPreparation {
                        status: PoolPreparation::Ready,
                    } => break,
                    Reply::PoolPreparation {
                        status: PoolPreparation::Pending,
                    } => {
                        assert!(
                            Instant::now() < deadline,
                            "native controller preparation deadline"
                        );
                        std::thread::sleep(Duration::from_millis(25));
                    }
                    reply => panic!("native controller preparation failed: {reply:?}"),
                }
            }
        }
        // Exercise the ordinary checked asynchronous API too, not only manually issued protocol requests.
        let refreshed = runtime
            .block_on(client.prepare_nix_pool(&selection, system()))
            .unwrap();
        loop {
            match runtime
                .block_on(client.request(Operation::InspectNixPoolPreparation {
                    id: refreshed.clone(),
                }))
                .unwrap()
            {
                Reply::PoolPreparation {
                    status: PoolPreparation::Ready,
                } => break,
                Reply::PoolPreparation {
                    status: PoolPreparation::Pending,
                } => {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(25));
                }
                reply => panic!("checked preparation failed: {reply:?}"),
            }
        }
        assert!(
            request::claim(&id).is_err(),
            "accepted request was not consumed"
        );
        let pool = Admission::acquire(&selection, system())
            .unwrap()
            .pool()
            .clone();
        let record: serde_json::Value = serde_json::from_slice(
            &fs::read(
                host.roots()
                    .state()
                    .join("control/nix")
                    .join(pool.repository.to_string())
                    .join(pool.system.as_str())
                    .join("backend.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let original = record["state"]["connection"]["target"]["id"]
            .as_str()
            .unwrap();
        client::podman_probe(&runtime, &client, &selection, &host, &mut controller);
        let (ticket, delivery) = client::prepare(&runtime, &client, &selection, &host);
        let (peer_ticket, peer_delivery) = client::prepare(&runtime, &client, &selection, &host);
        wrix_state::execution::cancel(&host, selection.context().unwrap(), &peer_ticket).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let record: serde_json::Value = serde_json::from_slice(
                &fs::read(peer_delivery.store().parent().unwrap().join("client.json")).unwrap(),
            )
            .unwrap();
            if record["state"]["phase"] == "detached" {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "controller did not retire cancelled execution projection"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(
            fs::read_dir(delivery.store()).unwrap().next().is_some(),
            "peer retirement disrupted the live projection"
        );
        let pid =
            rustix::process::Pid::from_raw(i32::try_from(controller.0.id()).unwrap()).unwrap();
        rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();
        assert!(controller.0.wait().unwrap().success());
        let local = {
            let _entered = runtime.enter();
            Controller::bind(&authority).unwrap()
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut owner = loop {
            match local.ensure_nix_pool(&selection, system()) {
                Ok(owner) => break owner,
                Err(wrix_service::control::Error::PoolOwner(
                    wrix_service::nix_store::owner::backend::Error::Busy,
                )) => {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => panic!("recorded owner was not reusable: {error}"),
            }
        };
        assert_eq!(owner.runtime_identity().unwrap(), original);
        client::exercise(&mut owner, &selection, &host, &delivery, &ticket);
        let proof = wrix_state::execution::terminal(
            &host,
            selection.context().unwrap().key(),
            &peer_ticket,
        )
        .unwrap()
        .unwrap();
        owner.release_execution(&proof).unwrap();
        owner.collect().unwrap();
        worker::exercise(&mut owner, &selection, &host, &caller);
        seed::retained(&owner, &selection, &host);
        owner.shutdown().unwrap();
        owner.resume().unwrap();
        seed::retained(&owner, &selection, &host);
        owner.collect().unwrap();
        owner.shutdown().unwrap();
        local.close().unwrap();
        fs::write(directory.join("native-finished"), b"released").unwrap();
        return;
    }
    let (fixture, checkout) = setup();
    let fixture = NativeFixture::new(fixture);
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    let runtime = fixture.root.path().join("runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(
        &runtime,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let mut command = isolation::command(
        fixture.root.path(),
        false,
        &std::env::current_exe().unwrap(),
        &["--ignored", "--exact", NAME, "--nocapture"],
    );
    command
        .env(CALLER, &checkout)
        .env("XDG_RUNTIME_DIR", runtime);
    if let Some(address) = std::env::var_os("DBUS_SESSION_BUS_ADDRESS") {
        command.env("DBUS_SESSION_BUS_ADDRESS", address);
    }
    for name in [
        "WRIX_NIX_POOL_PACKAGE_BINDING",
        "WRIX_NIX_POOL_HOST_TOOLS",
        "WRIX_TEST_POOL_CONTROLLER",
        "WRIX_TEST_POOL_NIX",
        "WRIX_TEST_POOL_BASH",
        "WRIX_TEST_POOL_WORKER_IMAGE",
        "WRIX_TEST_POOL_UNSIGNED",
    ] {
        command.env(
            name,
            std::env::var_os(name).expect("native package test inputs"),
        );
    }
    Running(command.spawn().unwrap()).finish();
    assert!(fixture.root.path().join("native-finished").exists());
    fixture.finish();
}
