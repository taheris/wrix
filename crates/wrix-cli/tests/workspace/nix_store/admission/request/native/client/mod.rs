//! Real execution-bound untrusted SSH client, read-only FUSE view and terminal native release.
use super::*;

pub(super) fn prepare(
    runtime: &tokio::runtime::Runtime,
    client: &Client,
    selection: &Selection,
    host: &wrix_state::host::Host,
) -> (
    wrix_state::execution::Ticket,
    wrix_service::nix_store::owner::backend::client::Delivery,
) {
    prepare_on(runtime, client, selection, host, request::Plane::Host)
}

fn prepare_on(
    runtime: &tokio::runtime::Runtime,
    client: &Client,
    selection: &Selection,
    host: &wrix_state::host::Host,
    plane: request::Plane,
) -> (
    wrix_state::execution::Ticket,
    wrix_service::nix_store::owner::backend::client::Delivery,
) {
    let ticket = wrix_state::execution::reserve(host, selection.context().unwrap()).unwrap();
    let binding = super::seed::binding();
    let id = match plane {
        request::Plane::Host => {
            runtime.block_on(client.prepare_nix_worker(selection, system(), &ticket, &binding))
        }
        request::Plane::Podman => runtime.block_on(client.prepare_nix_podman_worker(
            selection,
            system(),
            &ticket,
            &binding,
        )),
    }
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        match runtime
            .block_on(client.request(Operation::InspectNixExecutionDelivery { id: id.clone() }))
            .unwrap()
        {
            Reply::PoolDelivery { delivery } => {
                assert_eq!(delivery.ticket(), &ticket);
                return (ticket, delivery);
            }
            Reply::PoolPreparation {
                status: PoolPreparation::Pending,
            } => {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(25));
            }
            reply => panic!("execution preparation failed: {reply:?}"),
        }
    }
}

