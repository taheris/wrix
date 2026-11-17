#![cfg(test)]

#[path = "execution_lifecycle/context/mod.rs"]
mod context;
#[path = "execution_lifecycle/launch/mod.rs"]
mod launch;
#[path = "execution_fixtures/process/mod.rs"]
mod process;

use context::{Agent, Mode};
use launch::with_live;
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    process::Command,
};

fn count_records(workspace: &Path) -> usize {
    fs::read_dir(workspace.join(".wrix/log")).unwrap().count()
}

#[test]
#[ignore = "requires actual packaged Wrix and a live host container runtime"]
fn normal_runtime_exits_complete_the_original_record() {
    with_live(|harness| {
        for mode in [Mode::Run, Mode::Spawn] {
            for status in [0, 37] {
                let workspace = harness.workspace(&format!("normal-{}-{status}", mode.name()));
                let index = harness.start(mode, &workspace, Agent::Direct, status, None, true);
                harness.probe_ready(index);
                let launch = &harness.launches[index];
                assert_eq!(fs::read(&launch.record).unwrap(), launch.initial);
                context::checked(harness.runtime(&["inspect".as_ref(), launch.name.as_ref()]));
                fs::write(workspace.join(format!("exit-{}", launch.member)), "").unwrap();
                let (value, _) = harness.finish(index, status);
                assert_eq!(value["agent_session_dir"], Value::Null);
                assert_eq!(count_records(&workspace), 1);
            }
        }
    });
}

#[test]
#[ignore = "requires actual packaged Wrix and a live host container runtime"]
fn observed_container_startup_failures_complete_the_original_record() {
    with_live(|harness| {
        let mismatch = harness.mismatch_profile();
        for mode in [Mode::Run, Mode::Spawn] {
            let workspace = harness.workspace(&format!("failure-{}", mode.name()));
            let index = harness.start(mode, &workspace, Agent::Claude, 0, Some(&mismatch), true);
            let (_, log) = harness.finish(index, 1);
            assert!(
                log.contains("this image was built for agent=direct"),
                "{log}"
            );
            assert!(
                !workspace
                    .join(format!("ready-{}", harness.launches[index].member))
                    .exists()
            );
            assert_eq!(count_records(&workspace), 1);
        }
    });
}

