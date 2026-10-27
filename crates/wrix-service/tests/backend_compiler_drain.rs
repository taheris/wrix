#[path = "../src/lifecycle/drain/mod.rs"]
pub mod drain;
#[cfg(test)]
mod test {
    use super::drain;
    use drain::Native;
    use std::{
        fs,
        io::Read,
        os::unix::net::UnixStream,
        path::Path,
        process::{Child, Command, Stdio},
        time::{Duration, Instant},
    };

    struct Process(Child);
    impl Drop for Process {
        fn drop(&mut self) {
            if self.0.try_wait().unwrap().is_none() {
                self.0.kill().unwrap();
                self.0.wait().unwrap();
            }
        }
    }
    struct Socket<'a>(&'a Path);
    impl Native for Socket<'_> {
        fn command(&mut self, command: drain::Command) -> Result<drain::Response, drain::Failure> {
            let mut stream = UnixStream::connect(self.0)?;
            stream.set_read_timeout(Some(Duration::from_secs(10)))?;
            drain::exchange(&mut stream, command)
        }
    }
    fn start(binary: &Path, root: &Path) -> (Process, std::path::PathBuf, std::path::PathBuf) {
        let protocol = root.join("compiler.sock");
        let management = root.join("drain.sock");
        let mut server = Process(
            Command::new(binary)
                .env("SCCACHE_START_SERVER", "1")
                .env("SCCACHE_NO_DAEMON", "1")
                .env("SCCACHE_SERVER_UDS", &protocol)
                .env("WRIX_SCCACHE_DRAIN_SOCKET", &management)
                .env("SCCACHE_DIR", root.join("cache"))
                .env("SCCACHE_CACHE_SIZE", "1M")
                .env("WRIX_COMPILER_VIEW_ROOT", root)
                .env_remove("SCCACHE_CONF")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let started = Instant::now();
        while !management.exists() {
            assert!(
                server.0.try_wait().unwrap().is_none(),
                "native owner died before readiness"
            );
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "native management did not bind"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        (server, protocol, management)
    }

    fn compile(
        binary: &Path,
        socket: &Path,
        directory: &Path,
        source: &str,
    ) -> std::process::Output {
        fs::create_dir_all(directory).unwrap();
        fs::write(directory.join("input.rs"), source).unwrap();
        Command::new(binary)
            .args([
                "rustc",
                "--crate-type=lib",
                "--crate-name=origin",
                "input.rs",
                "--emit=link",
                "--out-dir",
                ".",
            ])
            .current_dir(directory)
            .env("SCCACHE_SERVER_UDS", socket)
            .env("WRIX_SCCACHE_MANAGED", "1")
            .env("WRIX_SCCACHE_REQUIRE_NATIVE", "1")
            .env(
                "WRIX_COMPILER_EXECUTION",
                directory.parent().unwrap().file_name().unwrap(),
            )
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    #[test]
    #[ignore = "requires WRIX_TEST_SCCACHE pointing to the built patched native daemon and rustc"]
    fn native_compiler_drains_output_receipts_and_ignores_idle_transports() {
        assert_eq!(
            drain::Role::Compiler.socket(),
            "/run/wrix/compiler/drain.sock"
        );
        let _fixed = drain::Client::new(drain::Role::Celler, Duration::from_secs(1));
        let binary = std::env::var_os("WRIX_TEST_SCCACHE").expect("patched native binary required");
        let binary = Path::new(&binary);
        let directory = tempfile::tempdir().unwrap();
        let (mut server, protocol, management) = start(binary, directory.path());
        let inspect = Command::new(binary)
            .args(["--wrix-work-drain", "status"])
            .env("WRIX_SCCACHE_DRAIN_SOCKET", &management)
            .output()
            .unwrap();
        assert!(
            inspect.status.success(),
            "{}",
            String::from_utf8_lossy(&inspect.stderr)
        );
        let observed: drain::Response = serde_json::from_slice(&inspect.stdout).unwrap();
        assert_eq!(observed.status.phase, drain::Phase::Serving);
        let a = directory.path().join("execution-a/workspace");
        let b = directory.path().join("execution-b/workspace");
        fs::create_dir_all(&a).unwrap();
        let not_compilation = Command::new(binary)
            .args(["rustc", "-vV"])
            .current_dir(&a)
            .env("SCCACHE_SERVER_UDS", &protocol)
            .env("WRIX_SCCACHE_MANAGED", "1")
            .env("WRIX_COMPILER_EXECUTION", "execution-a")
            .env("WRIX_SCCACHE_REQUIRE_NATIVE", "1")
            .output()
            .unwrap();
        assert!(!not_compilation.status.success());
        assert!(
            String::from_utf8_lossy(&not_compilation.stderr)
                .contains("no managed execution/output receipt")
        );
        let missing_view = Command::new(binary)
            .args([
                "rustc",
                "--crate-type=lib",
                "--crate-name=invalid_view",
                "--emit=link",
                "--out-dir",
                ".",
                "missing.rs",
            ])
            .current_dir(directory.path())
            .env("SCCACHE_SERVER_UDS", &protocol)
            .env("WRIX_SCCACHE_MANAGED", "1")
            .env("WRIX_COMPILER_EXECUTION", "execution-a")
            .output()
            .unwrap();
        assert!(!missing_view.status.success());
        assert!(
            String::from_utf8_lossy(&missing_view.stderr)
                .contains("managed originating compiler view unavailable")
        );
        let first = compile(binary, &protocol, &a, "pub fn answer() -> u32 { 41 }");
        assert!(
            first.status.success(),
            "{}",
            String::from_utf8_lossy(&first.stderr)
        );
        let second = compile(binary, &protocol, &b, "pub fn answer() -> u32 { 42 }");
        assert!(
            second.status.success(),
            "{}",
            String::from_utf8_lossy(&second.stderr)
        );
        assert_ne!(
            fs::read(a.join("liborigin.rlib")).unwrap(),
            fs::read(b.join("liborigin.rlib")).unwrap()
        );
        fs::write(a.join("gate.rs"), r#"extern crate proc_macro; #[proc_macro] pub fn gate(input: proc_macro::TokenStream) -> proc_macro::TokenStream { std::fs::write("entered", "1").unwrap(); while std::path::Path::new("hold").exists() { std::thread::sleep(std::time::Duration::from_millis(10)); } input }"#).unwrap();
        let macro_build = Command::new("rustc")
            .args([
                "--crate-type=proc-macro",
                "--crate-name=gate",
                "gate.rs",
                "-o",
                "libgate.so",
            ])
            .current_dir(&a)
            .output()
            .unwrap();
        assert!(
            macro_build.status.success(),
            "{}",
            String::from_utf8_lossy(&macro_build.stderr)
        );
        fs::write(a.join("hold"), "1").unwrap();
        fs::write(
            a.join("input.rs"),
            "extern crate gate; gate::gate!(pub fn delayed() -> u32 { 99 });",
        )
        .unwrap();
        let mut delayed = Process(
            Command::new(binary)
                .args([
                    "rustc",
                    "--crate-type=lib",
                    "--crate-name=delayed",
                    "input.rs",
                    "--extern",
                    "gate=libgate.so",
                    "--emit=link",
                    "--out-dir",
                    ".",
                ])
                .current_dir(&a)
                .env("SCCACHE_SERVER_UDS", &protocol)
                .env("WRIX_SCCACHE_MANAGED", "1")
                .env("WRIX_COMPILER_EXECUTION", "execution-a")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let started = Instant::now();
        while !a.join("entered").exists() {
            assert!(delayed.0.try_wait().unwrap().is_none());
            assert!(started.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut busy = Socket(&management);
        let before = busy.command(drain::Command::Status).unwrap();
        assert!(
            before.status.requests > 0,
            "native compile was not admitted: {before:?}"
        );
        let outcome = drain::quiesce(&mut busy, Duration::ZERO);
        assert!(matches!(outcome, Err(drain::Failure::Busy)), "{outcome:?}");
        assert_eq!(
            busy.command(drain::Command::Status).unwrap().status.phase,
            drain::Phase::Serving
        );
        assert!(server.0.try_wait().unwrap().is_none());
        assert!(delayed.0.try_wait().unwrap().is_none());
        fs::remove_file(a.join("hold")).unwrap();
        assert!(delayed.0.wait().unwrap().success());
        assert!(a.join("libdelayed.rlib").is_file());
        let failed = compile(binary, &protocol, &b, "not valid Rust");
        assert!(!failed.status.success());
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "ordinary job failure stopped the daemon"
        );
        let statistics = Command::new(binary)
            .args(["--show-stats", "--stats-format=json"])
            .env("SCCACHE_SERVER_UDS", &protocol)
            .env("WRIX_SCCACHE_MANAGED", "1")
            .output()
            .unwrap();
        assert!(statistics.status.success());
        let statistics: serde_json::Value = serde_json::from_slice(&statistics.stdout).unwrap();
        assert!(statistics["stats"]["requests_executed"].as_u64().unwrap() >= 4);
        assert_eq!(statistics["max_cache_size"], 50_u64 * 1024 * 1024 * 1024);
        let mut idle = UnixStream::connect(&protocol).unwrap();
        idle.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut native = Socket(&management);
        assert!(
            drain::quiesce(&mut native, Duration::from_secs(5))
                .unwrap()
                .drained()
        );
        assert_eq!(
            drain::release(&mut native).unwrap().phase,
            drain::Phase::Released
        );
        assert_eq!(idle.read(&mut [0u8; 1]).unwrap(), 0);
        assert!(server.0.wait().unwrap().success());
        let expected = fs::read(a.join("liborigin.rlib")).unwrap();
        fs::remove_file(a.join("liborigin.rlib")).unwrap();
        let (mut replacement, protocol, management) = start(binary, directory.path());
        let reused = compile(binary, &protocol, &a, "pub fn answer() -> u32 { 41 }");
        assert!(
            reused.status.success(),
            "{}",
            String::from_utf8_lossy(&reused.stderr)
        );
        assert_eq!(fs::read(a.join("liborigin.rlib")).unwrap(), expected);
        let statistics = Command::new(binary)
            .args(["--show-stats", "--stats-format=json"])
            .env("SCCACHE_SERVER_UDS", &protocol)
            .env("WRIX_SCCACHE_MANAGED", "1")
            .output()
            .unwrap();
        assert!(statistics.status.success());
        let statistics: serde_json::Value = serde_json::from_slice(&statistics.stdout).unwrap();
        let hits: u64 = statistics["stats"]["cache_hits"]["counts"]
            .as_object()
            .unwrap()
            .values()
            .map(|count| count.as_u64().unwrap())
            .sum();
        assert!(
            hits >= 1,
            "compatible native cache entry was not reused: {statistics}"
        );
        let mut native = Socket(&management);
        drain::quiesce(&mut native, Duration::from_secs(5)).unwrap();
        drain::release(&mut native).unwrap();
        assert!(replacement.0.wait().unwrap().success());
    }

    #[test]
    #[ignore = "requires WRIX_TEST_SCCACHE and WRIX_TEST_LEGACY_SCCACHE pointing to native binaries"]
    fn unmanaged_client_retains_the_stock_private_cache_wire_protocol() {
        let patched =
            std::env::var_os("WRIX_TEST_SCCACHE").expect("patched native client required");
        let legacy =
            std::env::var_os("WRIX_TEST_LEGACY_SCCACHE").expect("stock private daemon required");
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("private.sock");
        let mut server = Process(
            Command::new(&legacy)
                .env("SCCACHE_START_SERVER", "1")
                .env("SCCACHE_NO_DAEMON", "1")
                .env("SCCACHE_SERVER_UDS", &socket)
                .env("SCCACHE_DIR", root.path().join("private-cache"))
                .env_remove("SCCACHE_CONF")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let started = Instant::now();
        while !socket.exists() {
            assert!(server.0.try_wait().unwrap().is_none());
            assert!(started.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(10));
        }
        fs::write(
            root.path().join("input.rs"),
            "pub fn answer() -> u32 { 123 }",
        )
        .unwrap();
        let output = Command::new(patched)
            .args([
                "rustc",
                "--crate-type=lib",
                "--crate-name=private",
                "input.rs",
                "--emit=link",
                "--out-dir",
                ".",
            ])
            .current_dir(root.path())
            .env("SCCACHE_SERVER_UDS", &socket)
            .env_remove("WRIX_SCCACHE_MANAGED")
            .env_remove("WRIX_SCCACHE_REQUIRE_NATIVE")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("compiling locally"));
        assert!(root.path().join("libprivate.rlib").exists());
        assert!(
            Command::new(legacy)
                .arg("--stop-server")
                .env("SCCACHE_SERVER_UDS", &socket)
                .output()
                .unwrap()
                .status
                .success()
        );
        assert!(server.0.wait().unwrap().success());
    }

    #[test]
    #[ignore = "requires WRIX_TEST_SCCACHE and WRIX_TEST_LEGACY_SCCACHE pointing to native binaries"]
    fn legacy_client_is_rejected_before_admitting_a_job_or_mutating_originating_outputs() {
        let binary = std::env::var_os("WRIX_TEST_SCCACHE").expect("patched native binary required");
        let legacy =
            std::env::var_os("WRIX_TEST_LEGACY_SCCACHE").expect("unpatched native binary required");
        let directory = tempfile::tempdir().unwrap();
        let (mut server, protocol, management) = start(Path::new(&binary), directory.path());
        let origin = directory.path().join("execution-a/workspace");
        fs::create_dir_all(&origin).unwrap();
        fs::write(origin.join("input.rs"), "pub fn answer() -> u32 { 123 }").unwrap();
        let output = Command::new(legacy)
            .args([
                "rustc",
                "--crate-type=lib",
                "--crate-name=legacy",
                "input.rs",
                "--emit=link",
                "--out-dir",
                ".",
            ])
            .current_dir(&origin)
            .env("SCCACHE_SERVER_UDS", &protocol)
            .env("WRIX_COMPILER_EXECUTION", "execution-a")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!origin.join("liblegacy.rlib").exists());
        let mut native = Socket(&management);
        let status = native.command(drain::Command::Status).unwrap().status;
        assert_eq!(status.phase, drain::Phase::Serving);
        assert!(!status.failed);
        drain::quiesce(&mut native, Duration::from_secs(3)).unwrap();
        drain::release(&mut native).unwrap();
        assert!(server.0.wait().unwrap().success());
    }

    #[test]
    #[ignore = "requires WRIX_TEST_SCCACHE pointing to the built patched native daemon and rustc"]
    fn lost_originating_output_aborts_before_native_shutdown_instead_of_claiming_completion() {
        let binary = std::env::var_os("WRIX_TEST_SCCACHE").expect("patched native binary required");
        let binary = Path::new(&binary);
        let directory = tempfile::tempdir().unwrap();
        let (mut server, protocol, management) = start(binary, directory.path());
        let origin = directory.path().join("execution-a/workspace");
        fs::create_dir_all(&origin).unwrap();
        fs::write(origin.join("broken.rs"), "this is not Rust").unwrap();
        let mut client = Command::new(binary)
            .args([
                "rustc",
                "--crate-type=lib",
                "--crate-name=broken",
                "broken.rs",
                "--emit=link",
                "--out-dir",
                ".",
            ])
            .current_dir(&origin)
            .env("SCCACHE_SERVER_UDS", &protocol)
            .env("WRIX_SCCACHE_MANAGED", "1")
            .env("WRIX_COMPILER_EXECUTION", "execution-a")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        drop(client.stderr.take());
        assert!(!client.wait().unwrap().success());
        let mut native = Socket(&management);
        let started = Instant::now();
        while !native
            .command(drain::Command::Status)
            .unwrap()
            .status
            .failed
        {
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(matches!(
            drain::quiesce(&mut native, Duration::from_secs(1)),
            Err(drain::Failure::Uncertain)
        ));
        assert_eq!(
            native.command(drain::Command::Status).unwrap().status.phase,
            drain::Phase::Serving
        );
        assert!(drain::release(&mut native).is_err());
        assert!(server.0.try_wait().unwrap().is_none());
    }
}
