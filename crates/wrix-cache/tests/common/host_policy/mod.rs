use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output},
};

pub struct Fixture {
    pub(super) root: tempfile::TempDir,
    pub(super) state: PathBuf,
    pub(super) cache: PathBuf,
    pub(super) key: String,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        // Metadata must be treated as data even when its paths contain shell syntax.
        let state = root
            .path()
            .join("state'\"$(touch MUST-NOT-EXIST)")
            .join("workspaces")
            .join("a".repeat(64));
        let cache = root.path().join("archives with spaces");
        fs::create_dir_all(state.join("keys")).unwrap();
        fs::create_dir(&cache).unwrap();
        let key = format!("retained-custom:{}", STANDARD.encode([0; 32]));
        fs::write(state.join("keys/cache.pub"), &key).unwrap();
        fs::write(
            root.path().join("config.json"),
            serde_json::to_vec_pretty(&wrix_cache::config::Settings::default()).unwrap(),
        )
        .unwrap();
        fs::write(
            root.path().join("endpoints.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "workspace_hash": "a".repeat(64), "state_root": state, "cache_root": cache,
                "endpoints": {"cache_http": {"host": "127.0.0.1", "port": 41000}}
            }))
            .unwrap(),
        )
        .unwrap();
        let fixture = Self {
            root,
            state,
            cache,
            key,
        };
        fixture.script(
            "service",
            r#"printf '%s\n' "$*" >> "$FIXTURE/service-calls"
case "$*" in
  'service cache config') cat "$FIXTURE/config.json" ;;
  'service endpoints') cat "$FIXTURE/endpoints.json" ;;
  *) exit 64 ;;
esac"#,
        );
        fixture.script(
            "store",
            r#"case "$1" in
  --add-fixed)
    output="$FIXTURE/hook-store"
    if [[ -d "$output" ]]; then chmod -R u+w "$output"; rm -rf "$output"; fi
    cp -R "${@: -1}" "$output"
    chmod -R a-w "$output"
    printf '%s\n' "$output"
    ;;
  --add-root) mkdir -p "$(dirname "$2")"; ln -sfn "${@: -1}" "$2" ;;
  *) exit 64 ;;
esac"#,
        );
        fixture.script(
            "hook",
            "printf '%s\\0' \"$@\" > \"$FIXTURE/hook-arguments\"",
        );
        fixture
    }

    fn script(&self, name: &str, body: &str) {
        let path = self.root.path().join(name);
        fs::write(
            &path,
            format!("#!/usr/bin/env bash\nset -euo pipefail\n{body}\n"),
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    pub(super) fn run(&self, configuration: &str) -> Output {
        Command::new("bash")
            .args([
                "-euc",
                r#"
source "$BOOTSTRAP"
printf '%s' "$NIX_CONFIG" > "$FIXTURE/host-config"
printf '\n--CALLER-CONFIG--\n%s\n' "$NIX_CONFIG"
"$WRIX_NIX_BIN" config show
"#,
            ])
            .current_dir(self.root.path())
            .env("FIXTURE", self.root.path())
            .env(
                "BOOTSTRAP",
                std::env::var_os("WRIX_TEST_HOST_BOOTSTRAP_SCRIPT").map_or_else(
                    || {
                        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                            .join("../../lib/services/host-nix-config.sh")
                    },
                    PathBuf::from,
                ),
            )
            .env("WRIX_SERVICE_BIN", self.root.path().join("service"))
            .env("WRIX_NIX_STORE_BIN", self.root.path().join("store"))
            .env("WRIX_NIX_BIN", "nix")
            .env(
                "WRIX_HOST_CACHE_POLICY_BIN",
                std::env::var_os("WRIX_TEST_HOST_CACHE_POLICY_BIN")
                    .unwrap_or_else(|| env!("CARGO_BIN_EXE_wrix-host-cache-policy").into()),
            )
            .env("WRIX_CACHE_HOOK_BIN", self.root.path().join("hook"))
            .env("WRIX_CACHE_PUBLISH_BIN", self.root.path().join("publisher"))
            .env("WRIX_NIX_CACHE_REMINDER", "0")
            .env("NIX_CONFIG", configuration)
            .env("NIX_CONF_DIR", self.root.path().join("no-system-config"))
            .env("NIX_USER_CONF_FILES", "/dev/null")
            .output()
            .unwrap()
    }
}
