//! Disposable native HTTP backend using the CLI fixture's existing issuer verifier.

use std::{
    fs, io,
    net::TcpStream,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub struct Server {
    child: Child,
}

impl Server {
    pub fn start(root: &Path, port: u16, public: u16) -> super::TestResult<Self> {
        let layout = wrix_cache::backend::layout::Layout::resolve(
            if cfg!(target_os = "macos") {
                wrix_cache::backend::layout::Platform::Darwin
            } else {
                wrix_cache::backend::layout::Platform::Linux
            },
            &root.join("home"),
            Some(&root.join("state")),
            Some(&root.join("cache")),
        )?;
        let data = root.join("native-celler");
        fs::create_dir(&data)?;
        fs::create_dir(data.join("archives"))?;
        let config = format!(
            "listen = {address:?}\nallowed-hosts = [{address:?}, {public_address:?}]\napi-endpoint = {endpoint:?}\nsoft-delete-caches = false\nrequire-proof-of-possession = true\n\
            [database]\nurl = {database:?}\n\
            [storage]\ntype = \"local\"\npath = {archives:?}\n\
            [chunking]\nnar-size-threshold = 0\nmin-size = 16384\navg-size = 65536\nmax-size = 262144\n\
            [compression]\ntype = \"none\"\n\
            [garbage-collection]\ninterval = \"0s\"\n\
            [jwt]\nrs256-public-key-file = {verifier:?}\n",
            address = format!("127.0.0.1:{port}"),
            public_address = format!("127.0.0.1:{public}"),
            endpoint = format!("http://127.0.0.1:{public}/"),
            database = format!("sqlite://{}?mode=rwc", data.join("metadata.db").display()),
            archives = data.join("archives").display().to_string(),
            verifier = layout.metadata().join("jwt.pub").display().to_string(),
        );
        fs::write(data.join("server.toml"), config)?;
        let log = fs::File::create(data.join("server.log"))?;
        let child = Command::new(
            std::env::var_os("WRIX_TEST_CELLERD")
                .ok_or("missing pinned WRIX_TEST_CELLERD fixture")?,
        )
        .arg("--config")
        .arg(data.join("server.toml"))
        .env("NIX_REMOTE", "unix:///no-host-store-daemon")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
        let mut server = Self { child };
        let deadline = Instant::now() + Duration::from_secs(15);
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            if server.child.try_wait()?.is_some() || Instant::now() > deadline {
                return Err("native Celler fixture did not become ready".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(server)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let stopped = (|| -> io::Result<()> {
            if self.child.try_wait()?.is_none() {
                self.child.kill()?;
            }
            self.child.wait()?;
            Ok(())
        })();
        if let Err(error) = stopped {
            tracing::error!(%error, "native Celler fixture cleanup failed");
        }
    }
}
