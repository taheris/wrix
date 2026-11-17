use super::*;
use std::os::unix::fs::symlink;

#[test]
fn existing_control_and_outside_paths_are_never_overwritten() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("control");
    let outside = temporary.path().join("outside");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("foreign"), "preserved").unwrap();
    assert!(Server::bind(&root, &outside, 0).is_err());
    assert_eq!(
        fs::read_to_string(root.join("foreign")).unwrap(),
        "preserved"
    );
    fs::remove_dir_all(&root).unwrap();
    fs::write(&outside, "foreign outside").unwrap();
    assert!(Server::bind(&root, &outside, 0).is_err());
    assert_eq!(fs::read_to_string(&outside).unwrap(), "foreign outside");
}

#[test]
fn symlinked_control_roots_and_relative_fixture_paths_are_refused() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("control");
    let outside = temporary.path().join("outside");
    symlink(temporary.path(), &root).unwrap();
    assert!(Server::bind(&root, &outside, 0).is_err());
    assert!(!outside.exists());
    assert!(matches!(
        Server::bind(Path::new("relative"), &outside, 0),
        Err(Error::Arguments)
    ));
    assert!(matches!(
        Server::bind(&root, &root.join("inside"), 0),
        Err(Error::Arguments)
    ));
}

#[test]
fn protocol_whitespace_matches_bytes_strip_including_vertical_tab() {
    assert_eq!(trim(b" \t\n\r\x0b\x0cprobe\x0b\x0c"), b"probe");
    assert_eq!(trim(b"\xffprobe\0"), b"\xffprobe\0");
    assert_eq!(trim(b" \x0b\n"), b"");
}

#[test]
fn readiness_failure_preserves_existing_failure_evidence_instead_of_collecting_it() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
    }
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("control");
    let outside = temporary.path().join("outside");
    let server = Server::bind(&root, &outside, 0).unwrap();
    assert!(server.port().unwrap() > 0);
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
        0o644
    );
    assert!(server.serve(&mut Broken).is_err());
    assert!(root.join("token").is_file());
    assert_eq!(fs::read(root.join("unexpected")).unwrap(), b"");
    assert_eq!(
        fs::read(&outside).unwrap(),
        b"known-live-readable-service-control\n"
    );
}
