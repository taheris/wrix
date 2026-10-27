//! Focused packaged-controller interruption with two execution-bound native workers.
use super::super::super::interrupt::{self, Phase};
use super::*;
use wrix_sandbox::nix_store::Attempt;

const NAME: &str = "nix_store::admission::request::native::interruption::interrupts_packaged_controller_with_two_live_workers";

#[test]
#[ignore = "requires package-bound native Linux KVM/krun/Podman; run the interruption CI app"]
fn interrupts_packaged_controller_with_two_live_workers() {
    let Some(workspace) = child() else {
        run_fixture(NAME);
        return;
    };
    assert!(
        std::env::var_os(interrupt::NONCE).is_some(),
        "use the automated interruption app"
    );
    let selection = Selection::inspect(&workspace).unwrap();
    let host = wrix_state::host::Host::discover().unwrap();
    let authority = host.roots().state().join("control/authority");
    interrupt::progress(&workspace, Phase::Controller);
    let mut controller = Running(
        std::process::Command::new(std::env::var_os("WRIX_TEST_POOL_CONTROLLER").unwrap())
            .arg(authority.parent().unwrap())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + BUDGET;
    while !authority.join("control.sock").exists() {
        assert!(
            controller.0.try_wait().unwrap().is_none(),
            "packaged controller exited before readiness"
        );
        assert!(
            Instant::now() < deadline,
            "packaged controller did not publish its socket"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let factory =
        wrix_service::nix_store::worker::Connected::new(Client::open(&authority).unwrap()).unwrap();
    interrupt::progress(&workspace, Phase::WorkerImage);
    let binding = seed::binding();
    let mut workers = Vec::new();
    for (name, phase) in [
        ("first", Phase::FirstWorker),
        ("second", Phase::SecondWorker),
    ] {
        interrupt::progress(&workspace, phase);
        let prepared = Attempt::reserve(&selection, system(), binding.clone(), None)
            .unwrap()
            .prepare(&factory)
            .unwrap();
        let mut command = prepared.command().unwrap();
        command.args(["run", "--rm", "--cap-add=NET_ADMIN", "--memory=768m", "--pids-limit=256", "--network=pasta:--map-host-loopback,169.254.1.2,--map-guest-addr,none,-t,none,-u,none,-T,none,-U,none", "--mount", "type=tmpfs,destination=/home/wrix,U=true"])
            .arg("-v").arg(format!("{}:/workspace:rw", workspace.display()))
            .args(["-e", "HOME=/home/wrix", "-e", "WRIX_AGENT=direct", "-e", "WRIX_NETWORK=open"]);
        prepared.delivery().configure(&mut command);
        command.arg(binding.digest().as_str()).arg(name);
        workers.push(prepared.spawn(&command, false).unwrap());
    }
    interrupt::progress(&workspace, Phase::WorkerEvaluations);
    let deadline = Instant::now() + Duration::from_secs(180);
    while !workspace.join("first-ready").exists() || !workspace.join("second-ready").exists() {
        assert!(
            controller.0.try_wait().unwrap().is_none(),
            "packaged controller exited before worker evaluations"
        );
        for worker in &mut workers {
            assert!(
                worker.child().try_wait().unwrap().is_none(),
                "native worker exited before both evaluations; inspect native.log"
            );
        }
        assert!(
            Instant::now() < deadline,
            "native workers did not evaluate before the deadline"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    for worker in &mut workers {
        assert!(worker.child().try_wait().unwrap().is_none());
    }
    assert!(controller.0.try_wait().unwrap().is_none());
    interrupt::pause(&workspace);
    panic!("interruption checkpoint returned without termination");
}