pub(super) fn podman_probe(
    runtime: &tokio::runtime::Runtime,
    client: &Client,
    selection: &Selection,
    host: &wrix_state::host::Host,
) {
    let namespace = wrix_service::control::Namespace::from(selection.context().unwrap().key());
    assert_eq!(
        runtime
            .block_on(client.request(Operation::Retire {
                namespace: namespace.clone()
            }))
            .unwrap(),
        Reply::Applied
    );
    let factory = wrix_service::nix_store::worker::Connected::new(
        Client::open(&host.roots().state().join("control/authority")).unwrap(),
    )
    .unwrap();
    let attempt = wrix_sandbox::nix_store::Attempt::reserve(
        selection,
        system(),
        super::seed::binding(),
        None,
    )
    .unwrap();
    let ticket = attempt.ticket().clone();
    let prepared = attempt.prepare(&factory).unwrap();
    let mounts = prepared.delivery().mounts();
    let store = &mounts[0].source;
    let Reply::Snapshot { namespaces, .. } = runtime
        .block_on(client.request(Operation::Inspect {}))
        .unwrap()
    else {
        panic!("missing registry snapshot")
    };
    assert_eq!(
        namespaces[&namespace],
        wrix_service::control::Policy {
            dolt: wrix_service::control::Access::Denied,
            private_cache_read: wrix_service::control::Access::Denied,
            common_cache_read: wrix_service::control::Access::Denied,
            egress: wrix_service::control::Access::Denied,
            compiler: wrix_service::control::Access::Denied
        }
    );
    assert!(
        fs::read_dir(store).unwrap().next().is_none(),
        "Podman source propagated into the host namespace"
    );
    assert_eq!(
        wrix_state::execution::observe(host, selection.context().unwrap(), &ticket).unwrap(),
        Some(wrix_state::execution::Phase::Reserved)
    );
    drop(prepared);
    assert_eq!(
        wrix_state::execution::observe(host, selection.context().unwrap(), &ticket).unwrap(),
        Some(wrix_state::execution::Phase::Cancelled)
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(store.parent().unwrap().join("client.json")).unwrap())
                .unwrap();
        if record["state"]["phase"] == "released" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "private controller did not retire its Podman-namespace source"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(fs::read_dir(store).unwrap().next().is_none());

    // A second programmatic reservation follows the same factory, renderer, atomic handoff and native start path.
    let ticket = wrix_state::execution::reserve(host, selection.context().unwrap()).unwrap();
    let binding = super::seed::binding();
    let prepared = wrix_sandbox::nix_store::Attempt::reserve(
        selection,
        system(),
        binding.clone(),
        Some(&ticket),
    )
    .unwrap()
    .prepare(&factory)
    .unwrap();
    let mounts = prepared.delivery().mounts();
    let root = mounts[0].source.parent().unwrap();
    let mut command = prepared.command().unwrap();
    command.args(["run", "--rm", "--cap-add=NET_ADMIN", "--memory=768m", "--pids-limit=256", "--network=pasta:--map-host-loopback,169.254.1.2,--map-guest-addr,none,-t,none,-u,none,-T,none,-U,none", "--mount", "type=tmpfs,destination=/home/wrix,U=true", "-e", "HOME=/home/wrix", "-e", "WRIX_AGENT=direct", "-e", "WRIX_NETWORK=open"]);
    let notification = host.roots().state().join("probe-notify");
    fs::create_dir(&notification).unwrap();
    let _listener =
        std::os::unix::net::UnixListener::bind(notification.join("notify.sock")).unwrap();
    command.arg("-v").arg(format!(
        "{}:/run/wrix/notify.sock:ro",
        notification.join("notify.sock").display()
    ));
    prepared.delivery().configure(&mut command);
    command.arg(binding.digest().as_str()).arg("factory-probe");
    let mut running = prepared.spawn(&command, false).unwrap();
    assert!(running.child().wait().unwrap().success());
    assert!(!notification.join("nix-client").exists());
    assert!(!notification.join("nix-pool").exists());
    assert_eq!(
        running.reconcile().unwrap(),
        wrix_sandbox::execution::Observation::Retired
    );
    assert!(
        wrix_state::execution::terminal(host, selection.context().unwrap().key(), &ticket)
            .unwrap()
            .is_some()
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("client.json")).unwrap()).unwrap();
        if record["state"]["phase"] == "released" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "native factory worker roots were not authoritatively retired"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub(super) fn exercise(
    owner: &mut wrix_service::nix_store::owner::backend::Owner,
    selection: &Selection,
    host: &wrix_state::host::Host,
    delivery: &wrix_service::nix_store::owner::backend::client::Delivery,
    ticket: &wrix_state::execution::Ticket,
) {
    let context = selection.context().unwrap();
    let request = Admission::acquire(selection, system())
        .unwrap()
        .request_execution(ticket.clone())
        .unwrap();
    let accepted = request::claim(request.id()).unwrap();
    super::seed::exercise(owner, &accepted, delivery);
    assert_eq!(&owner.admit_execution(&accepted).unwrap(), delivery);
    assert_eq!(delivery.ticket(), ticket);
    assert!(
        wrix_state::execution::terminal(host, context.key(), ticket)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        owner.collect(),
        Err(wrix_service::nix_store::owner::backend::Error::Busy)
    ));
    assert!(matches!(
        owner.shutdown(),
        Err(wrix_service::nix_store::owner::backend::Error::Busy)
    ));
    let nix = std::env::var_os("WRIX_TEST_POOL_NIX").unwrap();
    let options = format!(
        "-F /dev/null -p {} -i {} -o BatchMode=yes -o IdentitiesOnly=yes -o IdentityAgent=none -o StrictHostKeyChecking=yes -o UserKnownHostsFile={}",
        delivery.port(),
        delivery.key().display(),
        delivery.known_hosts().display()
    );
    let mut command = std::process::Command::new(&nix);
    command
        .env("NIX_REMOTE", "ssh-ng://nix-client@127.0.0.1")
        .env("NIX_SSHOPTS", &options);
    let output = command.args(["store", "info", "--json"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let info: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(info["trusted"], false);
    let bash = std::env::var("WRIX_TEST_POOL_BASH").unwrap();
    let system = system().as_str();
    let expression = format!(
        "builtins.derivation {{ name = \"wrix-bound-client\"; system = \"{system}\"; builder = (builtins.storePath \"{bash}\") + \"/bin/bash\"; args = [ \"-c\" \"printf client-live > $out\" ]; }}"
    );
    let output = std::process::Command::new(&nix)
        .env("NIX_REMOTE", "ssh-ng://nix-client@127.0.0.1")
        .env("NIX_SSHOPTS", &options)
        .args([
            "build",
            "--no-link",
            "--print-out-paths",
            "--impure",
            "--option",
            "substituters",
            "",
            "--expr",
            &expression,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let path = wrix_service::nix_store::native::image::StorePath::try_from(
        String::from_utf8(output.stdout).unwrap().trim().to_owned(),
    )
    .unwrap();
    assert_eq!(
        fs::read(delivery.store().join(path.basename())).unwrap(),
        b"client-live"
    );
    assert!(fs::write(delivery.store().join("forbidden"), b"no").is_err());
    let peer = wrix_state::execution::reserve(host, context).unwrap();
    wrix_state::execution::cancel(host, context, &peer).unwrap();
    let proof = wrix_state::execution::terminal(host, context.key(), &peer)
        .unwrap()
        .unwrap();
    assert!(owner.release_execution(&proof).is_err());
    assert_eq!(
        fs::read(delivery.store().join(path.basename())).unwrap(),
        b"client-live"
    );
    wrix_state::execution::cancel(host, context, ticket).unwrap();
    assert!(wrix_state::execution::begin(host, context, ticket).is_err());
    let proof = wrix_state::execution::terminal(host, context.key(), ticket)
        .unwrap()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match owner.release_execution(&proof) {
            Ok(()) => break,
            Err(wrix_service::nix_store::owner::backend::Error::Busy) => {
                assert!(
                    Instant::now() < deadline,
                    "native release activity: {:?}",
                    owner.activity()
                );
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => panic!("native execution release failed: {error}"),
        }
    }
    assert!(fs::read(delivery.store().join(path.basename())).is_err());
    let output = command.output().unwrap();
    assert!(
        !output.status.success(),
        "revoked native client authenticated"
    );
}
