use super::{Fixture, TestResult, network, sql};
use serde_json::Value;
use std::{fs, path::Path, process::Stdio};

fn acknowledge(fixture: &Fixture) -> TestResult {
    let reply = fixture.run(
        &fixture.first,
        &["workspace", "acknowledge-scope", "--version", "1"],
    )?;
    assert_eq!(reply["success"], true, "{reply}");
    Ok(())
}

fn parallel_starts(fixture: &Fixture) -> TestResult<[Value; 2]> {
    let arguments = ["service", "start", "--no-cache"];
    let paths = [
        fixture.root.path().join("first-result.json"),
        fixture.root.path().join("second-result.json"),
    ];
    let mut children = Vec::new();
    for (workspace, result) in [&fixture.first, &fixture.second].into_iter().zip(&paths) {
        children.push(
            fixture
                .command(workspace, &arguments)?
                .env("WRIX_SHARED_RESULT", result)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
        );
    }
    for child in children {
        let output = child.wait_with_output()?;
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok([
        serde_json::from_slice(&fs::read(&paths[0])?)?,
        serde_json::from_slice(&fs::read(&paths[1])?)?,
    ])
}

fn endpoints(fixture: &Fixture, workspace: &Path) -> TestResult<Value> {
    let reply = fixture.run(workspace, &["service", "endpoints", "--no-cache"])?;
    assert_eq!(reply["success"], true, "{reply}");
    Ok(serde_json::from_str(
        reply["stdout"].as_str().ok_or("missing endpoint output")?,
    )?)
}

fn runtime_calls(fixture: &Fixture) -> TestResult<Vec<Vec<String>>> {
    fs::read_to_string(fixture.root.path().join("runtime.log"))?
        .lines()
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

fn assert_single_creation(fixture: &Fixture) -> TestResult {
    let calls = runtime_calls(fixture)?;
    let runs: Vec<_> = calls.iter().filter(|call| call[0] == "run").collect();
    assert_eq!(runs.len(), 2, "one gateway and one backend: {runs:?}");
    for name in ["wrix-gateway-1", "wrix-host-service"] {
        assert_eq!(
            runs.iter()
                .filter(|call| call.windows(2).any(|pair| pair == ["--name", name]))
                .count(),
            1,
            "{runs:?}"
        );
    }
    assert!(
        !calls
            .iter()
            .any(|call| matches!(call[0].as_str(), "stop" | "rm"))
    );
    Ok(())
}

fn authenticated_client(fixture: &Fixture, workspace: &Path) -> TestResult<Value> {
    let reply = fixture.run(workspace, &["service", "dolt", "client"])?;
    assert_eq!(reply["success"], true, "{reply}");
    let client: Value = serde_json::from_str(reply["stdout"].as_str().ok_or("missing client")?)?;
    let output = sql(&client, "SELECT * FROM marker")?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(client)
}

#[test]
fn concurrent_starts_preserve_one_deployment_selection() -> TestResult {
    if network::isolate()? {
        return Ok(());
    }
    let fixture = Fixture::new()?;
    acknowledge(&fixture)?;
    for reply in parallel_starts(&fixture)? {
        assert_eq!(reply["success"], true, "{reply}");
    }
    assert_single_creation(&fixture)?;
    let first = authenticated_client(&fixture, &fixture.first)?;
    let second = authenticated_client(&fixture, &fixture.second)?;
    assert_ne!(
        first["credentials"]["database"],
        second["credentials"]["database"]
    );
    assert_ne!(first["lock"], second["lock"]);
    for client in [&first, &second] {
        assert!(
            sql(client, "INSERT INTO marker VALUES(41)")?
                .status
                .success()
        );
    }
    let selection = fixture
        .root
        .path()
        .join("state/wrix/control/authority/native-plan.json");
    let before = fs::read(&selection)?;
    for reply in parallel_starts(&fixture)? {
        assert_eq!(reply["success"], true, "{reply}");
    }
    assert_eq!(fs::read(selection)?, before);
    assert_single_creation(&fixture)?;
    assert_eq!(
        authenticated_client(&fixture, &fixture.first)?["credentials"],
        first["credentials"]
    );
    assert_eq!(
        authenticated_client(&fixture, &fixture.second)?["credentials"],
        second["credentials"]
    );
    Ok(())
}

#[test]
fn new_workspace_registers_without_container_replacement() -> TestResult {
    if network::isolate()? {
        return Ok(());
    }
    let fixture = Fixture::new()?;
    acknowledge(&fixture)?;
    let first = fixture.start(&fixture.first)?;
    let first_endpoints = endpoints(&fixture, &fixture.first)?;
    let retained = [
        "runtime.json",
        "run.argv",
        "state/wrix/control/authority/role-endpoints.json",
    ]
    .map(|name| fixture.root.path().join(name));
    let before = retained
        .iter()
        .map(fs::read)
        .collect::<Result<Vec<_>, _>>()?;
    let second = fixture.start(&fixture.second)?;
    assert_single_creation(&fixture)?;
    for (path, content) in retained.iter().zip(before) {
        assert_eq!(fs::read(path)?, content, "{}", path.display());
    }
    let second_endpoints = endpoints(&fixture, &fixture.second)?;
    assert_eq!(endpoints(&fixture, &fixture.first)?, first_endpoints);
    assert_eq!(
        first_endpoints["container_name"],
        second_endpoints["container_name"]
    );
    assert_ne!(
        first_endpoints["workspace_hash"],
        second_endpoints["workspace_hash"]
    );
    assert_ne!(
        first_endpoints["state_root"],
        second_endpoints["state_root"]
    );
    assert_eq!(first_endpoints["endpoints"], second_endpoints["endpoints"]);
    assert_ne!(first["lock"], second["lock"]);
    assert_ne!(first["git_common"], second["git_common"]);
    assert_ne!(first["worktree"], second["worktree"]);
    assert!(sql(&first, "SELECT * FROM marker")?.status.success());
    assert!(sql(&second, "SELECT * FROM marker")?.status.success());
    let peer = second["credentials"]["database"]
        .as_str()
        .ok_or("missing peer database")?;
    assert!(
        !sql(&first, &format!("SELECT * FROM `{peer}`.marker"))?
            .status
            .success()
    );
    Ok(())
}

#[test]
fn malformed_and_concurrent_registrations_preserve_peer_namespace() -> TestResult {
    if network::isolate()? {
        return Ok(());
    }
    let fixture = Fixture::new()?;
    acknowledge(&fixture)?;
    let first = fixture.start(&fixture.first)?;
    let before = endpoints(&fixture, &fixture.first)?;
    let config = fixture.second.join(".beads/config.yaml");
    let original = fs::read(&config)?;
    let malformed = b"sync-branch: invalid..branch\n";
    fs::write(&config, malformed)?;
    let [valid, invalid] = parallel_starts(&fixture)?;
    assert_eq!(valid["success"], true, "{valid}");
    assert_eq!(invalid["success"], false, "{invalid}");
    assert_eq!(fs::read(&config)?, malformed);
    assert_eq!(endpoints(&fixture, &fixture.first)?, before);
    assert_eq!(
        authenticated_client(&fixture, &fixture.first)?["credentials"],
        first["credentials"]
    );
    assert!(
        sql(&first, "INSERT INTO marker VALUES(42)")?
            .status
            .success()
    );
    fs::write(config, original)?;
    for reply in parallel_starts(&fixture)? {
        assert_eq!(reply["success"], true, "{reply}");
    }
    assert_single_creation(&fixture)?;
    assert_eq!(
        authenticated_client(&fixture, &fixture.first)?["credentials"],
        first["credentials"]
    );
    authenticated_client(&fixture, &fixture.second)?;
    Ok(())
}
