use super::*;

#[test]
fn observed_completion_keeps_leader_waitable_until_group_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("worker.sh");
    std::fs::write(&script, "exit 37\n").unwrap();
    let mut worker = start(root.path(), Path::new("bash"), script.to_str().unwrap(), 0).unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    while !worker.exited().unwrap() {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        rustix::process::waitid(
            WaitId::Pid(worker.group),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        )
        .unwrap()
        .is_some()
    );
    assert!(!worker.stopped);
    assert_eq!(worker.finish().unwrap().code(), Some(37));
    assert!(worker.stopped);
    assert!(matches!(
        rustix::process::waitid(
            WaitId::Pid(worker.group),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG,
        ),
        Err(rustix::io::Errno::CHILD)
    ));
}

#[test]
fn zombie_exception_requires_every_observed_group_member_to_be_a_zombie() {
    for states in [
        b"".as_slice(),
        b" \n",
        b"S\n",
        b"Z\nS+\n",
        b"unknown\n",
        b"Z\nR\n",
    ] {
        assert!(!only_zombies(states), "accepted {states:?}");
    }
    assert!(only_zombies(b"Z\n"));
    assert!(only_zombies(b"  Z+\n ZN \n"));
}
