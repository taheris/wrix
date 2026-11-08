//! Real kernel-pipe cancellation of the native archive executable.
use sha2::{Digest as _, Sha256};
use std::{
    ffi::OsString,
    fs,
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[test]
fn consumer_failure_after_the_pipe_fills_is_nonzero_without_deadlock() {
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join("package");
    let custom = root.path().join("custom");
    fs::create_dir(&package).unwrap();
    fs::create_dir(&custom).unwrap();
    fs::write(package.join("payload"), vec![b'x'; 4 * 1024 * 1024]).unwrap();
    let empty_tar = vec![0; 10240];
    fs::write(custom.join("layer.tar"), &empty_tar).unwrap();
    fs::write(
        custom.join("checksum"),
        format!("{:x}", Sha256::digest(&empty_tar)),
    )
    .unwrap();
    let conf = root.path().join("conf.json");
    fs::write(&conf, serde_json::to_vec(&serde_json::json!({
        "from_image": null, "created":"1970-01-01T00:00:01Z", "mtime":"1970-01-01T00:00:01Z",
        "uid":"0", "gid":"0", "uname":"root", "gname":"root", "store_layers":[[package]],
        "customisation_layer":custom, "architecture":"amd64", "os":"linux", "config":{}, "repo_tag":"test:latest"
    })).unwrap()).unwrap();
    let layout = root.path().join("layout");
    let options =
        wrix_image_tools::layout::Options::parse(&[OsString::from(&conf), OsString::from(&layout)])
            .unwrap();
    wrix_image_tools::layout::build(&options).unwrap();
    let stderr = root.path().join("stderr");
    let mut child = Command::new(env!("CARGO_BIN_EXE_wrix-image-tools"))
        .env_clear()
        .args(["docker-archive"])
        .arg(&layout)
        .arg(&conf)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(fs::File::create(&stderr).unwrap())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    stdout.read_exact(&mut [0; 4096]).unwrap();
    thread::sleep(Duration::from_millis(50));
    drop(stdout);
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("native archive writer deadlocked after its consumer closed the pipe");
        }
        thread::sleep(Duration::from_millis(10));
    };
    assert!(!status.success());
    let diagnostic = fs::read_to_string(stderr).unwrap();
    assert!(
        diagnostic.contains("Broken pipe") || diagnostic.contains("broken pipe"),
        "{diagnostic}"
    );
}
