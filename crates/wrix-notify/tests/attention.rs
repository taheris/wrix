#![cfg(test)]

mod common;

use common::{Fixture, edit_json};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, sync::Arc};
use wrix_notify::{
    attention,
    navigation::Outcome,
    route::{Platform, Route, Target},
};

#[test]
fn newest_first_deduplicated_and_consumed() {
    let f = Fixture::linux();
    f.register("a", 1);
    f.register("b", 2);
    for target in ["a", "b", "a"] {
        f.enqueue(target);
    }
    assert_eq!(f.pending(), ["a", "b"]);
    assert_eq!(f.calls().len(), 0);
    f.focus();
    assert_eq!(f.pending(), ["b"]);
    f.focus();
    f.focus();
    assert_eq!(f.visits(), ["1", "2"]);
}

#[test]
fn stale_registration_closed_window_and_changed_route_skipped() {
    let f = Fixture::linux();
    f.register("good", 1);
    let missing = f.register("missing", 2);
    f.register("closed", 101);
    f.register("changed", 1);
    for target in ["good", "missing", "closed", "changed"] {
        f.enqueue(target);
    }
    fs::remove_file(missing).unwrap();
    f.register("changed", 2);
    f.focus();
    assert_eq!(f.visits(), ["1"]);
    assert_eq!(f.pending().len(), 0);
}

#[test]
fn unregistered_and_filename_collision_not_queued() {
    let f = Fixture::linux();
    f.register("a:b", 1);
    f.enqueue("a/b");
    f.enqueue("missing");
    assert_eq!(f.pending().len(), 0);
    assert_eq!(
        Fixture::target(" host/é:\"quoted\"\n").filename(),
        "-host-----quoted--.json"
    );
}

#[test]
fn legacy_linux_route_is_invalidated_without_navigation() {
    let f = Fixture::linux();
    let path = f.register("legacy", 1);
    edit_json(&path, |value| {
        value.as_object_mut().unwrap().remove("linux_route");
    });
    f.enqueue("legacy");
    assert_eq!(f.pending().len(), 0);
    assert!(!f.focused("legacy"));
    assert_eq!(f.calls().len(), 0);
}

#[test]
fn malformed_route_identifiers_never_reach_navigation() {
    for field in ["tmux_target", "tmux_socket", "linux_route"] {
        let f = Fixture::linux();
        let path = f.register("a", 1);
        edit_json(&path, |v| {
            v[field] = json!(false);
        });
        f.enqueue("a");
        assert_eq!(f.pending().len(), 0);
        assert_eq!(f.calls().len(), 0);
    }
    for pid in [json!(0), json!(-1), json!(true), json!("10"), json!(1.5)] {
        let f = Fixture::linux();
        let path = f.register("a", 1);
        edit_json(&path, |v| {
            v["linux_route"]["terminal_pid"] = pid;
        });
        f.enqueue("a");
        assert_eq!(f.pending().len(), 0);
    }
    assert!(Target::try_from(String::new()).is_err());
}

#[test]
fn malformed_focus_information_does_not_suppress_or_consume_attention() {
    for windows in [
        json!(null),
        json!({}),
        json!(["invalid"]),
        json!([{"id":1,"pid":10,"is_focused":"true"}]),
    ] {
        let f = Fixture::linux();
        f.register("a", 1);
        f.enqueue("a");
        f.windows(windows);
        assert!(!f.focused("a"));
        f.focus();
        assert_eq!(f.pending(), ["a"]);
    }
}

#[test]
fn ambiguous_focused_windows_do_not_suppress() {
    let f = Fixture::linux();
    f.register("a", 1);
    f.windows(json!([{"id":1,"pid":10,"is_focused":true},{"id":2,"pid":20,"is_focused":true}]));
    assert!(!f.focused("a"));
}

#[test]
fn window_pid_mismatch_cannot_focus_unrelated_application() {
    let f = Fixture::linux();
    f.register("a", 1);
    f.enqueue("a");
    f.windows(json!([{"id":1,"pid":11,"is_focused":true}]));
    f.focus();
    assert_eq!(f.pending().len(), 0);
    assert_eq!(f.visits().len(), 0);
}

