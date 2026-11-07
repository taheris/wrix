//! Linux packaging-boundary conformance against the actual verifier and runners.
#![cfg(test)]

use std::{
    collections::HashMap,
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

const CAPABILITIES: [(&str, u32); 9] = [
    ("CAP_CHOWN", 0),
    ("CAP_DAC_OVERRIDE", 1),
    ("CAP_DAC_READ_SEARCH", 2),
    ("CAP_FOWNER", 3),
    ("CAP_FSETID", 4),
    ("CAP_SETGID", 6),
    ("CAP_SETUID", 7),
    ("CAP_MKNOD", 27),
    ("CAP_SETFCAP", 31),
];

fn required_bits() -> u64 {
    CAPABILITIES.iter().map(|(_, bit)| 1u64 << bit).sum()
}

fn requirements(virtiofs: bool) -> Value {
    json!({
        "platforms": ["aarch64-linux", "x86_64-linux"],
        "capabilities": if virtiofs { vec!["virtiofsd-capabilities"] } else { vec![] },
    })
}

fn output(command: &mut Command) -> Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("VM prerequisite fixture exceeded its execution deadline");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    child.wait_with_output().unwrap()
}

struct Fixture {
    root: tempfile::TempDir,
    environment: HashMap<OsString, OsString>,
    library: PathBuf,
    bash: PathBuf,
    lifecycle: PathBuf,
    cache: PathBuf,
    system: String,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("wrix-vm-prerequisite-")
            .tempdir_in("/tmp")
            .unwrap();
        let mut environment: HashMap<_, _> = env::vars_os().collect();
        let local = output(Command::new("git").args(["rev-parse", "--local-env-vars"]));
        assert!(local.status.success(), "{local:?}");
        for name in String::from_utf8(local.stdout).unwrap().lines() {
            environment.remove(&OsString::from(name));
        }
        for (name, value) in [
            ("WRIX_TEST_VM_ROOT", root.path().as_os_str().to_owned()),
            (
                "WRIX_TEST_VM_CALLS",
                root.path().join("calls").into_os_string(),
            ),
            (
                "WRIX_TEST_VM_BUILDS",
                root.path().join("builds").into_os_string(),
            ),
            ("WRIX_TEST_VM_STATUS", "pass".into()),
            ("WRIX_TEST_VM_BUILD_STATUS", "pass".into()),
            ("GIT_CONFIG_GLOBAL", "/dev/null".into()),
            ("GIT_CONFIG_NOSYSTEM", "1".into()),
            ("GIT_CONFIG_COUNT", "1".into()),
            ("GIT_CONFIG_KEY_0", "safe.directory".into()),
            ("GIT_CONFIG_VALUE_0", root.path().as_os_str().to_owned()),
            ("TMPDIR", "/tmp".into()),
            (
                "XDG_RUNTIME_DIR",
                root.path().join("ambient-runtime").into_os_string(),
            ),
        ] {
            environment.insert(name.into(), value);
        }
        for name in ["calls", "builds"] {
            fs::write(root.path().join(name), "").unwrap();
        }
        let fixture = Self {
            root,
            environment,
            library: env::var_os("WRIX_TEST_VM_LIBRARY").unwrap().into(),
            bash: env::var_os("WRIX_TEST_VM_BASH").unwrap().into(),
            lifecycle: env::var_os("WRIX_TEST_VM_LIFECYCLE").unwrap().into(),
            cache: env::var_os("WRIX_TEST_VM_CACHE").unwrap().into(),
            system: env::var("WRIX_TEST_VM_SYSTEM").unwrap(),
        };
        let result = output(fixture.command("git").args(["init", "-q"]));
        assert!(result.status.success(), "{result:?}");
        fixture
    }

    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(self.root.path())
            .env_clear()
            .envs(&self.environment);
        command
    }

    fn helper(&self, uid: u32, masks: [String; 3]) -> Output {
        output(
            self.command(&self.bash)
                .args([
                    "-c",
                    "source \"$1\"; verifier_virtiofsd_capability_set \"$2\" \"$3\" \"$4\" \"$5\"",
                    "fixture",
                ])
                .arg(&self.library)
                .arg(uid.to_string())
                .args(masks),
        )
    }

    fn invoke(&self, requirement: &Value, program: &Path, arguments: &[&str]) -> Output {
        output(self.command(&self.bash)
            .args(["-c", "source \"$1\"; shift; requirement=$1; shift; verifier_run fixture \"$requirement\" \"$@\"", "fixture"])
            .arg(&self.library).arg(requirement.to_string()).arg(program).args(arguments))
    }

    fn wrapper(&self, package: &Path, guest: &str, build: &str) -> Output {
        output(
            self.command(&self.bash)
                .args([
                    "-c",
                    "source \"$1\"; verifier_run fixture \"$2\" \"$3\"",
                    "fixture",
                ])
                .arg(&self.library)
                .arg(requirements(false).to_string())
                .arg(executable(package))
                .env("WRIX_TEST_VM_STATUS", guest)
                .env("WRIX_TEST_VM_BUILD_STATUS", build),
        )
    }

    fn file(&self, name: &str) -> String {
        fs::read_to_string(self.root.path().join(name)).unwrap()
    }
    fn execution(&self, virtiofs: bool) -> Value {
        let mut value = requirements(virtiofs);
        value["platform"] = self.system.clone().into();
        value
    }
}

