mod common;
#[cfg(test)]
#[path = "common/tool_cache.rs"]
mod coordination;

use std::{ffi::OsString, fs, path::PathBuf};

use common::{ChildSpec, ProfileFixture, TestResult};
use serde_json::{Value, json};
use wrix_sandbox::command::Command;

#[test]
#[ignore = "subprocess entrypoint"]
fn command_child() -> TestResult {
    common::run_command_child()
}

struct Fixture {
    root: tempfile::TempDir,
    workspace: PathBuf,
    profile: PathBuf,
    pool: PathBuf,
}

impl Fixture {
    fn new() -> TestResult<Self> {
        let root = tempfile::tempdir()?;
        let workspace = root.path().join("repository");
        fs::create_dir(&workspace)?;
        let output = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(&workspace)
            .output()?;
        assert!(output.status.success());
        let profile = root.path().join("profile.json");
        common::write_profile_config(&profile, &ProfileFixture::default())?;
        let mut value: Value = serde_json::from_slice(&fs::read(&profile)?)?;
        value["profile"]["tool_caches"] = json!(["cargo", "sccache", "uv"]);
        fs::write(&profile, serde_json::to_vec(&value)?)?;
        let pool = root
            .path()
            .canonicalize()?
            .join("cache/wrix/tool-caches/v1");
        Ok(Self {
            root,
            workspace,
            profile,
            pool,
        })
    }

    fn pool(&self) -> PathBuf {
        self.pool.clone()
    }

    fn launch(
        &self,
        command: Command,
        shared: Option<bool>,
        preview: bool,
    ) -> TestResult<common::ChildRun> {
        let args = if command == Command::Run {
            let mut args = Vec::new();
            if preview {
                args.extend(["--dry-run".into(), "--json".into()]);
            }
            if let Some(shared) = shared {
                args.push(if shared { "--cache" } else { "--no-cache" }.into());
            }
            args.push(self.workspace.display().to_string());
            args
        } else {
            let path = self.root.path().join("spawn.json");
            let mut value = json!({"workspace": self.workspace, "env": [], "agent_args": []});
            if let Some(shared) = shared {
                value["tool_caches"] = json!({"shared": shared});
            }
            fs::write(&path, serde_json::to_vec(&value)?)?;
            let mut args = vec!["--spawn-config".into(), path.display().to_string()];
            if preview {
                args.extend(["--dry-run".into(), "--json".into()]);
            }
            args
        };
        common::run_child(
            "command_child",
            self.root.path(),
            "launch",
            ChildSpec {
                command,
                profile_config: Some(self.profile.clone()),
                args,
                env: vec![(
                    "XDG_STATE_HOME".into(),
                    OsString::from(self.root.path().join("state")),
                )],
                capture_runtime: !preview,
            },
        )
    }
}

fn cache_mounts(output: &str) -> Vec<(PathBuf, String)> {
    output
        .lines()
        .filter_map(|arg| {
            let (host, rest) = arg.split_once(":/mnt/wrix/tool-caches/")?;
            Some((PathBuf::from(host), rest.trim_end_matches(":rw").to_owned()))
        })
        .collect()
}

#[test]
fn mode_precedence_follows_launch_repo_shared_default() -> TestResult {
    for command in [Command::Run, Command::Spawn] {
        let fixture = Fixture::new()?;
        for repo in [None, Some(false), Some(true)] {
            let policy = fixture.workspace.join("wrix.toml");
            if let Some(shared) = repo {
                fs::write(&policy, format!("[tool_caches]\nshared = {shared}\n"))?;
            }
            for explicit in [None, Some(false), Some(true)] {
                let result = fixture.launch(command, explicit, true)?;
                assert!(result.success, "{}", result.stderr);
                let report: Value = serde_json::from_str(&result.stdout)?;
                let expected = if explicit.or(repo).unwrap_or(true) {
                    "shared"
                } else {
                    "private"
                };
                assert_eq!(
                    report["plan"]["tool_cache_mode"], expected,
                    "{}",
                    result.stdout
                );
            }
        }
    }
    Ok(())
}