#[test]
fn return_restores_niri_window_and_is_single_use_without_consuming_attention() {
    let f = Fixture::linux();
    f.register("a", 1);
    f.enqueue("a");
    f.focus();
    assert_eq!(f.saved().unwrap()["window_id"], "99");
    assert_eq!(f.focused_window(), 1);
    f.enqueue("a");
    f.returning();
    assert_eq!(f.pending(), ["a"]);
    assert_eq!(f.focused_window(), 99);
    assert!(f.saved().is_none());
    f.returning();
    assert_eq!(f.visits(), ["1", "99"]);
}

#[test]
fn plain_desktop_origin_can_share_a_process_with_other_windows() {
    let f = Fixture::linux();
    f.register("a", 1);
    f.windows(json!([{"id":1,"pid":10},{"id":2,"pid":20,"is_focused":true},{"id":3,"pid":20}]));
    f.enqueue("a");
    f.focus();
    assert_eq!(f.saved().unwrap()["window_id"], "2");
    f.returning();
    assert_eq!(f.focused_window(), 2);
}

#[test]
fn empty_queue_does_not_replace_saved_origin() {
    let f = Fixture::linux();
    f.register("a", 1);
    f.enqueue("a");
    f.focus();
    let saved = f.saved();
    f.focus();
    assert_eq!(f.saved(), saved);
}

#[test]
fn unsuccessful_navigation_does_not_replace_saved_origin() {
    let f = Fixture::linux();
    f.register("a", 1);
    f.enqueue("a");
    f.focus();
    let saved = f.saved();
    f.register("b", 2);
    f.enqueue("b");
    f.fail_focus("2");
    f.focus();
    assert_eq!(f.saved(), saved);
    assert_eq!(f.pending(), ["b"]);
}

#[test]
fn successful_jump_with_unavailable_origin_clears_older_return_position() {
    let f = Fixture::linux();
    f.register("a", 1);
    f.enqueue("a");
    f.focus();
    assert!(f.saved().is_some());
    f.windows(json!([{"id":1,"pid":10},{"id":2,"pid":null,"is_focused":true}]));
    f.enqueue("a");
    f.focus();
    assert!(f.saved().is_none());
    assert_eq!(f.focused_window(), 1);
}

#[test]
fn stale_return_window_is_discarded_without_focus() {
    let f = Fixture::linux();
    f.register("a", 1);
    f.enqueue("a");
    f.focus();
    f.windows(json!([{"id":1,"pid":10,"is_focused":true}]));
    f.returning();
    assert!(f.saved().is_none());
    assert_eq!(f.visits(), ["1"]);
}

#[test]
fn failed_return_retains_saved_position_for_retry() {
    let f = Fixture::linux();
    f.register("a", 1);
    f.enqueue("a");
    f.focus();
    let saved = f.saved();
    f.fail_focus("99");
    f.returning();
    assert_eq!(f.saved(), saved);
    f.state.lock().unwrap().failures.clear();
    f.returning();
    assert_eq!(f.focused_window(), 99);
}

#[test]
fn malformed_saved_origin_is_discarded() {
    let f = Fixture::linux();
    for route in ["invalid", "null", "{\"window_id\":\"1\"}"] {
        attention::open(&f.directory)
            .unwrap()
            .execute(
                "INSERT OR REPLACE INTO return_position VALUES (1,?)",
                [route],
            )
            .unwrap();
        f.returning();
        assert!(f.saved().is_none());
        assert_eq!(f.calls().len(), 0);
    }
}

#[test]
fn darwin_return_is_explicitly_unsupported_and_does_not_change_state() {
    let f = Fixture::new(Platform::Darwin);
    attention::open(&f.directory)
        .unwrap()
        .execute(
            "INSERT INTO return_position VALUES (1,?)",
            ["{\"window_id\":\"2\"}"],
        )
        .unwrap();
    let saved = f.saved();
    assert!(matches!(
        attention::return_to_origin(&f.directory, f.platform, &f.navigator),
        Err(attention::Error::UnsupportedReturn)
    ));
    assert_eq!(f.saved(), saved);
    assert_eq!(f.calls().len(), 0);
}