fn executable(package: &Path) -> PathBuf {
    let name = package
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .split_once('-')
        .unwrap()
        .1;
    package.join("bin").join(name)
}

fn record(result: &Output) -> Value {
    serde_json::from_slice(&result.stdout).unwrap()
}
fn masks() -> [String; 3] {
    [
        format!("{:016x}", required_bits()),
        "0000000000000000".into(),
        "0000000000000000".into(),
    ]
}

#[test]
#[ignore = "requires actual Linux Nix-packaged system-test runners"]
fn each_missing_root_capability_has_a_specific_supported_reason() {
    let fixture = Fixture::new();
    for (name, bit) in CAPABILITIES {
        let result = fixture.helper(
            0,
            [
                format!("{:016x}", required_bits() & !(1u64 << bit)),
                "0000000000000000".into(),
                "0000000000000000".into(),
            ],
        );
        assert_eq!(result.status.code(), Some(77), "{result:?}");
        let reason = record(&result);
        assert_eq!(reason["kind"], "missing-capability");
        assert_eq!(reason["capability"], "virtiofsd-capabilities");
        assert!(
            reason["reason"].as_str().unwrap().contains(name),
            "{reason}"
        );
    }
}

#[test]
#[ignore = "requires actual Linux Nix-packaged system-test runners"]
fn complete_root_capability_set_remains_enabled_without_kvm() {
    let result = Fixture::new().helper(0, masks());
    assert!(result.status.success(), "{result:?}");
    assert_eq!(result.stdout, b"");
}

#[test]
#[ignore = "requires actual Linux Nix-packaged system-test runners"]
fn permitted_or_inheritable_capabilities_prevent_a_false_bounding_skip() {
    let fixture = Fixture::new();
    for index in [1, 2] {
        let mut masks = [
            "0000000000000000".to_owned(),
            "0000000000000000".to_owned(),
            "0000000000000000".to_owned(),
        ];
        masks[index] = format!("{:016x}", required_bits());
        let result = fixture.helper(0, masks);
        assert!(result.status.success(), "{result:?}");
        assert_eq!(result.stdout, b"");
    }
}

#[test]
#[ignore = "requires actual Linux Nix-packaged system-test runners"]
fn unprivileged_virtiofsd_does_not_install_root_capabilities() {
    let result = Fixture::new().helper(
        65534,
        [
            "0000000000000000".into(),
            "0000000000000000".into(),
            "0000000000000000".into(),
        ],
    );
    assert!(result.status.success(), "{result:?}");
}

#[test]
#[ignore = "requires actual Linux Nix-packaged system-test runners"]
fn invalid_kernel_capability_data_fails_instead_of_skipping() {
    let fixture = Fixture::new();
    for invalid in ["", "not-hex", "00000000800405fb extra", "0"] {
        for index in 0..3 {
            let mut masks = std::array::from_fn(|_| format!("{:016x}", required_bits()));
            masks[index] = invalid.into();
            let result = fixture.helper(0, masks);
            assert_eq!(result.status.code(), Some(1), "{result:?}");
            assert!(
                String::from_utf8_lossy(&result.stderr).contains("invalid Linux capability"),
                "{result:?}"
            );
            assert_eq!(result.stdout, b"");
        }
    }
}

#[test]
#[ignore = "requires actual Linux Nix-packaged system-test runners"]
fn kernel_preflight_retains_metadata_and_never_executes_when_unavailable() {
    let fixture = Fixture::new();
    let executed = fixture.root.path().join("executed");
    let result = fixture.invoke(
        &requirements(true),
        Path::new("touch"),
        &[executed.to_str().unwrap()],
    );
    let status = fs::read_to_string("/proc/self/status").unwrap();
    let uid: u32 = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let kernel_masks: Vec<_> = ["CapBnd", "CapPrm", "CapInh"]
        .into_iter()
        .map(|name| {
            let text = status
                .lines()
                .find_map(|line| line.strip_prefix(&format!("{name}:")))
                .unwrap();
            (name, u64::from_str_radix(text.trim(), 16).unwrap())
        })
        .collect();
    let available_bits = kernel_masks.iter().fold(0, |bits, (_, mask)| bits | mask);
    let available = uid != 0 || available_bits & required_bits() == required_bits();
    assert_eq!(
        result.status.code(),
        Some(if available { 0 } else { 77 }),
        "{result:?}"
    );
    let record = record(&result);
    assert_eq!(record["execution"], fixture.execution(true));
    assert_eq!(
        record["outcome"],
        if available { "passed" } else { "skipped" }
    );
    assert_eq!(executed.exists(), available);
    if !available {
        assert_eq!(
            record["skip_reason"]["capability"],
            "virtiofsd-capabilities"
        );
        for (name, mask) in kernel_masks {
            assert!(
                record["skip_reason"]["reason"]
                    .as_str()
                    .unwrap()
                    .contains(&format!("{name}={mask:016x}")),
                "{record}"
            );
        }
    }
}