#[test]
fn launch_mode_overrides_do_not_write_repository_policy() -> TestResult {
    for command in [Command::Run, Command::Spawn] {
        let fixture = Fixture::new()?;
        let path = fixture.workspace.join("wrix.toml");
        for explicit in [false, true] {
            assert!(fixture.launch(command, Some(explicit), false)?.success);
            assert!(!path.exists());
        }
        for repo in [false, true, false] {
            let policy = format!("[tool_caches]\nshared = {repo}\n");
            fs::write(&path, &policy)?;
            assert!(fixture.launch(command, Some(!repo), false)?.success);
            assert_eq!(fs::read_to_string(&path)?, policy);
            let report = fixture.launch(command, None, true)?;
            let report: Value = serde_json::from_str(&report.stdout)?;
            assert_eq!(
                report["plan"]["tool_cache_mode"],
                if repo { "shared" } else { "private" }
            );
        }
    }
    Ok(())
}

#[test]
fn both_launch_modes_keep_tool_cache_preparation_host_separate() -> TestResult {
    for command in [Command::Run, Command::Spawn] {
        let fixture = Fixture::new()?;
        for dir in [
            "home/.cargo/registry",
            "home/.cargo/git",
            "home/.cache/sccache",
            "home/.cache/uv",
        ] {
            let path = fixture.root.path().join(dir);
            fs::create_dir_all(&path)?;
            fs::write(path.join("personal-sentinel"), "host private")?;
        }
        let preview = fixture.launch(command, None, true)?;
        assert!(preview.success, "{}", preview.stderr);
        assert!(!fixture.pool().exists());
        for _ in 0..2 {
            let result = fixture.launch(command, None, false)?;
            assert!(result.success, "{}", result.stderr);
            let mounts = cache_mounts(&result.stdout);
            assert_eq!(mounts.len(), 2);
            for (host, _) in &mounts {
                assert!(host.starts_with(fixture.pool()));
                assert!(!host.join("personal-sentinel").exists());
            }
            fs::remove_dir_all(fixture.pool())?;
        }
        for dir in [
            "home/.cargo/registry",
            "home/.cargo/git",
            "home/.cache/sccache",
            "home/.cache/uv",
        ] {
            assert_eq!(
                fs::read_to_string(fixture.root.path().join(dir).join("personal-sentinel"))?,
                "host private"
            );
        }
    }
    Ok(())
}

#[test]
fn shared_mode_uses_host_user_pool_across_repositories() -> TestResult {
    for command in [Command::Run, Command::Spawn] {
        let mut fixture = Fixture::new()?;
        let first = fixture.launch(command, None, false)?;
        assert!(first.success, "{}", first.stderr);
        fs::write(fixture.pool().join("uv/worker-sentinel"), "retained")?;
        fixture.workspace = fixture.root.path().join("other-checkout");
        fs::create_dir(&fixture.workspace)?;
        let second = fixture.launch(command, None, false)?;
        assert!(second.success, "{}", second.stderr);
        assert_eq!(cache_mounts(&first.stdout), cache_mounts(&second.stdout));
        assert_eq!(
            fs::read_to_string(fixture.pool().join("uv/worker-sentinel"))?,
            "retained"
        );
    }
    Ok(())
}

#[test]
fn private_mode_is_fresh_and_all_or_nothing() -> TestResult {
    for command in [Command::Run, Command::Spawn] {
        let fixture = Fixture::new()?;
        let mut previous = Vec::new();
        for _ in 0..2 {
            let result = fixture.launch(command, Some(false), false)?;
            assert!(result.success, "{}", result.stderr);
            let mounts = cache_mounts(&result.stdout);
            assert_eq!(
                mounts
                    .iter()
                    .map(|(_, name)| name.as_str())
                    .collect::<Vec<_>>(),
                ["cargo", "sccache", "uv"]
            );
            assert_ne!(mounts, previous);
            for (host, _) in &mounts {
                assert!(!host.starts_with(fixture.pool()));
                assert!(
                    !host.exists(),
                    "private backing must be disposed with launch"
                );
            }
            assert!(!fixture.pool().exists());
            previous = mounts;
        }
    }
    Ok(())
}