#[test]
fn darwin_focused_origin_window_suppresses_and_title_is_data() {
    let f = Fixture::new(Platform::Darwin);
    let title = "Alacritty \"quoted\"; do shell script \"bad\"";
    f.mac("mac", title);
    assert!(f.focused("mac"));
    let calls = f.calls();
    let (program, args) = &calls[0];
    assert_eq!(program, "osascript");
    assert_eq!(&args[2..], ["Alacritty", title]);
    assert!(!args[1].contains(title));
    assert!(args[1].contains("AXFocusedWindow"));
}

#[test]
fn darwin_background_window_and_unexpected_focus_responses_do_not_suppress() {
    let f = Fixture::new(Platform::Darwin);
    f.mac("mac", "Origin");
    for response in ["false", "unknown", "stale", ""] {
        f.state.lock().unwrap().ax = Some(response.into());
        assert!(!f.focused("mac"));
    }
}

#[test]
fn darwin_unavailable_accessibility_does_not_suppress() {
    let f = Fixture::new(Platform::Darwin);
    f.mac("mac", "Origin");
    assert!(f.focused("mac"));
    let args = f.calls()[0].1.clone();
    f.state.lock().unwrap().failures.push(args);
    assert!(!f.focused("mac"));
}

#[test]
fn darwin_missing_origin_title_does_not_suppress_or_enqueue() {
    let f = Fixture::new(Platform::Darwin);
    let path = f.mac("mac", "Origin");
    edit_json(&path, |v| {
        v.as_object_mut().unwrap().remove("terminal_window_title");
    });
    f.enqueue("mac");
    assert!(!f.focused("mac"));
    assert_eq!(f.pending().len(), 0);
    assert_eq!(f.calls().len(), 0);
}

#[test]
fn darwin_unique_window_required_and_navigation_title_is_data() {
    let f = Fixture::new(Platform::Darwin);
    let title = "title; \"quoted\"";
    f.mac("mac", title);
    f.enqueue("mac");
    f.focus();
    assert_eq!(f.pending().len(), 0);
    assert_eq!(&f.calls()[0].1[2..], ["Alacritty", title]);
    assert!(!f.calls()[0].1[1].contains(title));
    assert!(f.calls()[0].1[1].contains("AXRaise"));
    f.state.lock().unwrap().ax = Some("stale".into());
    f.enqueue("mac");
    f.focus();
    assert_eq!(f.pending().len(), 0);
    f.state.lock().unwrap().ax = Some("unknown".into());
    f.enqueue("mac");
    f.focus();
    assert_eq!(f.pending(), ["mac"]);
}

#[test]
fn darwin_only_active_attached_pane_in_focused_window_suppresses() {
    let f = Fixture::new(Platform::Darwin);
    let path = f.mac("mac", "Origin");
    edit_json(&path, |v| {
        v["tmux_socket"] = json!("/socket");
        v["tmux_target"] = json!("mac:0.0");
    });
    f.response(
        &[
            "-S",
            "/socket",
            "display-message",
            "-p",
            "-t",
            "mac:0.0",
            "#{session_name}:#{window_index}.#{pane_index} #{pane_dead}",
        ],
        "mac:0.0 0",
    );
    let args = [
        "-S",
        "/socket",
        "display-message",
        "-p",
        "-t",
        "mac:0.0",
        "#{window_active} #{pane_active} #{?session_attached,1,0}",
    ];
    for response in ["0 1 1", "1 0 1", "1 1 0", "unknown", ""] {
        f.response(&args, response);
        assert!(!f.focused("mac"));
    }
    f.response(&args, "1 1 1");
    assert!(f.focused("mac"));
}

