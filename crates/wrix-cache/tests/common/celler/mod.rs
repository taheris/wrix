use std::{
    fs::{self, File},
    io::Write,
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use attic::cache::CacheName;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;
use tempfile::TempDir;
use wrix_cache::backend::Client;

const SECRET: &str = "disposable-celler-test-credential-32-bytes";

pub fn in_isolated_store(test: &str) -> bool {
    if let Some(root) = std::env::var_os("WRIX_TEST_STORE_ROOT") {
        let marker = fs::read_to_string("/nix/var/nix/wrix-test-isolated-store").unwrap();
        assert_eq!(Path::new(&root), Path::new(&marker));
        return true;
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/cache/isolated-store.sh");
    let status = Command::new("unshare")
        .args(["-Urm", "bash"])
        .arg(script)
        .arg(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", test, "--nocapture"])
        .env("WRIX_UPSTREAM_SUBSTITUTERS", "")
        .env("WRIX_NIX_BIN", "/no-cli-metadata-or-transport")
        .env("WRIX_NIX_STORE_BIN", "/no-cli-metadata-or-transport")
        .status()
        .unwrap();
    assert!(status.success(), "isolated native fixture failed: {status}");
    false
}

pub struct Server {
    pub endpoint: String,
    pub token: String,
    child: Child,
    root: TempDir,
    database: PathBuf,
}

impl Server {
    pub fn start() -> Self {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("metadata.db");
        let archives = root.path().join("archives");
        fs::create_dir(&archives).unwrap();
        Self::spawn(root, database, &archives)
    }

    pub fn start_in_layout(layout: &wrix_cache::backend::layout::Layout) -> Self {
        layout.prepare().unwrap();
        Self::spawn(
            tempfile::tempdir().unwrap(),
            layout.metadata().join("metadata.db"),
            layout.archives(),
        )
    }

    fn spawn(root: TempDir, database: PathBuf, archives: &Path) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let endpoint = format!("http://{address}/");
        let path = root.path();
        fs::write(path.join("jwt.key"), SECRET).unwrap();
        let config = format!(
            "listen = {address:?}\nallowed-hosts = [{address:?}]\napi-endpoint = {endpoint:?}\nsoft-delete-caches = false\nrequire-proof-of-possession = true\n\
            [database]\nurl = {database:?}\n\
            [storage]\ntype = \"local\"\npath = {storage:?}\n\
            [chunking]\nnar-size-threshold = 0\nmin-size = 16384\navg-size = 65536\nmax-size = 262144\n\
            [compression]\ntype = \"none\"\n\
            [garbage-collection]\ninterval = \"0s\"\n\
            [jwt]\nhs256-secret-key-file = {key:?}\n",
            address = address.to_string(),
            database = format!("sqlite://{}?mode=rwc", database.display()),
            storage = archives.display().to_string(),
            key = path.join("jwt.key").display().to_string(),
        );
        fs::write(path.join("server.toml"), config).unwrap();
        let executable = std::env::var_os("WRIX_TEST_CELLERD")
            .expect("set WRIX_TEST_CELLERD to the pinned packaged cellerd");
        let log = File::create(path.join("server.log")).unwrap();
        let child = Command::new(executable)
            .arg("--config")
            .arg(path.join("server.toml"))
            .env("NIX_REMOTE", "unix:///no-host-store-daemon")
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let mut server = Self {
            endpoint,
            token: token(true),
            child,
            root,
            database,
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        while TcpStream::connect(address).is_err() {
            assert!(
                server.child.try_wait().unwrap().is_none(),
                "Celler exited: {}",
                fs::read_to_string(server.root.path().join("server.log")).unwrap()
            );
            assert!(
                Instant::now() < deadline,
                "Celler startup timeout: {}",
                fs::read_to_string(server.root.path().join("server.log")).unwrap()
            );
            thread::sleep(Duration::from_millis(20));
        }
        server
    }

    pub fn start_verification_only(layout: &wrix_cache::backend::layout::Layout) -> Self {
        assert_eq!(
            std::env::var("WRIX_TEST_RUNTIME_NAMESPACE").as_deref(),
            Ok("1")
        );
        layout.prepare().unwrap();
        let root = tempfile::tempdir().unwrap();
        let issuer = wrix_cache::backend::issuer::Authority::provision(
            &root.path().join("issuer"),
            layout.metadata(),
        )
        .unwrap();
        wrix_cache::backend::layout::runtime::install(layout.metadata()).unwrap();
        for mount in layout.mounts() {
            assert!(
                Command::new("mount")
                    .arg("--bind")
                    .arg(mount.source)
                    .arg(mount.destination)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let token = issuer
            .publication_token(&CacheName::new("private".into()).unwrap())
            .unwrap();
        let executable = std::env::var_os("WRIX_TEST_CELLERD").unwrap();
        let log = File::create(root.path().join("server.log")).unwrap();
        let child = Command::new(executable)
            .args(["--config", "/var/lib/wrix/celler/server.toml"])
            .env("NIX_REMOTE", "unix:///no-host-store-daemon")
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let mut server = Self {
            endpoint: String::from("http://127.0.0.1:8080/"),
            token,
            child,
            root,
            database: layout.metadata().join("metadata.db"),
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        while TcpStream::connect("127.0.0.1:8080").is_err() {
            assert!(
                server.child.try_wait().unwrap().is_none(),
                "{}",
                fs::read_to_string(server.root.path().join("server.log")).unwrap()
            );
            assert!(Instant::now() < deadline, "Celler startup timeout");
            thread::sleep(Duration::from_millis(20));
        }
        server
    }

    pub fn client(&self, namespace: &str) -> Client {
        Client::new(
            &self.endpoint,
            CacheName::new(namespace.to_owned()).unwrap(),
            &self.token,
        )
        .unwrap()
    }

    pub fn pull_client(&self) -> Client {
        Client::new(
            &self.endpoint,
            CacheName::new(String::from("private")).unwrap(),
            &token(false),
        )
        .unwrap()
    }

    pub fn corrupt_key(&self, namespace: &str) {
        let status = Command::new("python3")
            .args(["-c", "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('update cache set keypair=? where name=?', ('invalid-key',sys.argv[2])); c.commit()"])
            .arg(&self.database)
            .arg(namespace)
            .status().unwrap();
        assert!(status.success());
    }

    pub async fn get(&self, resource: &str) -> reqwest::Response {
        reqwest::Client::new()
            .get(format!("{}{resource}", self.endpoint))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}

pub fn pull_token() -> String {
    token(false)
}

fn token(management: bool) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let grants = if management {
        json!({
            "private":{"r":1,"w":1,"cc":1,"cr":1,"cq":1,"cd":1},
            "peer":{"r":1,"w":1,"cc":1,"cr":1,"cq":1,"cd":1}
        })
    } else {
        json!({"private":{"r":1}})
    };
    let claims = json!({
        "sub":"native-fixture", "exp":now + 3600,
        "https://jwt.attic.rs/v1":{"caches":grants}
    });
    let message = format!("{header}.{}", URL_SAFE_NO_PAD.encode(claims.to_string()));
    let mut child = Command::new("openssl")
        .args(["dgst", "-sha256", "-hmac", SECRET, "-binary"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(message.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    format!("{message}.{}", URL_SAFE_NO_PAD.encode(output.stdout))
}

pub fn add_file(contents: &str, name: &str) -> PathBuf {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(name);
    fs::write(&path, contents).unwrap();
    let output = Command::new("nix-store")
        .args(["--store", "daemon", "--add"])
        .arg(path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
}

pub fn hash(path: &Path) -> &str {
    &path.file_name().unwrap().to_str().unwrap()[..32]
}
