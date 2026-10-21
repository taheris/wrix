#![cfg(test)]

use std::{
    fs,
    io::Read,
    net::{IpAddr, Ipv4Addr},
    os::unix::fs::{MetadataExt, PermissionsExt},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use wrix_sandbox::cache_credentials::{Clock, Error, Grant, Issuer, Renewal, Session, Time};

const LIFETIME: Duration = Duration::from_secs(3600);
const DUE: Duration = Duration::from_mins(55);
const HOST: IpAddr = IpAddr::V4(Ipv4Addr::new(169, 254, 1, 2));

#[derive(Clone)]
struct Manual(Arc<Mutex<Time>>);

impl Manual {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Time {
            wall: UNIX_EPOCH + Duration::from_secs(1_000_000),
            monotonic: Duration::ZERO,
        })))
    }
    fn advance(&self, wall: Duration, monotonic: Duration) {
        let mut now = self.0.lock().unwrap();
        now.wall += wall;
        now.monotonic += monotonic;
    }
}
impl Clock for Manual {
    fn now(&self) -> Time {
        *self.0.lock().unwrap()
    }
}

struct Source {
    clock: Manual,
    calls: Arc<AtomicUsize>,
    unavailable: Arc<AtomicBool>,
}
impl Issuer for Source {
    fn issue(&mut self) -> Result<Grant, Error> {
        let number = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(Error::Unavailable);
        }
        Grant::new(format!("pull.{number}"), self.clock.now().wall + LIFETIME)
    }
}

struct Fixture {
    clock: Manual,
    calls: Arc<AtomicUsize>,
    unavailable: Arc<AtomicBool>,
}
impl Fixture {
    fn new() -> Self {
        Self {
            clock: Manual::new(),
            calls: Arc::new(AtomicUsize::new(0)),
            unavailable: Arc::new(AtomicBool::new(false)),
        }
    }
    fn session(&self) -> Session<Source, Manual> {
        Session::create(
            Source {
                clock: self.clock.clone(),
                calls: self.calls.clone(),
                unavailable: self.unavailable.clone(),
            },
            self.clock.clone(),
            HOST,
        )
        .unwrap()
    }
}

#[test]
fn session_issuance_has_no_host_publication_dependency() {
    let fixture = Fixture::new();
    let first = fixture.session();
    let second = fixture.session();
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
    assert_ne!(first.directory(), second.directory());
    assert_eq!(
        fs::read_to_string(first.netrc()).unwrap(),
        "machine 169.254.1.2 login wrix password pull.1\n"
    );
    assert_eq!(
        fs::read_to_string(second.netrc()).unwrap(),
        "machine 169.254.1.2 login wrix password pull.2\n"
    );
}

#[test]
fn renewal_engine_reissues_across_multiple_token_lifetimes() {
    let fixture = Fixture::new();
    let mut session = fixture.session();
    let netrc = session.netrc();
    for number in 2..6 {
        fixture.clock.advance(DUE, DUE);
        assert_eq!(session.tick(), Renewal::Replaced);
        assert_eq!(session.netrc(), netrc);
        assert_eq!(session.expires_at(), fixture.clock.now().wall + LIFETIME);
        assert!(
            fs::read_to_string(&netrc)
                .unwrap()
                .ends_with(&format!("pull.{number}\n"))
        );
        assert_eq!(session.tick(), Renewal::Unchanged);
    }
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 5);
}

