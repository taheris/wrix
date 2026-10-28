use super::*;
use std::{
    os::unix::{fs::PermissionsExt, process::CommandExt},
    time::{Duration, Instant},
};

type TestResult = Result<(), Box<dyn std::error::Error>>;
const CHILD: &str = "WRIX_NATIVE_ADMIN_CHILD";

struct Process(std::process::Child, PathBuf);
impl Process {
    fn finish(&mut self) -> TestResult {
        fs::write(self.1.join("stop"), b"stop\n")?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = self.0.try_wait()? {
                if !status.success() {
                    return Err(format!("native SQL fixture exited {status}").into());
                }
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("native SQL fixture did not release cleanly".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        match self.0.try_wait() {
            Ok(Some(_)) => (),
            Ok(None) => {
                let result = Command::new("kill")
                    .args(["-TERM", "--", &format!("-{}", self.0.id())])
                    .status();
                match result {
                    Ok(status) if status.success() => (),
                    failure => {
                        tracing::error!(?failure, "owned native SQL process-group cleanup failed");
                    }
                }
                if let Err(error) = self.0.wait() {
                    tracing::error!(%error, "owned native SQL fixture could not be reaped");
                }
            }
            Err(error) => tracing::error!(%error, "owned native SQL fixture state is unavailable"),
        }
    }
}

fn host_socket_identity() -> io::Result<Option<(u64, u64)>> {
    match fs::symlink_metadata("/run/wrix/dolt/native.sock") {
        Ok(metadata) => Ok(Some((metadata.dev(), metadata.ino()))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[test]
#[ignore = "requires native Linux user/mount namespaces, util-linux nsenter and real Dolt; no runtime substitution"]
fn native_management_works_while_the_real_tenant_sql_gate_is_closed() -> TestResult {
    if let Some(data) = std::env::var_os(CHILD) {
        return tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(serve(Path::new(&data)));
    }
    let host_socket = host_socket_identity()?;
    let mut root = tempfile::Builder::new()
        .prefix("wrix-admin-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in("/tmp")?;
    root.disable_cleanup(true);
    eprintln!(
        "native SQL fixture retained on failure: {}",
        root.path().display()
    );
    let data = root.path().join("data");
    fs::create_dir(&data)?;
    fs::set_permissions(&data, fs::Permissions::from_mode(0o700))?;
    let home = root.path().join("home");
    fs::create_dir(&home)?;
    let log = File::create(root.path().join("child.log"))?;
    let mut command = Command::new("unshare");
    command.args(["--user", "--map-root-user", "--mount", "--propagation", "private", "--", "bash", "-c", "set -euo pipefail; mount -t tmpfs -o mode=0755 tmpfs /run; exec \"$@\"", "native-admin"])
        .arg(std::env::current_exe()?)
        .args(["--ignored", "--exact", "dolt::native_admin_test::native_management_works_while_the_real_tenant_sql_gate_is_closed", "--nocapture"])
        .env(CHILD, &data).env("HOME", &home)
        .current_dir(root.path()).process_group(0)
        .stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    let mut process = Process(command.spawn()?, data.clone());
    let deadline = Instant::now() + Duration::from_secs(30);
    while !data.join("ready").exists() {
        if process.0.try_wait()?.is_some() || Instant::now() >= deadline {
            return Err(format!(
                "native SQL fixture unavailable: {}",
                fs::read_to_string(root.path().join("child.log"))?
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let runtime = root.path().join("runtime");
    fs::write(
        &runtime,
        format!(
            "#!/usr/bin/env bash\nset -euo pipefail\n[[ $# -ge 4 && $1 == exec && $2 == -i && $3 == selected-backend ]]\nshift 3\nexec nsenter --user --mount --preserve-credentials --target {} -- \"$@\"\n",
            process.0.id()
        ),
    )?;
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700))?;
    let executor = Executor::Container {
        runtime: runtime.clone(),
        container: "selected-backend".into(),
        data: data.clone(),
        administration: Administration::Private,
    };
    let mut query = executor.command(None, Some("controller"));
    let mut child = query.spawn()?;
    child
        .stdin
        .take()
        .ok_or("missing native query input")?
        .write_all(b"SELECT 73 AS answer;")?;
    let reply = child.wait_with_output()?;
    assert!(
        reply.status.success(),
        "{}",
        String::from_utf8_lossy(&reply.stderr)
    );
    let reply: serde_json::Value = serde_json::from_slice(&reply.stdout)?;
    assert_eq!(reply["rows"][0]["answer"], "73");
    let denied = Executor::Container {
        runtime,
        container: "selected-backend".into(),
        data: data.clone(),
        administration: Administration::Legacy,
    };
    let mut query = denied.command(None, Some("controller"));
    let mut child = query.spawn()?;
    child
        .stdin
        .take()
        .ok_or("missing negative query input")?
        .write_all(b"SELECT 73 AS answer;")?;
    assert!(
        !child.wait_with_output()?.status.success(),
        "tenant gate admitted private management while closed"
    );
    process.finish()?;
    assert_eq!(
        host_socket_identity()?,
        host_socket,
        "fixture changed the host's private socket"
    );
    assert!(data.join("databases/controller").is_dir());
    root.disable_cleanup(false);
    Ok(())
}

async fn serve(data: &Path) -> TestResult {
    let _owner = {
        let owner = File::options()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(data.join("owner.lock"))?;
        owner.try_lock_exclusive()?;
        owner
    };
    private_directory(&data.join("databases"))?;
    private_directory(&data.join("run"))?;
    private_directory(Path::new("/run/wrix/dolt"))?;
    server::initialize(data)?;
    let config = data.join("server.json");
    fs::write(
        &config,
        serde_json::to_vec(&server::configuration_value(data, true))?,
    )?;
    let log = File::create(data.join("dolt.log"))?;
    let mut native = tokio::process::Command::new("dolt")
        .args(["sql-server", "--config"])
        .arg(config)
        .current_dir(data)
        .env_remove("DOLT_ROOT_HOST")
        .env_remove("DOLT_ROOT_PASSWORD")
        .env_remove("DOLT_CLI_PASSWORD")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .kill_on_drop(true)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while !Path::new("/run/wrix/dolt/native.sock").exists() {
        if native.try_wait()?.is_some() || Instant::now() >= deadline {
            return Err(format!(
                "native Dolt did not start: {}",
                fs::read_to_string(data.join("dolt.log"))?
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let gate = crate::lifecycle::sql::Gate::default();
    let serving = crate::lifecycle::sql::serve(data, gate);
    tokio::pin!(serving);
    loop {
        tokio::select! {
            result = &mut serving => return Err(format!("native SQL gate stopped: {result:?}").into()),
            () = tokio::time::sleep(Duration::from_millis(20)) => {
                if Path::new("/run/wrix/dolt/drain.sock").exists() && !data.join("ready").exists() {
                    fs::write(data.join("ready"), b"ready\n")?;
                }
                if data.join("stop").exists() { break; }
                if native.try_wait()?.is_some() { return Err("native SQL owner exited unexpectedly".into()); }
            }
        }
    }
    let pid = native
        .id()
        .ok_or("native SQL process lost before shutdown")?;
    let status = tokio::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .await?;
    assert!(status.success());
    assert!(native.wait().await?.success());
    Ok(())
}
