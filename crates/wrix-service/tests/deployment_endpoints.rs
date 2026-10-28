use std::{fs, net::Ipv4Addr, os::unix::fs::PermissionsExt};

use wrix_service::{
    control::{Access, Client, Controller, Error, Namespace, Operation, Policy, Reply},
    route::{Listeners, Role},
};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

static ROLE_PORTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn addresses(
    listeners: &Listeners,
) -> Result<Vec<std::net::SocketAddr>, wrix_service::route::Error> {
    Role::ALL
        .into_iter()
        .map(|role| listeners.address(role))
        .collect()
}

#[tokio::test]
async fn controller_role_leases_are_bound_persisted_and_reused() -> TestResult {
    let _ports = ROLE_PORTS.lock().await;
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?.join("controller");
    let controller = Controller::bind(&root)?;
    let listeners = controller.bind_role_listeners().await?;
    let selected = addresses(&listeners)?;
    assert!((23_000..25_000).contains(&selected[0].port()));
    assert!((21_000..23_000).contains(&selected[1].port()));
    for address in &selected {
        assert_eq!(address.ip(), Ipv4Addr::LOCALHOST);
        assert!(tokio::net::TcpStream::connect(address).await.is_ok());
        assert!(tokio::net::TcpListener::bind(address).await.is_err());
    }
    let before = fs::read(root.join("role-endpoints.json"))?;
    drop(listeners);
    drop(controller);
    fs::remove_file(root.join("control.sock"))?;
    let controller = Controller::bind(&root)?;
    let listeners = controller.bind_role_listeners().await?;
    assert_eq!(addresses(&listeners)?, selected);
    assert_eq!(fs::read(root.join("role-endpoints.json"))?, before);
    Ok(())
}

#[tokio::test]
async fn legacy_four_role_leases_preserve_their_ports_when_adding_the_dolt_sync_plane() -> TestResult
{
    let _ports = ROLE_PORTS.lock().await;
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?.join("controller");
    let controller = Controller::bind(&root)?;
    let listeners = controller.bind_role_listeners().await?;
    let original = addresses(&listeners)?;
    drop(listeners);
    drop(controller);
    fs::remove_file(root.join("control.sock"))?;
    fs::write(
        root.join("role-endpoints.json"),
        serde_json::to_vec(&serde_json::json!({
            "version": 1, "ports": original[..4].iter().map(std::net::SocketAddr::port).collect::<Vec<_>>()
        }))?,
    )?;
    let controller = Controller::bind(&root)?;
    let listeners = controller.bind_role_listeners().await?;
    let current = addresses(&listeners)?;
    assert_eq!(&current[..4], &original[..4]);
    assert_eq!(current.len(), 5);
    assert!(tokio::net::TcpStream::connect(current[4]).await.is_ok());
    let snapshot: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("role-endpoints.json"))?)?;
    assert_eq!(snapshot["version"], 2);
    assert_eq!(
        snapshot["ports"].as_array().ok_or("missing ports")?.len(),
        5
    );
    Ok(())
}

#[tokio::test]
async fn busy_saved_role_port_falls_back_to_a_bound_persisted_loopback_listener() -> TestResult {
    let _ports = ROLE_PORTS.lock().await;
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?.join("controller");
    let controller = Controller::bind(&root)?;
    let listeners = controller.bind_role_listeners().await?;
    let original = addresses(&listeners)?;
    let snapshot = fs::read(root.join("role-endpoints.json"))?;
    drop(controller);
    fs::remove_file(root.join("control.sock"))?;
    let controller = Controller::bind(&root)?;
    let replacement = controller.bind_role_listeners().await?;
    for (old, new) in original.iter().zip(addresses(&replacement)?) {
        assert_ne!(*old, new);
        assert_eq!(new.ip(), Ipv4Addr::LOCALHOST);
        assert!(tokio::net::TcpStream::connect(new).await.is_ok());
    }
    assert_ne!(fs::read(root.join("role-endpoints.json"))?, snapshot);
    drop(listeners);
    Ok(())
}

#[tokio::test]
async fn concurrent_listener_requests_cannot_create_competing_deployment_leases() -> TestResult {
    let _ports = ROLE_PORTS.lock().await;
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?.join("controller");
    let controller = Controller::bind(&root)?;
    let (first, second) = tokio::join!(
        controller.bind_role_listeners(),
        controller.bind_role_listeners()
    );
    assert!(first.is_ok());
    assert!(matches!(second, Err(Error::EndpointsAlreadyBound)));
    Ok(())
}

#[tokio::test]
async fn namespace_registration_does_not_allocate_or_rewrite_controller_role_leases() -> TestResult
{
    let _ports = ROLE_PORTS.lock().await;
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?.join("controller");
    let controller = Controller::bind(&root)?;
    let listeners = controller.bind_role_listeners().await?;
    let selected = addresses(&listeners)?;
    let before = fs::read(root.join("role-endpoints.json"))?;
    let policy = Policy {
        dolt: Access::Allowed,
        private_cache_read: Access::Allowed,
        common_cache_read: Access::Allowed,
        egress: Access::Denied,
        compiler: Access::Denied,
    };
    for digit in ['a', 'b'] {
        let client = Client::open(&root)?;
        let (served, reply) = tokio::join!(
            controller.serve_one(),
            client.request(Operation::Register {
                namespace: Namespace::try_from(digit.to_string().repeat(64))?,
                policy,
            })
        );
        served?;
        assert_eq!(reply?, Reply::Applied);
        assert_eq!(addresses(&listeners)?, selected);
        assert_eq!(fs::read(root.join("role-endpoints.json"))?, before);
    }
    Ok(())
}

#[tokio::test]
async fn malformed_role_leases_fail_before_credentials_or_control_publication() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?.join("controller");
    fs::create_dir(&root)?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    for value in [
        serde_json::json!({"version": 2, "ports": [23000, 21000, 30000, 30001]}),
        serde_json::json!({"version": 1, "ports": [0, 21000, 30000, 30001]}),
        serde_json::json!({"version": 1, "ports": [23000, 25000, 30000, 30001]}),
        serde_json::json!({"version": 1, "ports": [23000, 21000, 30000, 30000]}),
        serde_json::json!({"version": 1, "ports": [23000, 21000, 30000, 30001], "host": "0.0.0.0"}),
    ] {
        let bytes = serde_json::to_vec(&value)?;
        fs::write(root.join("role-endpoints.json"), &bytes)?;
        fs::set_permissions(
            root.join("role-endpoints.json"),
            fs::Permissions::from_mode(0o600),
        )?;
        assert!(Controller::bind(&root).is_err());
        assert!(!root.join("management.key").exists());
        assert!(!root.join("control.sock").exists());
        assert_eq!(fs::read(root.join("role-endpoints.json"))?, bytes);
    }
    Ok(())
}

#[tokio::test]
async fn invalid_endpoint_path_prevents_binding_and_allows_repaired_retry() -> TestResult {
    let _ports = ROLE_PORTS.lock().await;
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?.join("controller");
    let controller = Controller::bind(&root)?;
    let path = root.join("role-endpoints.json");
    fs::create_dir(&path)?;
    assert!(matches!(
        controller.bind_role_listeners().await,
        Err(Error::PrivateRoot)
    ));
    fs::remove_dir(&path)?;
    assert!(controller.bind_role_listeners().await.is_ok());
    Ok(())
}
