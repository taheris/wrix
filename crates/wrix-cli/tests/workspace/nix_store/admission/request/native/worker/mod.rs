//! Real independent Podman workers through their unchanged network/bootstrap and execution containment.
use super::*;
use std::process::{Command, Stdio};
use wrix_sandbox::execution::{Prepared, ProjectionMount, Running};
use wrix_service::nix_store::owner::backend::{
    Owner,
    client::Delivery,
    seed::{Image, Outcome},
};

struct Worker {
    ticket: wrix_state::execution::Ticket,
    delivery: Delivery,
    execution: Running,
}
fn launch(
    owner: &mut Owner,
    selection: &Selection,
    host: &wrix_state::host::Host,
    workspace: &Path,
    name: &str,
) -> Worker {
    let binding = super::seed::binding();
    let ticket = wrix_state::execution::reserve(host, selection.context().unwrap()).unwrap();
    let request = Admission::acquire(selection, system())
        .unwrap()
        .request_podman_worker(ticket.clone(), binding.clone())
        .unwrap();
    let issued = request::claim(request.id()).unwrap();
    let digest = wrix_sandbox::image::Digest::parse(binding.digest().as_str()).unwrap();
    let image = Image::inspect(system(), &digest, binding.metadata()).unwrap();
    assert_eq!(
        owner.seed_execution(&issued, &image).unwrap(),
        Outcome::Reused
    );
    let delivery = owner.admit_execution(&issued).unwrap();
    let directory = delivery.podman_configuration().unwrap();
    let mut run = Command::new("podman");
    run.args(["run", "--rm", "--cap-add=NET_ADMIN", "--memory=768m", "--pids-limit=256", "--network=pasta:--map-host-loopback,169.254.1.2,--map-guest-addr,none,-t,none,-u,none,-T,none,-U,none", "--mount", "type=tmpfs,destination=/home/wrix,U=true"])
        .arg("-v").arg(format!("{}:/workspace:rw", workspace.display()))
        .arg("-v").arg(format!("{}:/nix/store:ro", delivery.store().display()))
        .arg("-v").arg(format!("{}:/run/wrix/nix-pool:ro", directory.display()))
        .arg("-v").arg(format!("{}:/etc/nix/nix.conf:ro", directory.join("nix.conf").display()))
        .args(["-e", "HOME=/home/wrix", "-e", "WRIX_AGENT=direct", "-e", "WRIX_NETWORK=open"])
        .arg("-e").arg(format!("WRIX_NETWORK_LOCAL_ENDPOINTS=169.254.1.2:{}/tcp", delivery.port()))
        .arg(binding.digest().as_str()).arg(name);
    let handoff = Admission::acquire(selection, system())
        .unwrap()
        .handoff(ticket.clone())
        .unwrap();
    let mounts = [
        ProjectionMount {
            source: delivery.store().to_path_buf(),
            destination: PathBuf::from("/nix/store"),
            writable: false,
        },
        ProjectionMount {
            source: directory.clone(),
            destination: PathBuf::from("/run/wrix/nix-pool"),
            writable: false,
        },
        ProjectionMount {
            source: directory.join("nix.conf"),
            destination: PathBuf::from("/etc/nix/nix.conf"),
            writable: false,
        },
    ];
    let prepared = Prepared::create(handoff, &run, false, &digest, &mounts).unwrap();
    assert_eq!(prepared.ticket(), &ticket);
    let log = fs::File::create(workspace.join(format!("{name}-runtime.log"))).unwrap();
    let execution = prepared
        .stdio(
            Stdio::null(),
            Stdio::from(log.try_clone().unwrap()),
            Stdio::from(log),
        )
        .spawn()
        .unwrap();
    Worker {
        ticket,
        delivery,
        execution,
    }
}
fn await_file(workspace: &Path, file: &str, workers: &mut [&mut Worker]) {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(180);
    while !workspace.join(file).exists() {
        for worker in workers.iter_mut() {
            assert!(
                worker.execution.child().try_wait().unwrap().is_none(),
                "worker exited before {file}; inspect retained runtime logs in {}",
                workspace.display()
            );
        }
        assert!(
            Instant::now() < deadline,
            "worker did not produce {file}; logs in {}",
            workspace.display()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    eprintln!("native worker milestone {file}: {:?}", started.elapsed());
}
fn retire(
    owner: &mut Owner,
    selection: &Selection,
    host: &wrix_state::host::Host,
    workspace: &Path,
    name: &str,
    worker: &mut Worker,
) -> wrix_state::execution::Terminal {
    fs::write(workspace.join(format!("{name}-exit")), b"exit").unwrap();
    assert!(worker.execution.child().wait().unwrap().success());
    retire_containment(owner, selection, host, worker)
}
fn retire_containment(
    owner: &mut Owner,
    selection: &Selection,
    host: &wrix_state::host::Host,
    worker: &Worker,
) -> wrix_state::execution::Terminal {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match worker.execution.reconcile().unwrap() {
            wrix_sandbox::execution::Observation::Retired => break,
            wrix_sandbox::execution::Observation::Protected => {
                assert!(
                    Instant::now() < deadline,
                    "native worker containment did not become authoritatively inactive"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }
    let terminal =
        wrix_state::execution::terminal(host, selection.context().unwrap().key(), &worker.ticket)
            .unwrap()
            .unwrap();
    match owner.release_execution(&terminal) {
        Ok(()) | Err(wrix_service::nix_store::owner::backend::Error::Busy) => (),
        Err(error) => panic!(
            "native worker release failed: {error}; owner={:?}; activity={:?}",
            owner.inspect(),
            owner.activity()
        ),
    }
    assert!(worker.delivery.podman_configuration().is_err());
    terminal
}
pub(super) fn exercise(
    owner: &mut Owner,
    selection: &Selection,
    host: &wrix_state::host::Host,
    workspace: &Path,
) {
    let archive = fs::File::create(workspace.join("unsigned.nar")).unwrap();
    let exported = Command::new("nix-store")
        .env_remove("NIX_REMOTE")
        .arg("--export")
        .arg(std::env::var_os("WRIX_TEST_POOL_UNSIGNED").unwrap())
        .stdout(archive)
        .status()
        .unwrap();
    assert!(exported.success());
    let control = PathBuf::from(std::env::var_os("HOME").unwrap()).join("worker-escape-control");
    fs::write(&control, b"host-only-readable-control").unwrap();
    assert_eq!(fs::read(&control).unwrap(), b"host-only-readable-control");
    fs::write(
        workspace.join("escape-target"),
        control.as_os_str().as_encoded_bytes(),
    )
    .unwrap();
    let mut first = launch(owner, selection, host, workspace, "first");
    let mut second = launch(owner, selection, host, workspace, "second");
    await_file(workspace, "first-ready", &mut [&mut first, &mut second]);
    await_file(workspace, "second-ready", &mut [&mut first, &mut second]);
    assert!(matches!(
        owner.inspect().unwrap(),
        wrix_service::nix_store::native::Reply::Ready { .. }
    ));
    fs::write(workspace.join("workers-go"), b"go").unwrap();
    await_file(workspace, "first-done", &mut [&mut first, &mut second]);
    await_file(workspace, "second-done", &mut [&mut first, &mut second]);
    assert_eq!(
        fs::read_link(workspace.join("first-result")).unwrap(),
        fs::read_link(workspace.join("second-result")).unwrap()
    );
    assert_eq!(
        fs::read_to_string(workspace.join("first-run")).unwrap(),
        "installed\n"
    );
    assert!(matches!(
        wrix_sandbox::execution::reconcile(selection.context().unwrap().caller(), &first.ticket)
            .unwrap(),
        wrix_sandbox::execution::Observation::Protected
    ));
    assert!(matches!(
        owner.collect(),
        Err(wrix_service::nix_store::owner::backend::Error::Busy)
    ));
    let first_proof = retire(owner, selection, host, workspace, "first", &mut first);
    assert!(second.execution.child().try_wait().unwrap().is_none());
    let mut replacement = launch(owner, selection, host, workspace, "replacement");
    await_file(
        workspace,
        "replacement-done",
        &mut [&mut second, &mut replacement],
    );
    assert_eq!(
        fs::read_link(workspace.join("first-result")).unwrap(),
        fs::read_link(workspace.join("replacement-result")).unwrap()
    );
    replacement.execution.child().kill().unwrap();
    assert!(!replacement.execution.child().wait().unwrap().success());
    assert!(matches!(
        wrix_sandbox::execution::reconcile(
            selection.context().unwrap().caller(),
            &replacement.ticket
        )
        .unwrap(),
        wrix_sandbox::execution::Observation::Protected
    ));
    assert!(matches!(
        owner.collect(),
        Err(wrix_service::nix_store::owner::backend::Error::Busy)
    ));
    let second_proof = retire(owner, selection, host, workspace, "second", &mut second);
    let projection = wrix_sandbox::execution::projection(
        selection.context().unwrap().caller(),
        &replacement.ticket,
    )
    .unwrap();
    fs::write(workspace.join("replacement-exit"), b"exit").unwrap();
    let output = Command::new("podman")
        .args(["wait", projection.id.as_str()])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"0\n");
    let replacement_proof = retire_containment(owner, selection, host, &replacement);
    for proof in [first_proof, second_proof, replacement_proof] {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match owner.release_execution(&proof) {
                Ok(()) => break,
                Err(wrix_service::nix_store::owner::backend::Error::Busy) => {
                    assert!(
                        Instant::now() < deadline,
                        "native work did not drain after all workers were removed: {:?}",
                        owner.activity()
                    );
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => panic!(
                    "native worker drain failed: {error}; owner={:?}; activity={:?}",
                    owner.inspect(),
                    owner.activity()
                ),
            }
        }
    }
    owner.collect().unwrap();
}