#[test]
fn linux_tmux_route_requires_matching_client_tty_ancestry_and_session() {
    let f = Fixture::linux();
    let route = f.tmux_route();
    f.tmux_ready();
    assert_eq!(f.navigator.validate(&route, false), Outcome::Success);
    let args = [
        "-S",
        "/socket",
        "list-clients",
        "-F",
        wrix_notify::navigation::CLIENT_FORMAT,
    ];
    for response in [
        "",
        "client|12|/dev/pts/1|$0|@0|%0",
        "client|11|/dev/pts/2|$0|@0|%0",
        "client|11|/dev/pts/1|$1|@0|%0",
        "client|11|/dev/pts/1|$0|@0|%0\nother|12|/dev/pts/2|$0|@0|%0",
    ] {
        f.response(&args, response);
        assert_eq!(f.navigator.validate(&route, false), Outcome::Stale);
    }
    f.tmux_ready();
    fs::remove_file(f.root.path().join("proc/11/fd/0")).unwrap();
    assert_eq!(f.navigator.validate(&route, false), Outcome::Stale);
}

#[test]
fn linked_or_dead_or_moved_panes_are_stale() {
    let f = Fixture::linux();
    let route = f.tmux_route();
    f.tmux_ready();
    let args = [
        "-S",
        "/socket",
        "list-panes",
        "-a",
        "-F",
        "#{pane_id}|#{session_id}",
    ];
    for response in ["%0|$1", "%0|$0\n%0|$1", ""] {
        f.response(&args, response);
        assert_eq!(f.navigator.validate(&route, false), Outcome::Stale);
    }
    f.tmux_ready();
    f.response(
        &[
            "-S",
            "/socket",
            "display-message",
            "-p",
            "-t",
            "%0",
            "#{pane_id} #{pane_dead} #{session_id}",
        ],
        "%0 1 $0",
    );
    assert_eq!(f.navigator.validate(&route, false), Outcome::Stale);
    f.response(
        &[
            "-S",
            "/socket",
            "display-message",
            "-p",
            "-t",
            "%0",
            "#{pane_id} #{pane_dead} #{session_id} #{window_id}",
        ],
        "%0 0 $0 @1",
    );
    assert_eq!(f.navigator.validate(&route, true), Outcome::Stale);
}

#[test]
fn return_switches_only_recorded_client_and_selects_saved_position() {
    let f = Fixture::linux();
    let route = f.tmux_route();
    f.tmux_ready();
    f.save(&route);
    f.response(
        &[
            "-S",
            "/socket",
            "list-clients",
            "-F",
            wrix_notify::navigation::CLIENT_FORMAT,
        ],
        "client|11|/dev/pts/1|$1|@1|%1\nother|12|/dev/pts/2|$2|@2|%2",
    );
    f.returning();
    assert!(f.saved().is_none());
    assert_eq!(f.visits(), ["1"]);
    let switches: Vec<_> = f
        .calls()
        .into_iter()
        .filter(|(_, args)| args.contains(&"switch-client".to_owned()))
        .collect();
    assert_eq!(switches.len(), 1);
    assert_eq!(
        switches[0].1,
        ["-S", "/socket", "switch-client", "-c", "client", "-t", "$0"]
    );
}

#[test]
fn ambiguous_return_session_does_not_select_shared_state() {
    let f = Fixture::linux();
    let route = f.tmux_route();
    f.tmux_ready();
    f.save(&route);
    f.response(
        &[
            "-S",
            "/socket",
            "list-clients",
            "-F",
            wrix_notify::navigation::CLIENT_FORMAT,
        ],
        "client|11|/dev/pts/1|$1|@1|%1\nother|12|/dev/pts/2|$0|@0|%0",
    );
    f.returning();
    assert!(f.saved().is_none());
    assert_eq!(f.visits().len(), 0);
    assert!(
        !f.calls()
            .iter()
            .any(|(_, args)| args.contains(&"select-pane".to_owned()))
    );
}

#[test]
fn stale_return_client_and_pane_are_discarded_without_switching() {
    for response in ["", "client|12|/dev/pts/1|$0|@0|%0"] {
        let f = Fixture::linux();
        let route = f.tmux_route();
        f.tmux_ready();
        f.save(&route);
        f.response(
            &[
                "-S",
                "/socket",
                "list-clients",
                "-F",
                wrix_notify::navigation::CLIENT_FORMAT,
            ],
            response,
        );
        f.returning();
        assert!(f.saved().is_none());
        assert_eq!(f.visits().len(), 0);
    }
    let f = Fixture::linux();
    let route = f.tmux_route();
    f.tmux_ready();
    f.save(&route);
    f.response(
        &[
            "-S",
            "/socket",
            "list-panes",
            "-a",
            "-F",
            "#{pane_id}|#{session_id}",
        ],
        "",
    );
    f.returning();
    assert!(f.saved().is_none());
    assert_eq!(f.visits().len(), 0);
}

