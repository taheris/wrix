use std::{fs, process::Command};

#[test]
fn packaged_command_prepares_queries_and_extracts_docker_archive() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("image.tar");
    let mut inner = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(7);
    header.set_mode(0o755);
    header.set_cksum();
    inner
        .append_data(&mut header, "etc/marker", b"native\n".as_slice())
        .unwrap();
    let inner = inner.into_inner().unwrap();
    let mut outer = tar::Builder::new(Vec::new());
    for (name, data) in [
        ("layer", inner.as_slice()),
        ("config.json", b"{}".as_slice()),
        (
            "manifest.json",
            br#"[{"Config":"config.json","Layers":["layer"]}]"#.as_slice(),
        ),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        outer.append_data(&mut header, name, data).unwrap();
    }
    fs::write(&source, outer.into_inner().unwrap()).unwrap();
    let image = temporary.path().join("inspection");
    let layers = temporary.path().join("layers");
    fs::write(&layers, "layer\n").unwrap();
    let executable = env!("CARGO_BIN_EXE_wrix-verify-tools");
    let prepared = Command::new(executable)
        .arg("image-artifact")
        .arg("prepare")
        .arg(&source)
        .arg(&image)
        .output()
        .unwrap();
    assert!(prepared.status.success(), "{prepared:?}");
    let found = Command::new(executable)
        .args(["image-artifact", "find"])
        .arg(&image)
        .arg(&layers)
        .arg("etc/marker")
        .output()
        .unwrap();
    assert!(found.status.success(), "{found:?}");
    assert_eq!(found.stdout, b"etc/marker\n");
    let output = temporary.path().join("extracted");
    let extracted = Command::new(executable)
        .args(["image-artifact", "extract"])
        .arg(&image)
        .arg(&layers)
        .arg("etc/marker")
        .arg(&output)
        .output()
        .unwrap();
    assert!(extracted.status.success(), "{extracted:?}");
    assert_eq!(fs::read(output).unwrap(), b"native\n");
    let missing = Command::new(executable)
        .args(["image-artifact", "exists"])
        .arg(&image)
        .arg(&layers)
        .arg("absent")
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(missing.stdout, [] as [u8; 0]);
    assert_eq!(missing.stderr, [] as [u8; 0]);
    let malformed = Command::new(executable)
        .args(["image-artifact", "prepare"])
        .output()
        .unwrap();
    assert_eq!(malformed.status.code(), Some(2));
    assert!(
        String::from_utf8(malformed.stderr)
            .unwrap()
            .contains("usage:")
    );
    let help = Command::new(executable)
        .args(["image-artifact", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("prepare, paths, digests, extract, exists, link, find")
    );
}