#[test]
fn mode_changes_do_not_migrate_or_delete_cache_data() -> TestResult {
    for command in [Command::Run, Command::Spawn] {
        let fixture = Fixture::new()?;
        assert!(fixture.launch(command, None, false)?.success);
        let sentinel = fixture.pool().join("cargo/registry/shared-sentinel");
        fs::write(&sentinel, "shared")?;
        assert!(fixture.launch(command, Some(false), false)?.success);
        assert_eq!(fs::read_to_string(&sentinel)?, "shared");
        assert!(fixture.launch(command, Some(true), false)?.success);
        assert_eq!(fs::read_to_string(&sentinel)?, "shared");
        assert_eq!(fs::read_dir(fixture.pool())?.count(), 2);
    }
    Ok(())
}

#[test]
fn shared_backing_excludes_credentials_and_control_state() -> TestResult {
    for command in [Command::Run, Command::Spawn] {
        let fixture = Fixture::new()?;
        assert!(fixture.launch(command, None, false)?.success);
        let mut entries = fs::read_dir(fixture.pool().join("cargo"))?
            .map(|entry| entry.map(|e| e.file_name()))
            .collect::<Result<Vec<_>, _>>()?;
        entries.sort();
        assert_eq!(
            entries,
            [
                ".global-cache",
                ".package-cache",
                ".package-cache-mutate",
                "git",
                "registry"
            ]
            .map(OsString::from)
        );
        assert!(
            !fixture.pool().join("sccache").exists(),
            "never share a raw disk cache among worker daemons"
        );
    }
    Ok(())
}

#[test]
fn preparation_failure_never_falls_back_to_personal_data() -> TestResult {
    for command in [Command::Run, Command::Spawn] {
        let fixture = Fixture::new()?;
        let personal = fixture.root.path().join("personal");
        fs::create_dir(&personal)?;
        fs::write(personal.join("sentinel"), "untouched")?;
        fs::create_dir_all(fixture.pool().parent().ok_or("missing parent")?)?;
        std::os::unix::fs::symlink(&personal, fixture.pool())?;
        let result = fixture.launch(command, None, false)?;
        assert!(!result.success);
        assert_eq!(fs::read_to_string(personal.join("sentinel"))?, "untouched");
        assert_eq!(fs::read_dir(&personal)?.count(), 1);
    }
    Ok(())
}

