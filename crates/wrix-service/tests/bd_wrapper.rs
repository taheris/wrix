//! Execute the production Nix-rendered wrapper with external host/native clients mocked.
#![cfg(target_os = "linux")]
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
struct Fixture {
    root: tempfile::TempDir,
    wrapper: PathBuf,
    first: PathBuf,
    second: PathBuf,
}
impl Fixture {
    fn new() -> TestResult<Self> {
        let root = tempfile::tempdir()?;
        fs::create_dir(root.path().join("host-context"))?;
        fs::create_dir(root.path().join("guest-context"))?;
        fs::write(root.path().join("guest-context/image-agent"), "fixture")?;
        let native = root.path().join("native");
        fs::create_dir_all(native.join("bin"))?;
        fs::write(
            native.join("bin/bd"),
            "#!/usr/bin/env bash\nset -euo pipefail\nif [[ -z \"${BEADS_DOLT_SERVER_DATABASE+x}\" ]]; then BEADS_DOLT_SERVER_DATABASE=unmanaged; fi\nprintf 'native:%s' \"$BEADS_DOLT_SERVER_DATABASE\"\n",
        )?;
        fs::set_permissions(native.join("bin/bd"), fs::Permissions::from_mode(0o700))?;
        let host = root.path().join("wrix");
        fs::write(
            &host,
            r#"#!/usr/bin/env bash
set -euo pipefail
case "$3" in
  client-env) printf 'export WRIX_BEADS_MANAGED=1 BEADS_DOLT_SERVER_DATABASE=%q\n' "namespace-$(basename "$PWD")" ;;
  sync-push|sync-pull) printf 'broker:%s:%s' "$3" "$BEADS_DOLT_SERVER_DATABASE" ;;
  *) exit 64 ;;
esac
"#,
        )?;
        fs::set_permissions(host, fs::Permissions::from_mode(0o700))?;
        let package = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lib/beads/package.nix");
        let expression = format!(
            "import {} {{ pkgs = {{ writeShellScriptBin = name: script: script; beads = {}; }}; }}",
            package.display(),
            serde_json::to_string(&native)?
        );
        let output = Command::new("nix")
            .args(["eval", "--raw", "--impure", "--expr", &expression])
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let wrapper = root.path().join("bd");
        fs::write(&wrapper, output.stdout)?;
        let first = root.path().join("first");
        let second = root.path().join("second");
        for repository in [&first, &second] {
            fs::create_dir_all(repository.join(".beads/dolt"))?;
            fs::write(
                repository.join(".beads/dolt/WRIX-MANAGED"),
                "fixture managed marker",
            )?;
            let output = Command::new("git")
                .args(["init", "-q"])
                .arg(repository)
                .output()?;
            assert!(output.status.success());
        }
        Ok(Self {
            root,
            wrapper,
            first,
            second,
        })
    }
    fn command(&self, repository: &Path, arguments: &[&str], guest: bool) -> Command {
        let mut command = Command::new("unshare");
        command.args(["--user", "--map-root-user", "--mount", "--propagation", "private", "sh", "-c",
            "if ! test -d /etc/wrix; then mount -t tmpfs tmpfs /etc; mkdir /etc/wrix; fi; mount --bind \"$1\" /etc/wrix; shift; exec bash \"$@\"", "fixture"])
            .arg(self.root.path().join(if guest { "guest-context" } else { "host-context" }));
        command
            .arg(&self.wrapper)
            .args(arguments)
            .current_dir(repository)
            .env("WRIX_BIN", self.root.path().join("wrix"))
            .env("WRIX_BEADS_MANAGED", "1")
            .env("BEADS_DOLT_SERVER_DATABASE", "stale-first")
            .env("BEADS_DOLT_SERVER_USER", "stale-user")
            .env("BEADS_DOLT_PASSWORD", "stale-password")
            .env_remove("IS_SANDBOX");
        command
    }
    fn run(&self, repository: &Path, arguments: &[&str]) -> TestResult<Output> {
        Ok(self.command(repository, arguments, false).output()?)
    }
}

#[test]
fn managed_host_wrapper_refreshes_scope_even_with_inherited_managed_credentials() -> TestResult {
    let fixture = Fixture::new()?;
    let first = fixture.run(&fixture.first, &["sql", "SELECT DATABASE()"])?;
    assert!(first.status.success());
    assert_eq!(first.stdout, b"native:namespace-first");
    let second = fixture.run(&fixture.second, &["sql", "SELECT DATABASE()"])?;
    assert!(second.status.success());
    assert_eq!(second.stdout, b"native:namespace-second");
    Ok(())
}

#[test]
fn explicit_directory_options_reject_instead_of_using_wrong_database() -> TestResult {
    let fixture = Fixture::new()?;
    for arguments in [
        vec!["-C", fixture.first.to_str().unwrap(), "sql", "SELECT 1"],
        vec!["--directory=../first", "sql", "SELECT 1"],
    ] {
        let output = fixture.run(&fixture.second, &arguments)?;
        assert!(!output.status.success());
        assert_eq!(output.stdout, [] as [u8; 0]);
    }
    Ok(())
}

#[test]
fn issue_body_directory_text_is_data_and_unmanaged_scope_flags_are_native() -> TestResult {
    let fixture = Fixture::new()?;
    for arguments in [
        vec!["create", "--title", "-C", "--description", "--directory"],
        vec!["create", "--", "-C"],
    ] {
        assert!(fixture.run(&fixture.first, &arguments)?.status.success());
    }
    fs::remove_file(fixture.first.join(".beads/dolt/WRIX-MANAGED"))?;
    let output = fixture.run(&fixture.first, &["-C", "elsewhere", "sql", "SELECT 1"])?;
    assert!(output.status.success());
    assert_eq!(output.stdout, b"native:unmanaged");
    Ok(())
}

#[test]
fn leading_global_flags_use_fixed_sync_broker_without_admin_fallback() -> TestResult {
    let fixture = Fixture::new()?;
    let output = fixture.run(&fixture.second, &["--json", "dolt", "push"])?;
    assert!(output.status.success());
    assert_eq!(output.stdout, b"broker:sync-push:namespace-second");
    assert!(
        !fixture
            .run(&fixture.second, &["--json", "dolt", "push", "file:///peer"])?
            .status
            .success()
    );
    Ok(())
}

#[test]
fn guest_wrapper_keeps_sealed_launch_scope_instead_of_host_refresh() -> TestResult {
    let fixture = Fixture::new()?;
    let output = fixture
        .command(&fixture.second, &["sql", "SELECT 1"], true)
        .env("IS_SANDBOX", "1")
        .output()?;
    assert!(output.status.success());
    assert_eq!(output.stdout, b"native:stale-first");
    Ok(())
}
