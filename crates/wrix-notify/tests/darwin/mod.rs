//! Portable, mocked OmniWM/ps/lsof tests: these do not validate macOS AX or workspace behavior.
use super::{Fixture, edit_json};
use serde_json::json;
use wrix_notify::{
    attention,
    navigation::Outcome,
    route::{Platform, Route},
};

const WINDOWS: &[&str] = &["query", "windows", "--fields", "id,pid", "--json"];
const FOCUSED: &[&str] = &["query", "focused-window", "--json"];
const CLIENTS: &[&str] = &[
    "-S",
    "/socket",
    "list-clients",
    "-F",
    wrix_notify::navigation::CLIENT_FORMAT,
];

fn fixture() -> Fixture {
    Fixture::new(Platform::Darwin)
}

fn tmux_route(f: &Fixture) -> Route {
    f.tmux_ready();
    f.response(&["-p", "11", "-o", "ppid="], "10");
    f.response(&["-p", "10", "-o", "ppid="], "1");
    f.response(&["-p", "11", "-o", "tty="], "pts/1");
    Route::parse(
        &json!({"omniwm_route":{"window":"ow_1","terminal_pid":10,
        "client":{"name":"client","pid":11,"tty":"/dev/pts/1","session":"$0"}},
        "tmux_socket":"/socket","tmux_target":"%0","tmux_window":"@0"}),
        Platform::Darwin,
    )
    .unwrap()
}

#[test]
fn nonzero_cli_exit_cannot_be_mistaken_for_success_and_keeps_json_stale_codes() {
    for (response, retry) in [
        (json!({"ok":true,"kind":"window","status":"executed"}), true),
        (
            json!({"ok":false,"kind":"window","status":"error","code":"stale_window_id"}),
            false,
        ),
        (
            json!({"ok":false,"kind":"window","status":"error","code":"unauthorized"}),
            true,
        ),
    ] {
        let f = fixture();
        f.mac("mac", "ow_1");
        f.enqueue("mac");
        f.state.lock().unwrap().status_responses.insert(
            ["window", "navigate", "ow_1", "--json"]
                .into_iter()
                .map(String::from)
                .collect(),
            response.to_string(),
        );
        assert_eq!(
            attention::focus(&f.directory, f.platform, &f.navigator).is_err(),
            retry
        );
        assert_eq!(f.pending().len(), usize::from(retry));
        assert!(f.saved().is_none());
    }
}

#[test]
fn opaque_window_navigation_and_return_ignore_titles_and_restore_origin() {
    let f = fixture();
    let path = f.mac("mac", "ow_1");
    edit_json(&path, |value| {
        value["terminal_window_title"] = json!("duplicate; \"title\"");
    });
    f.enqueue("mac");
    f.focus();
    assert_eq!(f.focused_window(), "ow_1");
    assert_eq!(f.pending().len(), 0);
    assert_eq!(f.saved().unwrap()["omniwm_route"]["window"], "ow_99");
    assert!(
        f.calls()
            .iter()
            .any(|(program, args)| program == "omniwmctl"
                && args == &["window", "navigate", "ow_1", "--json"])
    );
    assert!(!f.calls().iter().any(|(program, _)| program == "osascript"));
    f.returning();
    assert_eq!(f.focused_window(), "ow_99");
    assert!(f.saved().is_none());
}

#[test]
fn latest_success_replaces_origin_even_within_the_same_workspace() {
    let f = fixture();
    f.mac("one", "ow_1");
    f.mac("two", "ow_2");
    f.enqueue("one");
    f.focus();
    f.enqueue("two");
    f.focus();
    f.returning();
    assert_eq!(f.focused_window(), "ow_1");
}

#[test]
fn empty_attention_leaves_saved_origin_unchanged() {
    let f = fixture();
    f.mac("mac", "ow_1");
    f.enqueue("mac");
    f.focus();
    let saved = f.saved();
    f.focus();
    assert_eq!(f.saved(), saved);
}

#[test]
fn legacy_title_routes_are_not_navigable() {
    let f = fixture();
    let path = f.mac("mac", "ow_1");
    edit_json(&path, |value| {
        value.as_object_mut().unwrap().remove("omniwm_route");
        value["terminal_app"] = json!("Alacritty");
        value["terminal_window_title"] = json!("unique title");
    });
    f.enqueue("mac");
    assert!(!f.focused("mac"));
    assert_eq!(f.pending().len(), 0);
    assert_eq!(f.calls().len(), 0);
}

#[test]
fn closed_reused_or_previous_ipc_session_id_is_discarded_without_focus() {
    for windows in [
        json!([]),
        json!([{"id":"ow_1","pid":500}]),
        json!([{"id":"ow_newsession","pid":10}]),
    ] {
        let f = fixture();
        f.mac("mac", "ow_1");
        f.enqueue("mac");
        f.windows(windows);
        f.focus();
        assert_eq!(f.pending().len(), 0);
        assert!(
            !f.calls()
                .iter()
                .any(|(_, args)| args.first().is_some_and(|arg| arg == "window"))
        );
    }
}

