use super::*;

#[test]
fn active_native_server_must_match_the_exact_selected_package_not_just_a_version() {
    let root = std::path::Path::new("/nix/store/00000000000000000000000000000000-container");
    let status = json!({
        "status":"running",
        "client":{"version":"1.5.0", "build":"release", "commit":"unknown", "appName":"container"},
        "server":{"version":"1.5.0", "build":"release", "commit":"unknown", "appName":"container-apiserver"},
        "paths":{"installRoot":root}
    });
    runtime_status(&serde_json::to_vec(&status).unwrap(), root).unwrap();
    for (pointer, replacement) in [
        ("/status", "not running"),
        ("/server/version", "1.4.0"),
        ("/server/commit", "other"),
        ("/paths/installRoot", "/usr/local"),
    ] {
        let mut changed = status.clone();
        *changed.pointer_mut(pointer).unwrap() = json!(replacement);
        assert!(runtime_status(&serde_json::to_vec(&changed).unwrap(), root).is_err());
    }
}
