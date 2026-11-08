//! Executable producer failures never publish a partial OCI output.
use std::{fs, os::unix::net::UnixListener, process::Command};

#[test]
fn early_and_late_producer_failures_do_not_publish_partial_output() {
    for socket_name in ["aaa-socket", "zzz-socket"] {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("package");
        fs::create_dir(&package).unwrap();
        fs::write(package.join("payload"), vec![b'x'; 4 * 1024 * 1024]).unwrap();
        let _socket = UnixListener::bind(package.join(socket_name)).unwrap();
        let conf = root.path().join("conf.json");
        fs::write(&conf, serde_json::to_vec(&serde_json::json!({
            "from_image": null, "created":"1970-01-01T00:00:01Z", "mtime":"1970-01-01T00:00:01Z",
            "uid":"0", "gid":"0", "uname":"root", "gname":"root", "store_layers":[[package]],
            "customisation_layer":"/not-needed", "architecture":"amd64", "os":"linux", "config":{}, "repo_tag":"test:latest"
        })).unwrap()).unwrap();
        let output = root.path().join("output");
        let result = Command::new(env!("CARGO_BIN_EXE_wrix-image-tools"))
            .arg("oci-layout")
            .arg(conf)
            .arg(&output)
            .arg("--store-layers-only")
            .output()
            .unwrap();
        assert!(!result.status.success());
        let stderr = String::from_utf8(result.stderr).unwrap();
        assert!(
            stderr.contains("unsupported Nix package file type"),
            "{stderr}"
        );
        assert!(!output.exists());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
    }
}
