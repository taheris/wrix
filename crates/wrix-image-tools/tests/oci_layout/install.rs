use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::MetadataExt, path::Path, process::Command};

fn configuration(root: &Path) -> std::path::PathBuf {
    let package = root.join("package");
    fs::create_dir(&package).unwrap();
    fs::write(package.join("payload"), b"payload").unwrap();
    let custom = root.join("custom");
    fs::create_dir(&custom).unwrap();
    assert!(
        Command::new("tar")
            .arg("-cf")
            .arg(custom.join("layer.tar"))
            .args(["--files-from", "/dev/null"])
            .status()
            .unwrap()
            .success()
    );
    fs::write(
        custom.join("checksum"),
        format!(
            "{:x}",
            Sha256::digest(fs::read(custom.join("layer.tar")).unwrap())
        ),
    )
    .unwrap();
    let conf = root.join("conf.json");
    fs::write(&conf,serde_json::to_vec(&serde_json::json!({
        "from_image":null,"created":"1970-01-01T00:00:01Z","mtime":"1970-01-01T00:00:01Z",
        "uid":"0","gid":"0","uname":"root","gname":"root","store_layers":[[package]],
        "customisation_layer":custom,"architecture":"arm64","os":"linux","repo_tag":"test:latest",
        "config":{"Env":["SETTING=kept"],"Labels":{"parent":"preserved"}}
    })).unwrap()).unwrap();
    conf
}

fn equivalent(first: &Path, second: &Path) {
    let names = |path: &Path| {
        let mut names = fs::read_dir(path)
            .unwrap()
            .map(|item| item.unwrap().file_name())
            .collect::<Vec<_>>();
        names.sort();
        names
    };
    assert_eq!(names(first), names(second));
    for name in names(first) {
        let a = first.join(&name);
        let b = second.join(&name);
        let metadata = fs::symlink_metadata(&a).unwrap();
        assert_eq!(metadata.mode(), fs::symlink_metadata(&b).unwrap().mode());
        if metadata.is_dir() {
            equivalent(&a, &b);
        } else if metadata.file_type().is_symlink() {
            assert_eq!(fs::read_link(&a).unwrap(), fs::read_link(&b).unwrap());
            assert_eq!(
                fs::metadata(&a).unwrap().ino(),
                fs::metadata(&b).unwrap().ino()
            );
        } else {
            assert_eq!(fs::read(&a).unwrap(), fs::read(&b).unwrap());
        }
    }
}

fn compare_install(destination: &Path) {
    let build = tempfile::tempdir().unwrap();
    let conf = configuration(build.path());
    let wrapper =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lib/sandbox/oci-layout-build.sh");
    for mode in [vec!["--store-layers-only"], vec![]] {
        let expected = build.path().join("original");
        let actual = destination.join("installed");
        assert!(
            Command::new(env!("CARGO_BIN_EXE_wrix-image-tools"))
                .arg("oci-layout")
                .arg(&conf)
                .arg(&expected)
                .args(&mode)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("bash")
                .arg(&wrapper)
                .arg(env!("CARGO_BIN_EXE_wrix-image-tools"))
                .arg(&conf)
                .args(&mode)
                .env("out", &actual)
                .env("NIX_BUILD_TOP", build.path())
                .status()
                .unwrap()
                .success()
        );
        equivalent(&expected, &actual);
        fs::remove_dir_all(expected).unwrap();
        fs::remove_dir_all(actual).unwrap();
    }
    fs::create_dir(destination.join("existing")).unwrap();
    fs::write(destination.join("existing/marker"), b"retain").unwrap();
    assert!(
        !Command::new("bash")
            .arg(wrapper)
            .arg(env!("CARGO_BIN_EXE_wrix-image-tools"))
            .arg(conf)
            .env("out", destination.join("existing"))
            .env("NIX_BUILD_TOP", build.path())
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        fs::read(destination.join("existing/marker")).unwrap(),
        b"retain"
    );
}

#[test]
fn same_device_install_preserves_both_modes_and_refuses_existing_output() {
    let destination = tempfile::tempdir().unwrap();
    compare_install(destination.path());
}

#[cfg(target_os = "linux")]
#[test]
fn cross_device_install_preserves_both_modes_and_refuses_existing_output() {
    let destination = tempfile::tempdir_in("/dev/shm").unwrap();
    assert_ne!(
        fs::metadata(destination.path()).unwrap().dev(),
        fs::metadata(std::env::temp_dir()).unwrap().dev()
    );
    compare_install(destination.path());
}