#[test]
#[ignore = "requires actual Linux Nix-packaged system-test runners"]
fn direct_app_checks_prerequisites_before_driver_preparation() {
    let fixture = Fixture::new();
    let probe = output(
        fixture
            .command(&fixture.bash)
            .args([
                "-c",
                "source \"$1\"; verifier_preflight \"$2\" \"$3\"",
                "fixture",
            ])
            .arg(&fixture.library)
            .arg(&fixture.system)
            .arg(requirements(false).to_string()),
    );
    assert!(matches!(probe.status.code(), Some(0 | 77)), "{probe:?}");
    for package in [&fixture.lifecycle, &fixture.cache] {
        fs::write(fixture.root.path().join("builds"), "").unwrap();
        let result = output(
            fixture
                .command(executable(package))
                .env("WRIX_TEST_VM_BUILD_STATUS", "19"),
        );
        assert_eq!(
            result.status.code(),
            Some(if probe.status.code() == Some(77) {
                77
            } else {
                1
            }),
            "{result:?}"
        );
        assert_eq!(!fixture.file("builds").is_empty(), probe.status.success());
        assert_eq!(fixture.file("calls"), "");
        if probe.status.code() == Some(77) {
            assert_eq!(
                serde_json::from_slice::<Value>(&result.stderr).unwrap(),
                record(&probe)
            );
        }
    }
}

#[test]
#[ignore = "requires actual Linux Nix-packaged system-test runners"]
fn capable_execution_builds_then_runs_the_driver_in_isolated_scratch() {
    let fixture = Fixture::new();
    for (package, name) in [
        (&fixture.lifecycle, "services-devshell-start-independent"),
        (&fixture.cache, "services-limit-mode-cache-endpoint"),
    ] {
        for file in ["calls", "builds"] {
            fs::write(fixture.root.path().join(file), "").unwrap();
        }
        let result = fixture.wrapper(package, "pass", "pass");
        assert!(result.status.success(), "{result:?}");
        let record = record(&result);
        assert_eq!(record["outcome"], "passed");
        assert_eq!(record["execution"], fixture.execution(false));
        assert_eq!(
            fixture.file("builds").lines().collect::<Vec<_>>(),
            [format!(
                "{}#legacyPackages.{}.systemTests.{name}.driver",
                fixture.root.path().display(),
                fixture.system
            )]
        );
        let calls = fixture.file("calls");
        let directories: Vec<_> = calls.lines().collect();
        assert_eq!(directories.len(), 1);
        assert_ne!(Path::new(directories[0]), fixture.root.path());
        assert!(!Path::new(directories[0]).exists());
    }
}

#[test]
#[ignore = "requires actual Linux Nix-packaged system-test runners"]
fn guest_assertion_failure_is_not_classified_from_its_log() {
    let fixture = Fixture::new();
    let result = fixture.wrapper(&fixture.lifecycle, "fail", "pass");
    assert_eq!(result.status.code(), Some(1), "{result:?}");
    let record = record(&result);
    assert_eq!(record["outcome"], "failed");
    assert!(
        record["evidence"]
            .as_str()
            .unwrap()
            .contains("can't apply the child capabilities")
    );
    assert!(record.get("skip_reason").is_none());
    let calls = fixture.file("calls");
    assert_eq!(calls.lines().count(), 1);
    assert!(!Path::new(calls.trim()).exists());
}

#[test]
#[ignore = "requires actual Linux Nix-packaged system-test runners"]
fn guest_exit_77_is_a_failure_not_a_prerequisite_skip() {
    let fixture = Fixture::new();
    let result = fixture.wrapper(&fixture.lifecycle, "skip", "pass");
    assert_eq!(result.status.code(), Some(1), "{result:?}");
    assert_eq!(record(&result)["outcome"], "failed");
}

#[test]
#[ignore = "requires actual Linux Nix-packaged system-test runners"]
fn driver_build_failures_never_execute_or_report_a_skip() {
    let fixture = Fixture::new();
    for status in ["19", "77"] {
        let result = fixture.wrapper(&fixture.lifecycle, "pass", status);
        assert_eq!(result.status.code(), Some(1), "{result:?}");
        let record = record(&result);
        assert_eq!(record["outcome"], "failed");
        assert!(
            record["evidence"]
                .as_str()
                .unwrap()
                .contains("fixture driver build")
        );
        assert!(record.get("skip_reason").is_none());
        assert_eq!(fixture.file("calls"), "");
    }
}
