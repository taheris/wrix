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
    let (ticket, delivery) = prepare_on(runtime, client, selection, host, request::Plane::Podman);
    assert!(delivery.podman_configuration().is_ok());
    assert!(
        fs::read_dir(delivery.store()).unwrap().next().is_none(),
        "Podman source propagated into the host namespace"
    );
    wrix_state::execution::cancel(host, selection.context().unwrap(), &ticket).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let record: serde_json::Value = serde_json::from_slice(
            &fs::read(delivery.key().parent().unwrap().join("client.json")).unwrap(),
        )
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
    assert!(delivery.podman_configuration().is_err());
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
