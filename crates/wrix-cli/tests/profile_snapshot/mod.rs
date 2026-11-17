use std::{
    fs::{self, FileTimes},
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
    process::{Command, Output},
    time::{Duration, UNIX_EPOCH},
};

use super::TestResult;

fn isolated(command: &mut Command) -> &mut Command {
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_COUNT",
    ] {
        command.env_remove(name);
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
}

fn git(root: &Path, arguments: &[&str]) -> TestResult {
    let output = isolated(Command::new("git").arg("-C").arg(root))
        .args(arguments)
        .output()?;
    assert!(output.status.success(), "{output:?}");
    Ok(())
}

fn snapshot(source: &Path, temporary: &Path) -> std::io::Result<Output> {
    isolated(&mut Command::new("bash"))
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/profiles/no-nightly-closure.sh"),
        )
        .arg("copy_tracked_flake")
        .env("REPO_ROOT", source)
        .env("TMPDIR", temporary)
        .output()
}

#[test]
fn tracked_profile_snapshot_preserves_working_content_without_shared_files() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let source = temporary.path().join("source with spaces");
    fs::create_dir(&source)?;
    git(&source, &["init", "--quiet", "--template="])?;
    fs::create_dir(source.join("nested"))?;
    fs::write(source.join("nested/payload"), "staged content")?;
    fs::hard_link(source.join("nested/payload"), source.join("alias"))?;
    fs::write(source.join("-binary\nname"), b"\0\xff\r\n")?;
    symlink("nested/payload", source.join("link"))?;
    git(
        &source,
        &[
            "add",
            "--",
            "nested/payload",
            "alias",
            "-binary\nname",
            "link",
        ],
    )?;
    fs::write(source.join("nested/payload"), "working content")?;
    fs::write(source.join("untracked"), "must not be copied")?;
    fs::set_permissions(
        source.join("nested/payload"),
        fs::Permissions::from_mode(0o751),
    )?;
    fs::File::open(source.join("nested/payload"))?.set_times(
        FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1_234_567_890)),
    )?;

    let output = snapshot(&source, temporary.path())?;
    assert!(output.status.success(), "{output:?}");
    let destination = std::path::PathBuf::from(String::from_utf8(output.stdout)?.trim_end());
    assert!(destination.starts_with(temporary.path().canonicalize()?));
    assert_eq!(
        fs::read(destination.join("nested/payload"))?,
        b"working content"
    );
    assert_eq!(fs::read(destination.join("-binary\nname"))?, b"\0\xff\r\n");
    assert_eq!(
        fs::read_link(destination.join("link"))?,
        Path::new("nested/payload")
    );
    assert!(!destination.join("untracked").exists());
    assert!(!destination.join(".git").exists());
    let original = fs::metadata(source.join("nested/payload"))?;
    let copied = fs::metadata(destination.join("nested/payload"))?;
    assert_eq!(copied.permissions().mode() & 0o777, 0o751);
    assert_eq!(copied.modified()?, original.modified()?);
    fs::write(destination.join("nested/payload"), "snapshot edit")?;
    assert_eq!(fs::read(destination.join("alias"))?, b"working content");
    assert_eq!(fs::read(source.join("nested/payload"))?, b"working content");
    Ok(())
}

#[test]
fn failed_tracked_snapshot_does_not_publish_an_empty_flake() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let source = temporary.path().join("not-a-repository");
    fs::create_dir(&source)?;
    let output = snapshot(&source, temporary.path())?;
    assert!(!output.status.success());
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert_eq!(fs::read_dir(temporary.path())?.count(), 1);
    Ok(())
}