#[test]
fn only_the_exact_focused_identity_suppresses() {
    let f = fixture();
    f.mac("mac", "ow_1");
    assert!(!f.focused("mac"));
    f.windows(json!([{"id":"ow_1","pid":10,"is_focused":true}]));
    assert!(f.focused("mac"));
    f.response(
        FOCUSED,
        &super::common::omni_response("focused-window", &json!({"window":null})).to_string(),
    );
    assert!(!f.focused("mac"));
}

#[test]
fn failed_or_malformed_queries_retain_attention_and_saved_origin() {
    for response in [
        "not JSON",
        "{}",
        r#"{"ok":false,"code":"unauthorized"}"#,
        r#"{"ok":true,"status":"success","result":{"kind":"windows","payload":{}}}"#,
    ] {
        let f = fixture();
        f.mac("mac", "ow_1");
        f.enqueue("mac");
        f.focus();
        let saved = f.saved();
        f.enqueue("mac");
        f.response(WINDOWS, response);
        assert!(matches!(
            attention::focus(&f.directory, f.platform, &f.navigator),
            Err(attention::Error::Navigation)
        ));
        assert_eq!(f.pending(), ["mac"]);
        assert_eq!(f.saved(), saved);
        assert!(!f.focused("mac"));
    }
}

#[test]
fn navigate_failure_is_visible_and_retryable_but_stale_race_is_consumed() {
    for (code, retry) in [
        ("ignored_overview", true),
        ("protocol_mismatch", true),
        ("stale_window_id", false),
        ("not_found", false),
    ] {
        let f = fixture();
        f.mac("mac", "ow_1");
        f.enqueue("mac");
        f.response(
            &["window", "navigate", "ow_1", "--json"],
            &json!({"ok":false,"status":"error","code":code}).to_string(),
        );
        let result = attention::focus(&f.directory, f.platform, &f.navigator);
        assert_eq!(result.is_err(), retry);
        assert_eq!(f.pending().len(), usize::from(retry));
        assert!(f.saved().is_none());
    }
}

#[test]
fn transport_timeout_retains_target_and_return_state() {
    let f = fixture();
    f.mac("mac", "ow_1");
    f.enqueue("mac");
    f.focus();
    let saved = f.saved();
    f.state
        .lock()
        .unwrap()
        .failures
        .push(WINDOWS.iter().map(|s| (*s).into()).collect());
    assert!(attention::return_to_origin(&f.directory, f.platform, &f.navigator).is_err());
    assert_eq!(f.saved(), saved);
}

#[test]
fn stale_return_does_not_consume_attention_or_save_another_origin() {
    let f = fixture();
    f.mac("mac", "ow_1");
    f.enqueue("mac");
    f.focus();
    f.enqueue("mac");
    f.windows(json!([{"id":"ow_1","pid":10,"is_focused":true}]));
    f.returning();
    assert!(f.saved().is_none());
    assert_eq!(f.pending(), ["mac"]);
    assert_eq!(f.focused_window(), "ow_1");
}

#[test]
fn successful_jump_without_resolvable_origin_clears_previous_return() {
    let f = fixture();
    f.mac("mac", "ow_1");
    f.enqueue("mac");
    f.focus();
    f.response(
        FOCUSED,
        &super::common::omni_response("focused-window", &json!({"window":null})).to_string(),
    );
    let origin = f.navigator.capture_position(&f.directory, f.platform);
    assert!(origin.is_none());
    f.state
        .lock()
        .unwrap()
        .responses
        .remove(&FOCUSED.iter().map(|s| (*s).into()).collect::<Vec<String>>());
    f.response(&["-n", "-P", "-a", "-U", "-c", "tmux", "-Fn"], "");
    f.windows(json!([{"id":"ow_1","pid":10,"is_focused":false},{"id":"ow_2","pid":20,"is_focused":false}]));
    f.enqueue("mac");
    f.focus();
    assert!(f.saved().is_none());
}

#[test]
fn launch_capture_requires_focused_window_to_belong_to_caller() {
    let f = fixture();
    f.windows(json!([{"id":"ow_1","pid":10,"is_focused":true}]));
    f.response(&["-p", "12", "-o", "ppid="], "10");
    f.response(&["-p", "10", "-o", "ppid="], "1");
    let route = f.navigator.capture_omni_launch(None, None, 12).unwrap();
    assert_eq!(Route::parse(&route.saved(), f.platform).unwrap(), route);
    f.response(&["-p", "12", "-o", "ppid="], "1");
    assert!(f.navigator.capture_omni_launch(None, None, 12).is_none());
}