#[test]
fn native_cargo_workers_share_locks_tracking_and_git_data_not_configuration() -> TestResult {
    use coordination::{HttpFixture, link_home, success};
    use fs2::FileExt;
    use std::collections::BTreeMap;
    use std::{
        os::unix::fs::MetadataExt,
        process::{Command as Process, Stdio},
        time::{Duration, Instant},
    };

    let fixture = Fixture::new()?;
    assert!(fixture.launch(Command::Run, None, false)?.success);
    let dependency = fixture.root.path().join("dependency");
    fs::create_dir_all(dependency.join("src"))?;
    fs::write(
        dependency.join("Cargo.toml"),
        "[package]\nname = 'cachedependency'\nversion = '0.1.0'\nedition = '2024'\n",
    )?;
    fs::write(
        dependency.join("src/lib.rs"),
        "pub fn value() -> u32 { 100 }\n",
    )?;
    for args in [
        vec!["init", "--quiet"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.org",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "Initialize",
        ],
    ] {
        success(Process::new("git").current_dir(&dependency).args(args))?;
    }
    let archive = fixture.root.path().join("registrydemo.crate");
    let checksum = success(Process::new("python3").arg("-c").arg("import tarfile, io, hashlib, sys\nwith tarfile.open(sys.argv[1], 'w:gz') as t:\n for name, data in [('Cargo.toml', b'[package]\\nname = \\\"registrydemo\\\"\\nversion = \\\"0.1.0\\\"\\nedition = \\\"2024\\\"\\n'), ('src/lib.rs', b'pub fn value() -> u32 { 2 }\\n')]:\n  i = tarfile.TarInfo('registrydemo-0.1.0/' + name); i.size = len(data); t.addfile(i, io.BytesIO(data))\nprint(hashlib.sha256(open(sys.argv[1], 'rb').read()).hexdigest())\n").arg(&archive))?;
    let checksum = String::from_utf8(checksum.stdout)?.trim().to_owned();
    let archive = fs::read(&archive)?;
    let index = serde_json::to_vec(
        &json!({"name":"registrydemo", "vers":"0.1.0", "deps":[], "cksum":checksum, "features":{}, "yanked":false}),
    )?;
    let server = HttpFixture::start_with(|url| {
        BTreeMap::from([
            (
                "/index/config.json".into(),
                serde_json::to_vec(&json!({"dl":format!("{url}/crate")})).unwrap(),
            ),
            ("/index/re/gi/registrydemo".into(), index),
            ("/crate/registrydemo/0.1.0/download".into(), archive),
        ])
    })?;
    let cargo = fixture.pool().join("cargo");
    let download_lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(cargo.join(".package-cache"))?;
    download_lock.lock_exclusive()?;
    let mutate_lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(cargo.join(".package-cache-mutate"))?;
    mutate_lock.lock_exclusive()?;
    let mut workers = Vec::new();
    for index in 0..2 {
        let home = fixture.root.path().join(format!("worker-{index}"));
        link_home(&fixture.pool(), &home)?;
        for name in [".package-cache", ".package-cache-mutate", ".global-cache"] {
            assert_eq!(
                fs::metadata(home.join(".cargo").join(name))?.ino(),
                fs::metadata(cargo.join(name))?.ino()
            );
        }
        fs::write(
            home.join(".cargo/config.toml"),
            format!(
                "[build]\njobs = {}\n[registries.fixture]\nindex = 'sparse+{}/index/'\n",
                index + 1,
                server.url
            ),
        )?;
        fs::write(
            home.join(".cargo/credentials.toml"),
            "[registries.fixture]\ntoken = 'test-only'\n",
        )?;
        let project = fixture.root.path().join(format!("project-{index}"));
        fs::create_dir_all(project.join("src"))?;
        fs::write(
            project.join("Cargo.toml"),
            format!(
                "[package]\nname = 'worker{index}'\nversion = '0.1.0'\nedition = '2024'\n[dependencies]\ncachedependency = {{ git = 'file://{}' }}\nregistrydemo = {{ version = '=0.1.0', registry = 'fixture' }}\n",
                dependency.display()
            ),
        )?;
        fs::write(
            project.join("src/main.rs"),
            format!(
                "fn main() {{ println!(\"{{}}\", cachedependency::value() + registrydemo::value() + {index}); }}\n"
            ),
        )?;
        let log = fixture.root.path().join(format!("cargo-{index}.log"));
        let child = Process::new("cargo")
            .arg("build")
            .current_dir(&project)
            .env("HOME", &home)
            .env("CARGO_HOME", home.join(".cargo"))
            .env("CARGO_TARGET_DIR", project.join("target"))
            .env_remove("RUSTC_WRAPPER")
            .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
            .stdout(Stdio::null())
            .stderr(fs::File::create(&log)?)
            .spawn()?;
        workers.push((child, project, log));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while !workers.iter().all(|(_, _, log)| {
        fs::read_to_string(log)
            .unwrap()
            .contains("Blocking waiting for file lock")
    }) && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    let blocked = workers.iter().all(|(_, _, log)| {
        fs::read_to_string(log)
            .unwrap()
            .contains("Blocking waiting for file lock")
    });
    FileExt::unlock(&download_lock)?;
    std::thread::sleep(Duration::from_millis(100));
    let mutate_blocked = workers
        .iter_mut()
        .all(|(child, _, _)| child.try_wait().unwrap().is_none());
    FileExt::unlock(&mutate_lock)?;
    for (index, (child, project, log)) in workers.iter_mut().enumerate() {
        assert!(child.wait()?.success(), "{}", fs::read_to_string(log)?);
        let output = success(&mut Process::new(
            project.join(format!("target/debug/worker{index}")),
        ))?;
        assert_eq!(
            String::from_utf8(output.stdout)?.trim(),
            (102 + index).to_string()
        );
    }
    assert!(
        blocked,
        "Cargo must contend on the pooled native download lock"
    );
    assert!(
        mutate_blocked,
        "Cargo readers must contend on the pooled native mutation lock"
    );
    let output = success(Process::new("python3").arg("-c").arg("import sqlite3, sys\nc = sqlite3.connect(sys.argv[1])\nassert c.execute('pragma integrity_check').fetchone()[0] == 'ok'\nassert c.execute('select count(*) from git_db').fetchone()[0] == 1\nassert c.execute('select count(*) from git_checkout').fetchone()[0] >= 1\nassert c.execute('select count(*) from registry_src').fetchone()[0] == 1\nassert c.execute('select count(*) from registry_crate').fetchone()[0] == 1\n").arg(cargo.join(".global-cache")))?;
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert!(!cargo.join("config.toml").exists());
    assert!(!cargo.join("credentials.toml").exists());
    assert!(!cargo.join("target").exists());
    Ok(())
}

#[test]
#[ignore = "requires uv and python3 on PATH; run explicitly with the pinned uv package"]
fn native_uv_workers_concurrently_populate_and_reuse_the_managed_pool() -> TestResult {
    use coordination::{HttpFixture, link_home, success};
    use std::{
        collections::BTreeMap,
        process::{Command as Process, Stdio},
    };

    let fixture = Fixture::new()?;
    assert!(fixture.launch(Command::Spawn, None, false)?.success);
    let wheel_name = "cache_demo-1.0-py3-none-any.whl";
    let wheel = fixture.root.path().join(wheel_name);
    success(Process::new("python3").arg("-c").arg("import zipfile, sys\nwith zipfile.ZipFile(sys.argv[1], 'w') as z:\n z.writestr('cache_demo.py', 'VALUE = 71\\n')\n z.writestr('cache_demo-1.0.dist-info/METADATA', 'Metadata-Version: 2.1\\nName: cache-demo\\nVersion: 1.0\\n')\n z.writestr('cache_demo-1.0.dist-info/WHEEL', 'Wheel-Version: 1.0\\nGenerator: fixture\\nRoot-Is-Purelib: true\\nTag: py3-none-any\\n')\n z.writestr('cache_demo-1.0.dist-info/RECORD', '')\n").arg(&wheel))?;
    let server = HttpFixture::start(BTreeMap::from([
        (
            "/simple/cache-demo/".into(),
            format!("<a href='/wheel/{wheel_name}'>{wheel_name}</a>").into_bytes(),
        ),
        (format!("/wheel/{wheel_name}"), fs::read(&wheel)?),
    ]))?;
    let url = format!("{}/simple", server.url);
    let python =
        success(Process::new("python3").args(["-c", "import sys; print(sys.executable)"]))?;
    let python = String::from_utf8(python.stdout)?.trim().to_owned();
    let mut workers = Vec::new();
    for index in 0..2 {
        let home = fixture.root.path().join(format!("uv-home-{index}"));
        link_home(&fixture.pool(), &home)?;
        let target = fixture.root.path().join(format!("uv-target-{index}"));
        let child = Process::new("uv")
            .args([
                "pip",
                "install",
                "--no-config",
                "--no-python-downloads",
                "--python",
                &python,
                "--index-url",
                &url,
                "cache-demo==1.0",
                "--target",
            ])
            .arg(&target)
            .env("HOME", &home)
            .env("UV_CACHE_DIR", home.join(".cache/uv"))
            .env_remove("UV_NO_CACHE")
            .env_remove("UV_OFFLINE")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        workers.push((child, target));
    }
    for (child, target) in workers {
        let output = child.wait_with_output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read_to_string(target.join("cache_demo.py"))?,
            "VALUE = 71\n"
        );
    }
    drop(server);
    let home = fixture.root.path().join("uv-reuse");
    link_home(&fixture.pool(), &home)?;
    let target = fixture.root.path().join("uv-offline-target");
    success(
        Process::new("uv")
            .args([
                "pip",
                "install",
                "--offline",
                "--no-config",
                "--no-python-downloads",
                "--python",
                &python,
                "--index-url",
                &url,
                "cache-demo==1.0",
                "--target",
            ])
            .arg(&target)
            .env("HOME", &home)
            .env("UV_CACHE_DIR", home.join(".cache/uv"))
            .env_remove("UV_NO_CACHE"),
    )?;
    assert_eq!(
        fs::read_to_string(target.join("cache_demo.py"))?,
        "VALUE = 71\n"
    );
    Ok(())
}