#[test]
fn session_files_are_replaced_atomically_without_changing_peer_credentials() {
    let fixture = Fixture::new();
    let mut session = fixture.session();
    let peer = fixture.session();
    let directory = session.directory();
    let inode = fs::metadata(&directory).unwrap().ino();
    let peer_bytes = fs::read(peer.netrc()).unwrap();
    assert_eq!(
        fs::metadata(directory.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(session.netrc()).unwrap().permissions().mode() & 0o777,
        0o444
    );
    let finished = Arc::new(AtomicBool::new(false));
    let stop = finished.clone();
    let netrc = session.netrc();
    let reader = thread::spawn(move || {
        let mut reads = 0;
        while !stop.load(Ordering::SeqCst) || reads == 0 {
            let bytes = fs::read_to_string(&netrc).unwrap();
            let number = bytes
                .strip_prefix("machine 169.254.1.2 login wrix password pull.")
                .unwrap()
                .strip_suffix('\n')
                .unwrap();
            assert!(number.parse::<usize>().unwrap() >= 1);
            reads += 1;
        }
        reads
    });
    for _ in 0..100 {
        fixture.clock.advance(DUE, DUE);
        assert_eq!(session.tick(), Renewal::Replaced);
    }
    finished.store(true, Ordering::SeqCst);
    assert!(reader.join().unwrap() > 0);
    assert_eq!(fs::metadata(&directory).unwrap().ino(), inode);
    assert_eq!(fs::read(peer.netrc()).unwrap(), peer_bytes);
    assert_eq!(fs::read_dir(directory).unwrap().count(), 1);
}

#[test]
fn renewal_engine_failures_preserve_protocol_exit_status_and_expiry() {
    let fixture = Fixture::new();
    let mut session = fixture.session();
    let expiry = session.expires_at();
    let original = fs::read(session.netrc()).unwrap();
    fixture.clock.advance(DUE, DUE);
    fixture.unavailable.store(true, Ordering::SeqCst);
    assert_eq!(session.tick(), Renewal::Retrying);
    assert_eq!(session.tick(), Renewal::Unchanged);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
    assert_eq!(session.expires_at(), expiry);
    assert_eq!(fs::read(session.netrc()).unwrap(), original);
    fixture
        .clock
        .advance(Duration::from_secs(30), Duration::from_secs(30));
    assert_eq!(session.tick(), Renewal::Retrying);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 3);
    let mut child = Command::new("sh")
        .args(["-c", "printf '%s\\n' '{\"event\":\"complete\"}'; exit 23"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let status = session.wait(&mut child).unwrap();
    let mut protocol = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut protocol)
        .unwrap();
    assert_eq!(status.code(), Some(23));
    assert_eq!(protocol, "{\"event\":\"complete\"}\n");
}

#[test]
fn renewal_engine_rechecks_absolute_expiry_after_suspension_and_clock_changes() {
    let fixture = Fixture::new();
    let mut session = fixture.session();
    fixture.clock.advance(DUE, DUE);
    fixture.unavailable.store(true, Ordering::SeqCst);
    assert_eq!(session.tick(), Renewal::Retrying);
    fixture.clock.advance(LIFETIME * 3, Duration::ZERO);
    fixture.unavailable.store(false, Ordering::SeqCst);
    assert_eq!(session.tick(), Renewal::Replaced);
    assert_eq!(session.expires_at(), fixture.clock.now().wall + LIFETIME);
    let expiry = session.expires_at();
    fixture.clock.0.lock().unwrap().wall -= LIFETIME;
    assert_eq!(session.tick(), Renewal::Unchanged);
    assert_eq!(session.expires_at(), expiry);
}

#[test]
fn waiting_session_completion_stops_renewal_and_cleans_credentials() {
    let fixture = Fixture::new();
    let session = fixture.session();
    let root = session.directory().parent().unwrap().to_owned();
    let mut child = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
    assert!(session.wait(&mut child).unwrap().success());
    assert!(!root.exists());
    let calls = fixture.calls.load(Ordering::SeqCst);
    fixture.clock.advance(LIFETIME * 10, LIFETIME * 10);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), calls);
}

#[test]
fn malformed_or_expired_grants_never_become_native_credentials() {
    struct Expired;
    impl Issuer for Expired {
        fn issue(&mut self) -> Result<Grant, Error> {
            Grant::new("expired".into(), UNIX_EPOCH)
        }
    }
    for bearer in [
        "",
        "secret\nextra",
        "secret password injected",
        "credential\0value",
    ] {
        let error = Grant::new(bearer.to_owned(), SystemTime::now())
            .err()
            .unwrap();
        assert!(matches!(error, Error::Credential));
        assert!(!error.to_string().contains("secret"));
    }
    assert!(matches!(
        Session::create(Expired, Manual::new(), HOST),
        Err(Error::Expired)
    ));
}
