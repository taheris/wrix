use super::*;
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::fs::symlink,
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

fn fixture() -> (tempfile::TempDir, Namespace, super::super::Batch) {
    let root = tempfile::Builder::new()
        .prefix("wrix-retirement-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let namespace = Namespace::open(&root.path().join("sessions")).unwrap();
    let batch = namespace.create("/bin/sh".as_ref()).unwrap();
    fs::write(batch.directory.join("identity"), "original").unwrap();
    let pending = batch.directory.join("pending");
    crate::retention::initialize(&pending).unwrap();
    let generation = pending.join("generation-test");
    fs::create_dir(&generation).unwrap();
    fs::set_permissions(&generation, fs::Permissions::from_mode(0o700)).unwrap();
    let output = crate::retention::StorePath::parse(
        "/nix/store/00000000000000000000000000000000-retirement-fixture",
    )
    .unwrap();
    fs::write(
        generation.join("manifest"),
        format!("{}\n", output.0.display()),
    )
    .unwrap();
    for (name, target) in crate::retention::indexed(&BTreeSet::from([output])) {
        symlink(target.0, generation.join(name)).unwrap();
    }
    symlink("generation-test", pending.join("current")).unwrap();
    (root, namespace, batch)
}

#[test]
fn retirement_record_outlives_owner_lease_and_nested_metadata() {
    let (_root, namespace, batch) = fixture();
    let (directory, path, _) = stage(
        &namespace,
        &batch.directory,
        contents(&batch.directory).unwrap(),
    )
    .unwrap();
    for file in ["owner", "lease", "pending/generation-test/manifest"] {
        fs::remove_file(directory.join(file)).unwrap();
    }
    drop(batch);
    namespace.reap().unwrap();
    assert!(!directory.exists() && !path.exists());
    assert_eq!(fs::read_dir(&namespace.path).unwrap().count(), 2);
    namespace.reap().unwrap();
}

#[test]
fn published_intent_does_not_remove_a_batch_that_is_still_leased() {
    let (_root, namespace, batch) = fixture();
    let (directory, path, _) = stage(
        &namespace,
        &batch.directory,
        contents(&batch.directory).unwrap(),
    )
    .unwrap();
    fs::rename(&directory, &batch.directory).unwrap();
    namespace.reap().unwrap();
    assert!(batch.directory.exists());
    assert!(!path.exists());
    drop(batch);
    namespace.reap().unwrap();
    assert_eq!(fs::read_dir(&namespace.path).unwrap().count(), 2);
}

#[test]
fn retirement_rejects_unknown_content_before_any_unlink() {
    let (_root, namespace, batch) = fixture();
    let (directory, path, _) = stage(
        &namespace,
        &batch.directory,
        contents(&batch.directory).unwrap(),
    )
    .unwrap();
    fs::write(directory.join("foreign"), "preserve").unwrap();
    drop(batch);
    assert!(namespace.reap().is_err());
    assert!(directory.join("owner").exists() && path.exists());
    assert_eq!(
        fs::read_to_string(directory.join("foreign")).unwrap(),
        "preserve"
    );
}

#[test]
fn retirement_rejects_same_length_metadata_mutation_and_replaced_directory() {
    let (_root, namespace, batch) = fixture();
    let (directory, path, _) = stage(
        &namespace,
        &batch.directory,
        contents(&batch.directory).unwrap(),
    )
    .unwrap();
    fs::write(directory.join("identity"), "modified").unwrap();
    drop(batch);
    assert!(namespace.reap().is_err());
    assert!(directory.join("owner").exists());
    fs::write(directory.join("identity"), "original").unwrap();
    let other = namespace.path.parent().unwrap().join("preserved");
    fs::rename(&directory, &other).unwrap();
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(namespace.reap().is_err());
    assert!(other.join("owner").exists() && path.exists());
}

#[test]
fn unrecorded_retirement_and_unknown_record_fields_are_not_adopted() {
    let (_root, namespace, batch) = fixture();
    let (directory, path, _) = stage(
        &namespace,
        &batch.directory,
        contents(&batch.directory).unwrap(),
    )
    .unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["unknown"] = true.into();
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    drop(batch);
    assert!(namespace.reap().is_err());
    fs::remove_file(path).unwrap();
    assert!(namespace.reap().is_err());
    assert!(directory.join("owner").exists());
}

#[test]
fn changed_registered_root_and_escaping_record_preserve_foreign_data() {
    let (root, namespace, batch) = fixture();
    let (directory, path, record) = stage(
        &namespace,
        &batch.directory,
        contents(&batch.directory).unwrap(),
    )
    .unwrap();
    let link = record
        .entries
        .iter()
        .find(|entry| matches!(&entry.kind, Kind::Link { target } if target.is_absolute()))
        .unwrap();
    fs::remove_file(directory.join(&link.path)).unwrap();
    symlink(
        "/nix/store/11111111111111111111111111111111-foreign",
        directory.join(&link.path),
    )
    .unwrap();
    drop(batch);
    assert!(namespace.reap().is_err());
    assert!(directory.join("owner").exists());
    let foreign = root.path().join("foreign");
    fs::write(&foreign, "preserve").unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["entries"][0]["path"] = "../foreign".into();
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(namespace.reap().is_err());
    assert_eq!(fs::read_to_string(foreign).unwrap(), "preserve");
}

#[test]
fn record_left_after_directory_removal_is_retired_without_touching_neighbors() {
    let (root, namespace, batch) = fixture();
    let (directory, path, record) = stage(
        &namespace,
        &batch.directory,
        contents(&batch.directory).unwrap(),
    )
    .unwrap();
    for entry in record
        .entries
        .iter()
        .filter(|entry| !matches!(entry.kind, Kind::Directory))
    {
        fs::remove_file(directory.join(&entry.path)).unwrap();
    }
    let mut directories = record
        .entries
        .iter()
        .filter(|entry| matches!(entry.kind, Kind::Directory))
        .collect::<Vec<_>>();
    directories.sort_by_key(|entry| std::cmp::Reverse(entry.path.components().count()));
    for entry in directories {
        fs::remove_dir(directory.join(&entry.path)).unwrap();
    }
    fs::remove_dir(&directory).unwrap();
    let neighbor = root.path().join("neighbor");
    fs::write(&neighbor, "preserve").unwrap();
    drop(batch);
    namespace.reap().unwrap();
    assert!(!path.exists());
    assert_eq!(fs::read_to_string(neighbor).unwrap(), "preserve");
}

#[test]
fn retirement_records_are_private_bounded_and_not_hardlinked() {
    let (root, namespace, batch) = fixture();
    let (_directory, path, _) = stage(
        &namespace,
        &batch.directory,
        contents(&batch.directory).unwrap(),
    )
    .unwrap();
    let bytes = fs::read(&path).unwrap();
    drop(batch);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(namespace.reap().is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let alias = root.path().join("alias");
    fs::hard_link(&path, &alias).unwrap();
    assert!(namespace.reap().is_err());
    fs::remove_file(alias).unwrap();
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(LIMIT + 1)
        .unwrap();
    assert!(namespace.reap().is_err());
    fs::write(&path, bytes).unwrap();
    namespace.reap().unwrap();
}

#[test]
#[ignore = "private killed-cleaner subprocess entry"]
fn killed_cleaner_entry() {
    let path = PathBuf::from(std::env::var_os("WRIX_TEST_RETIREMENT_NAMESPACE").unwrap());
    let namespace = Namespace::open(&path).unwrap();
    let batch = namespace.create("/bin/sh".as_ref()).unwrap();
    fs::write(batch.directory.join("identity"), "preserve until recovery").unwrap();
    let (directory, _, _) = stage(
        &namespace,
        &batch.directory,
        contents(&batch.directory).unwrap(),
    )
    .unwrap();
    fs::remove_file(directory.join("owner")).unwrap();
    fs::remove_file(directory.join("lease")).unwrap();
    println!("RETIREMENT-READY");
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

#[test]
fn killed_cleaner_is_recovered_without_recreating_owner_or_lease() {
    let root = tempfile::Builder::new()
        .prefix("wrix-killed-retirement-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let path = root.path().join("sessions");
    // This independent cleaner must not borrow leases from unrelated parallel unit tests.
    // Descriptor closing is confined to this private test child, never the dispatcher.
    let mut child = Command::new("bash")
        .args([
            "--noprofile",
            "--norc",
            "-c",
            r#"
            for descriptor in /dev/fd/*; do
                descriptor=${descriptor##*/}
                case "$descriptor" in 0|1|2|*[!0-9]*) continue ;; esac
                eval "exec $descriptor>&-"
            done
            exec "$@"
        "#,
            "retirement-test-cleaner",
        ])
        .arg(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "retention::session::retirement::test::killed_cleaner_entry",
            "--nocapture",
        ])
        .env("WRIX_TEST_RETIREMENT_NAMESPACE", &path)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let output = child.stdout.take().unwrap();
    let (send, receive) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            if line.unwrap().ends_with("RETIREMENT-READY") {
                send.send(()).unwrap();
                return;
            }
        }
    });
    let ready = receive.recv_timeout(Duration::from_secs(10));
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    reader.join().unwrap();
    ready.unwrap();
    super::super::clean(&path).unwrap();
    assert_eq!(fs::read_dir(path).unwrap().count(), 2);
}