#[test]
#[ignore = "requires actual packaged Wrix and a live host container runtime"]
fn killing_host_launcher_preserves_unknown_completion_after_container_cleanup() {
    with_live(|harness| {
        for mode in [Mode::Run, Mode::Spawn] {
            let workspace = harness.workspace(&format!("killed-{}", mode.name()));
            let index = harness.start(mode, &workspace, Agent::Direct, 0, None, true);
            harness.probe_ready(index);
            let launch = &harness.launches[index];
            context::checked(harness.runtime(&["inspect".as_ref(), launch.name.as_ref()]));
            let host: i32 = fs::read_to_string(launch.control.join("host-pid"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            rustix::process::kill_process(
                rustix::process::Pid::from_raw(host).unwrap(),
                rustix::process::Signal::KILL,
            )
            .unwrap();
            assert_eq!(fs::read(&launch.record).unwrap(), launch.initial);
            harness.remove(index).unwrap();
            harness.launches[index]
                .process
                .take()
                .unwrap()
                .stop()
                .unwrap();
            let launch = &harness.launches[index];
            assert_eq!(fs::read(&launch.record).unwrap(), launch.initial);
            assert_eq!(count_records(&workspace), 1);
        }
    });
}

#[test]
#[ignore = "requires actual packaged Wrix and a live host container runtime"]
fn concurrent_packaged_agents_do_not_adopt_shared_transcript_conversations() {
    with_live(|harness| {
        for mode in [Mode::Run, Mode::Spawn] {
            let workspace = harness.workspace(&format!("shared-{}", mode.name()));
            fs::create_dir(workspace.join(".claude")).unwrap();
            fs::create_dir_all(workspace.join(".pi/agent/sessions")).unwrap();
            let history = workspace.join(".claude/history.jsonl");
            fs::write(
                &history,
                "{\"sessionId\":\"unrelated-claude-conversation\"}\n",
            )
            .unwrap();
            fs::write(
                workspace.join(".pi/agent/sessions/unrelated.jsonl"),
                "{\"type\":\"session\",\"id\":\"unrelated-pi-conversation\"}\n",
            )
            .unwrap();
            let first = harness.start(mode, &workspace, Agent::Claude, 0, None, false);
            let second = harness.start(mode, &workspace, Agent::Pi, 0, None, false);
            writeln!(
                OpenOptions::new().append(true).open(&history).unwrap(),
                "{{\"sessionId\":\"concurrent-unrelated-conversation\"}}"
            )
            .unwrap();
            for index in [first, second] {
                fs::write(harness.launches[index].control.join("release"), "").unwrap();
            }
            for (index, session) in [
                (first, "/workspace/.claude"),
                (second, "/workspace/.pi/agent/sessions"),
            ] {
                let (value, _) = harness.finish(index, 0);
                assert_eq!(value["agent_session_id"], Value::Null);
                assert_eq!(value["agent_session_dir"], session);
                assert!(
                    workspace
                        .join(session.strip_prefix("/workspace/").unwrap())
                        .is_dir()
                );
                assert!(
                    !fs::read_to_string(&harness.launches[index].record)
                        .unwrap()
                        .contains("unrelated")
                );
                for retired in ["claude_session_id", "wrix_session_id", "claude_session_dir"] {
                    assert!(value.get(retired).is_none());
                }
            }
            assert_ne!(
                harness.launches[first].record,
                harness.launches[second].record
            );
            assert_eq!(count_records(&workspace), 2);
        }
    });
}

#[test]
fn shell_arguments_round_trip_quotes_empty_strings_and_newlines() {
    let args = [
        "space argument",
        "",
        "quote'and\"double",
        "line\nbreak",
        "$(exit 37)",
    ]
    .map(std::ffi::OsString::from);
    let rendered = launch::shell_join(&args);
    let output = process::run(
        Command::new("bash").args(["-c", &format!("printf '%s\\0' {rendered}")]),
        b"",
    );
    assert!(output.status.success());
    let expected: Vec<u8> = args
        .iter()
        .flat_map(|arg| arg.as_encoded_bytes().iter().copied().chain([0]))
        .collect();
    assert_eq!(output.stdout, expected);
}

#[test]
fn completed_record_comparison_rejects_changed_identity_or_invented_completion() {
    let initial = json!({"state":"incomplete","execution_id":"original","mode":"spawn","agent_kind":"direct","focus_target":"focus:fixture","bead_id":null,"timestamp_end":null,"duration_seconds":null,"exit_code":null,"signal":null});
    let mut complete = initial.clone();
    complete["state"] = json!("completed");
    complete["timestamp_end"] = json!("end");
    complete["duration_seconds"] = json!(0);
    complete["exit_code"] = json!(37);
    launch::assert_completed(&initial, &complete, 37);
    for (key, incorrect) in [
        ("execution_id", json!("adopted")),
        ("state", json!("incomplete")),
        ("duration_seconds", json!(-1)),
        ("exit_code", json!(0)),
        ("signal", json!("SIGKILL")),
        ("timestamp_end", Value::Null),
    ] {
        let mut invalid = complete.clone();
        invalid[key] = incorrect;
        assert!(
            std::panic::catch_unwind(|| launch::assert_completed(&initial, &invalid, 37)).is_err(),
            "accepted invalid {key}"
        );
    }
    for key in ["signal", "bead_id"] {
        let mut invalid = complete.clone();
        invalid.as_object_mut().unwrap().remove(key);
        assert!(
            std::panic::catch_unwind(|| launch::assert_completed(&initial, &invalid, 37)).is_err(),
            "accepted missing {key}"
        );
    }
}