#[test]
fn failed_tmux_query_retains_route_for_retry() {
    let f = Fixture::linux();
    let route = f.tmux_route();
    f.tmux_ready();
    f.save(&route);
    f.state.lock().unwrap().failures.push(vec![
        "-S".into(),
        "/socket".into(),
        "list-clients".into(),
        "-F".into(),
        wrix_notify::navigation::CLIENT_FORMAT.into(),
    ]);
    f.returning();
    assert!(f.saved().is_some());
}

#[test]
fn database_is_private_and_existing_python_queue_identity_remains_usable() {
    let f = Fixture::linux();
    f.register("a", 1);
    f.enqueue("a");
    let db = attention::open(&f.directory).unwrap();
    let text: String = db
        .query_row("SELECT route FROM pending", [], |r| r.get(0))
        .unwrap();
    let value: Value = serde_json::from_str(&text).unwrap();
    db.execute(
        "UPDATE pending SET route = ?",
        [serde_json::to_string_pretty(&value).unwrap()],
    )
    .unwrap();
    assert_eq!(
        fs::metadata(f.directory.join("attention.sqlite"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    f.focus();
    assert_eq!(f.visits(), ["1"]);
}

#[test]
fn concurrent_delivery_and_navigation_have_no_lost_entries() {
    let f = Arc::new(Fixture::linux());
    let windows: Vec<_> = (1..=40)
        .map(|i| {
            f.register(&i.to_string(), i);
            json!({"id":i,"pid":i*10,"is_focused":i==40})
        })
        .collect();
    f.windows(json!(windows));
    let mut threads = Vec::new();
    for i in 1..=40 {
        let f = f.clone();
        threads.push(std::thread::spawn(move || f.enqueue(&i.to_string())));
    }
    for _ in 0..20 {
        let f = f.clone();
        threads.push(std::thread::spawn(move || f.focus()));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    let mut observed = f.visits();
    observed.extend(f.pending());
    assert_eq!(observed.len(), 40);
    observed.sort();
    observed.dedup();
    assert_eq!(observed.len(), 40);
    assert_eq!(
        attention::open(&f.directory)
            .unwrap()
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}

#[test]
fn saved_routes_round_trip_without_registration_keys() {
    let f = Fixture::linux();
    let route = f.tmux_route();
    assert_eq!(
        Route::parse(&route.saved(), Platform::Linux).unwrap(),
        route
    );
}

#[test]
fn navigation_and_return_state_updates_are_serialized() {
    use std::sync::{Mutex, mpsc};
    use std::time::Duration;
    use wrix_notify::{
        navigation::Navigator,
        process::{Error, Runner},
    };

    struct Gate {
        inner: common::Stub,
        ready: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }
    impl Runner for Gate {
        fn run(&self, program: &str, args: &[&str]) -> Result<String, Error> {
            if args == ["msg", "action", "focus-window", "--id", "1"] {
                self.ready.send(()).unwrap();
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
            }
            self.inner.run(program, args)
        }
    }
    let f = Fixture::linux();
    f.register("first", 2);
    f.enqueue("first");
    f.focus();
    assert_eq!(f.saved().unwrap()["window_id"], "99");
    f.register("second", 1);
    f.enqueue("second");
    let (ready_sender, ready) = mpsc::channel();
    let (release, release_receiver) = mpsc::channel();
    let navigator = Navigator::new(
        Gate {
            inner: common::Stub(f.state.clone()),
            ready: ready_sender,
            release: Mutex::new(release_receiver),
        },
        f.root.path().join("proc"),
    );
    std::thread::scope(|scope| {
        let jumping =
            scope.spawn(|| attention::focus(&f.directory, f.platform, &navigator).unwrap());
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        let (done_sender, done) = mpsc::channel();
        let fixture = &f;
        let returning = scope.spawn(move || {
            fixture.returning();
            done_sender.send(()).unwrap();
        });
        let before_release = done.recv_timeout(Duration::from_millis(100));
        release.send(()).unwrap();
        assert!(matches!(
            before_release,
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        jumping.join().unwrap();
        returning.join().unwrap();
    });
    assert_eq!(f.focused_window(), 2);
    assert!(f.saved().is_none());
}

#[test]
fn native_lease_lifecycle_and_navigation() {
    use wrix_notify::route::registration;
    let f = Fixture::linux();
    let target = Fixture::target("native-fixture");
    let path = f.register(target.as_str(), 1);
    edit_json(&path, |value| {
        value["leased"] = json!(true);
    });
    let lease_path = path.with_extension("lease");
    let owner = fs::File::create(&lease_path).unwrap();
    owner.lock().unwrap();
    assert!(registration(&f.directory, &target, f.platform).is_some());
    assert!(!f.focused(target.as_str()));
    f.enqueue(target.as_str());
    assert_eq!(f.pending(), [target.as_str()]);
    f.focus();
    assert_eq!(f.pending().len(), 0);
    assert!(f.focused(target.as_str()));
    f.returning();
    assert!(!f.focused(target.as_str()));
    f.enqueue(target.as_str());
    drop(owner);
    assert!(path.exists());
    assert!(registration(&f.directory, &target, f.platform).is_none());
    f.focus();
    assert_eq!(f.pending().len(), 0);
    fs::remove_file(lease_path).unwrap();
    assert!(registration(&f.directory, &target, f.platform).is_none());
}

#[test]
fn native_lease_process_exit_invalidates_route() {
    use std::{
        io::{BufRead, BufReader},
        process::{Child, Command, Stdio},
    };
    use wrix_notify::route::registration;
    struct Owner(Child);
    impl Drop for Owner {
        fn drop(&mut self) {
            if self.0.try_wait().unwrap().is_none() {
                self.0.kill().unwrap();
            }
            self.0.wait().unwrap();
        }
    }
    let f = Fixture::linux();
    let target = Fixture::target("native-exited");
    let path = f.register(target.as_str(), 1);
    edit_json(&path, |value| {
        value["leased"] = json!(true);
    });
    let lease = path.with_extension("lease");
    let mut owner = Owner(Command::new("python3").args(["-c",
        "import fcntl,sys,time; f=open(sys.argv[1], 'wb'); fcntl.flock(f, fcntl.LOCK_EX); print('ready', flush=True); time.sleep(60)"])
        .arg(lease).stdout(Stdio::piped()).spawn().unwrap());
    let mut ready = String::new();
    BufReader::new(owner.0.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready.trim(), "ready");
    assert!(registration(&f.directory, &target, f.platform).is_some());
    f.enqueue(target.as_str());
    drop(owner);
    assert!(registration(&f.directory, &target, f.platform).is_none());
    f.focus();
    assert_eq!(f.pending().len(), 0);
}

#[test]
fn darwin_native_stable_pane_is_validated_without_mutable_session_names() {
    let f = Fixture::new(Platform::Darwin);
    let path = f.mac("native", "Origin");
    edit_json(&path, |value| {
        value["tmux_target"] = json!("%0");
        value["tmux_socket"] = json!("/socket");
    });
    f.response(
        &[
            "-S",
            "/socket",
            "display-message",
            "-p",
            "-t",
            "%0",
            "#{pane_id} #{pane_dead}",
        ],
        "%0 0",
    );
    f.response(
        &[
            "-S",
            "/socket",
            "display-message",
            "-p",
            "-t",
            "%0",
            "#{window_active} #{pane_active} #{?session_attached,1,0}",
        ],
        "1 1 1",
    );
    assert!(f.focused("native"));
    f.response(
        &[
            "-S",
            "/socket",
            "display-message",
            "-p",
            "-t",
            "%0",
            "#{pane_id} #{pane_dead}",
        ],
        "%0 1",
    );
    assert!(!f.focused("native"));
}
