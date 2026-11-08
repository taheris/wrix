use super::*;
use std::{
    collections::BTreeMap,
    os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink},
};

fn private_payload(root: &Path) -> std::path::PathBuf {
    fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.join("environment");
    let mut file = fs::File::options()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    Payload::from_pairs([("TOKEN".into(), "synthetic\nsecret=value\n".into())])
        .unwrap()
        .write(&mut file)
        .unwrap();
    path
}

#[test]
fn only_image_pinned_targets_are_executable_and_arguments_remain_unchanged() {
    let payload = Payload::from_pairs([
        ("TOKEN".into(), "synthetic\nsecret=value\n".into()),
        ("CONTAINER_HOST".into(), "guest-only".into()),
        ("DYLD_INSERT_LIBRARIES".into(), "guest-only".into()),
    ])
    .unwrap();
    let args: Vec<OsString> = ["--flag", "", "with spaces\nand newline"]
        .into_iter()
        .map(OsString::from)
        .collect();
    let names = ["TOKEN", "CONTAINER_HOST", "DYLD_INSERT_LIBRARIES"];
    let before = names.map(std::env::var_os);
    for (name, executable) in [
        ("network", "/network-bootstrap.sh"),
        ("agent", "/entrypoint.sh"),
        ("krun-relay", "/krun-relay"),
        ("krun-init", "/krun-init.sh"),
    ] {
        let target = Target::parse(name.as_ref()).unwrap();
        let command = prepare(target, &payload, args.clone());
        assert_eq!(command.get_program(), executable);
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            args.iter().map(OsString::as_os_str).collect::<Vec<_>>()
        );
        let values: BTreeMap<_, _> = command
            .get_envs()
            .map(|(name, value)| (name.to_owned(), value.unwrap().to_owned()))
            .collect();
        assert_eq!(
            values[std::ffi::OsStr::new("TOKEN")],
            "synthetic\nsecret=value\n"
        );
        assert_eq!(values[std::ffi::OsStr::new("CONTAINER_HOST")], "guest-only");
        assert_eq!(
            values[std::ffi::OsStr::new("DYLD_INSERT_LIBRARIES")],
            "guest-only"
        );
        assert_eq!(values[std::ffi::OsStr::new(codec::READY_VARIABLE)], "1");
    }
    let unchanged = names.map(std::env::var_os) == before;
    assert!(unchanged, "guest overlay changed the caller environment");
    for name in ["", "/workspace/program", "../network", "network --flag"] {
        assert!(Target::parse(name.as_ref()).is_err());
    }
}

#[test]
fn regular_private_payload_is_read_without_changing_its_bytes() {
    let root = tempfile::tempdir().unwrap();
    let path = private_payload(root.path());
    let before = fs::read(&path).unwrap();
    let payload = load(&path).unwrap();
    assert_eq!(
        payload.environment().next().unwrap().1,
        "synthetic\nsecret=value\n"
    );
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn symlinks_public_modes_and_nonregular_files_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let path = private_payload(root.path());
    let link = root.path().join("link");
    symlink(&path, &link).unwrap();
    assert!(matches!(load(&link), Err(Error::Unsafe)));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(load(&path), Err(Error::Unsafe)));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(load(&path), Err(Error::Unsafe)));
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(
        load(&root.path().join("missing")),
        Err(Error::Missing)
    ));
    let directory = root.path().join("not-file");
    fs::create_dir(&directory).unwrap();
    assert!(matches!(load(&directory), Err(Error::Unsafe)));
}

#[test]
fn malformed_payload_reports_no_secret_values() {
    let root = tempfile::tempdir().unwrap();
    let path = private_payload(root.path());
    fs::write(&path, b"synthetic-private-malformed-value").unwrap();
    let error = load(&path).unwrap_err();
    assert!(
        !error
            .to_string()
            .contains("synthetic-private-malformed-value")
    );
    assert!(matches!(error, Error::Payload(codec::Error::Invalid)));
}