#[test]
fn tmux_launch_captures_stable_pane_and_proven_client_not_server_ancestry() {
    let f = fixture();
    let Route::Darwin {
        tmux: Some(tmux), ..
    } = tmux_route(&f)
    else {
        panic!("tmux required");
    };
    f.windows(json!([{"id":"ow_1","pid":10,"is_focused":true}]));
    assert!(
        f.navigator
            .capture_omni_launch(Some(&tmux.pane), Some(&tmux.socket), 999)
            .is_some()
    );
    f.response(
        CLIENTS,
        "client|11|/dev/pts/1|$0|@0|%0\nother|12|/dev/pts/2|$0|@0|%0",
    );
    assert!(
        f.navigator
            .capture_omni_launch(Some(&tmux.pane), Some(&tmux.socket), 999)
            .is_none()
    );
}

#[test]
fn tmux_stale_client_dead_moved_linked_or_renamed_index_targets_are_safe() {
    for (args, response) in [
        (CLIENTS, "client|11|/dev/pts/1|$1|@0|%0"),
        (CLIENTS, "client|12|/dev/pts/1|$0|@0|%0"),
        (&["-p", "11", "-o", "tty="][..], "pts/2"),
        (&["-p", "11", "-o", "ppid="][..], "1"),
        (
            &[
                "-S",
                "/socket",
                "list-panes",
                "-a",
                "-F",
                "#{pane_id}|#{session_id}",
            ][..],
            "%0|$0\n%0|$1",
        ),
        (
            &[
                "-S",
                "/socket",
                "display-message",
                "-p",
                "-t",
                "%0",
                "#{pane_id} #{pane_dead} #{session_id}",
            ][..],
            "%0 1 $0",
        ),
    ] {
        let f = fixture();
        let route = tmux_route(&f);
        f.response(args, response);
        assert_eq!(f.navigator.navigate(&route, false), Outcome::Stale);
        assert!(!f.calls().iter().any(|(_, args)| {
            args.iter()
                .any(|arg| matches!(arg.as_str(), "select-pane" | "select-window" | "navigate"))
        }));
    }
}

#[test]
fn tmux_command_failure_retains_return_for_retry() {
    let f = fixture();
    let route = tmux_route(&f);
    f.save(&route);
    f.state.lock().unwrap().failures.push(vec![
        "-S".into(),
        "/socket".into(),
        "select-pane".into(),
        "-t".into(),
        "%0".into(),
    ]);
    assert!(attention::return_to_origin(&f.directory, f.platform, &f.navigator).is_err());
    assert!(f.saved().is_some());
}

#[test]
fn tmux_return_restores_only_the_identified_client_session_window_and_pane() {
    let f = fixture();
    let route = tmux_route(&f);
    f.save(&route);
    f.response(CLIENTS, "client|11|/dev/pts/1|$1|@4|%7");
    f.returning();
    assert!(f.saved().is_none());
    let calls = f.calls();
    for command in ["switch-client", "select-window", "select-pane"] {
        assert_eq!(calls.iter().filter(|(program, args)| program == "tmux" && args.iter().any(|arg| arg == command)).count(), 1);
    }
    assert_eq!(f.focused_window(), "ow_1");
}

#[test]
fn return_capture_discovers_unregistered_custom_tmux_server_with_lsof() {
    let f = fixture();
    let mut route = tmux_route(&f).saved();
    let path = f.root.path().join("custom.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let socket = path.to_str().unwrap();
    route["tmux_socket"] = json!(socket);
    let responses = f.state.lock().unwrap().responses.clone();
    for (mut args, response) in responses {
        if args.get(1).is_some_and(|arg| arg == "/socket") {
            args[1] = socket.into();
            f.state.lock().unwrap().responses.insert(args, response);
        }
    }
    f.response(
        &["-n", "-P", "-a", "-U", "-c", "tmux", "-Fn"],
        &format!("p100\nn{socket}\n"),
    );
    f.windows(json!([{"id":"ow_1","pid":10,"is_focused":true}]));
    let captured = f
        .navigator
        .capture_position(&f.directory, f.platform)
        .unwrap();
    assert_eq!(captured.saved(), route);
}

#[test]
fn tmux_suppression_requires_active_attached_pane() {
    let f = fixture();
    let route = tmux_route(&f);
    let path = f.mac("mac", "ow_1");
    edit_json(&path, |value| {
        let mut saved = route.saved();
        saved["focus_target"] = json!("mac");
        *value = saved;
    });
    f.windows(json!([{"id":"ow_1","pid":10,"is_focused":true}]));
    let args = &[
        "-S",
        "/socket",
        "display-message",
        "-p",
        "-t",
        "%0",
        "#{window_active} #{pane_active} #{?session_attached,1,0}",
    ];
    for state in ["0 1 1", "1 0 1", "1 1 0"] {
        f.response(args, state);
        assert!(!f.focused("mac"));
    }
    f.response(args, "1 1 1");
    assert!(f.focused("mac"));
}
