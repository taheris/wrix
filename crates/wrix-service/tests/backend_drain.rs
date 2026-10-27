//! These fixtures execute the same admission tracker compiled into the native Celler patch.
#[path = "../src/lifecycle/drain/mod.rs"]
pub mod drain;
#[path = "../../../lib/services/celler-work-drain-state.rs"]
mod native;

#[cfg(test)]
mod test {
    use super::{drain, native};
    use drain::{Command, Native};
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::{UnixListener, UnixStream},
        sync::Arc,
        time::Duration,
    };

    struct Boundary {
        tracker: Arc<native::Tracker>,
        commands: Vec<Command>,
    }
    impl Boundary {
        fn new() -> Self {
            Self {
                tracker: native::Tracker::new(),
                commands: Vec::new(),
            }
        }
    }
    impl Native for Boundary {
        fn command(&mut self, command: Command) -> Result<drain::Response, drain::Failure> {
            self.commands.push(command);
            let accepted = match command {
                Command::Begin => self.tracker.begin(),
                Command::Quiesce => self.tracker.quiescent(),
                Command::Status => true,
                Command::Abort => self.tracker.abort(),
                Command::Release => {
                    let release = self.tracker.release();
                    if release {
                        self.tracker.released();
                    }
                    release
                }
            };
            let wire = serde_json::json!({"accepted": accepted, "status": self.tracker.status()});
            Ok(serde_json::from_value(wire).unwrap())
        }
    }

    #[test]
    fn publication_idle_gap_remains_work_and_timeout_reopens_only_new_admission() {
        let mut boundary = Boundary::new();
        assert!(
            boundary
                .tracker
                .open_workflow("one".into(), "publisher".into())
        );
        assert!(matches!(
            drain::quiesce(&mut boundary, Duration::ZERO),
            Err(drain::Failure::Busy)
        ));
        assert_eq!(boundary.tracker.status().phase, native::Phase::Serving);
        assert_eq!(boundary.tracker.status().workflows, 1);
        assert!(!boundary.commands.contains(&Command::Release));
    }

    #[test]
    fn draining_preserves_admitted_workflow_but_rejects_new_publication() {
        let tracker = native::Tracker::new();
        assert!(tracker.open_workflow("one".into(), "publisher".into()));
        assert!(tracker.begin());
        assert!(!tracker.open_workflow("two".into(), "publisher".into()));
        assert!(tracker.request(None, "publisher").is_none());
        assert!(tracker.request(Some("one"), "other").is_none());
        let admitted = tracker.request(Some("one"), "publisher").unwrap();
        assert!(tracker.close_workflow("one", "publisher"));
        assert!(!tracker.release());
        drop(admitted);
        assert!(tracker.release());
        assert!(!tracker.abort());
    }

    #[test]
    fn detached_metadata_and_native_maintenance_outlive_http_completion() {
        let tracker = native::Tracker::new();
        let request = tracker.request(None, "reader").unwrap();
        let maintenance = tracker.maintenance().unwrap();
        let detached = tracker.background();
        tracker.begin();
        drop(request);
        assert!(tracker.maintenance().is_none());
        drop(maintenance);
        assert!(!tracker.release());
        drop(detached);
        assert!(tracker.release());
    }

    #[test]
    fn uncertain_metadata_completion_never_authorizes_clean_release() {
        let tracker = native::Tracker::new();
        tracker.begin();
        tracker.fail();
        tracker.background().uncertain();
        assert!(!tracker.release());
        assert!(tracker.abort());
    }

    #[test]
    fn coordinator_requires_native_closed_phase_not_just_empty_counters() {
        let mut boundary = Boundary::new();
        assert!(
            drain::quiesce(&mut boundary, Duration::ZERO)
                .unwrap()
                .drained()
        );
        assert_eq!(
            drain::release(&mut boundary).unwrap().phase,
            drain::Phase::Released
        );
        assert_eq!(
            boundary.commands,
            [Command::Begin, Command::Quiesce, Command::Release]
        );
    }

    #[test]
    fn private_exec_protocol_fixture_conforms_to_native_serialized_status() {
        let directory = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(directory.path().join("drain.sock")).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&mut stream).read_line(&mut line).unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&line).unwrap(),
                serde_json::json!({"command":"begin"})
            );
            let tracker = native::Tracker::new();
            tracker.begin();
            let mut bytes =
                serde_json::to_vec(&serde_json::json!({"accepted":true,"status":tracker.status()}))
                    .unwrap();
            bytes.push(b'\n');
            stream.write_all(&bytes).unwrap();
        });
        let mut stream = UnixStream::connect(directory.path().join("drain.sock")).unwrap();
        let response = drain::exchange(&mut stream, Command::Begin).unwrap();
        assert!(response.status.drained());
        server.join().unwrap();
    }

    #[test]
    fn fixed_role_transport_cannot_select_worker_supplied_paths() {
        assert_eq!(drain::Role::Celler.socket(), "/run/wrix/celler/drain.sock");
        assert_eq!(
            drain::Role::Compiler.socket(),
            "/run/wrix/compiler/drain.sock"
        );
        let _client = drain::Client::new(drain::Role::Celler, Duration::from_secs(2));
    }
}
