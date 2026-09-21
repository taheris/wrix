pub(super) fn command(first: &str, second: &str) -> String {
    format!(
        r#"set -euo pipefail
({first}) & first=$!
({second}) & second=$!
stop_children() {{
  trap - EXIT TERM INT
  local pid
  for pid in "$first" "$second"; do
    if kill -0 "$pid" 2>/dev/null; then
      if ! kill "$pid"; then
        echo "Warning: service child $pid exited during shutdown" >&2
      fi
    fi
    # Child exit statuses are expected after wait -n or intentional termination.
    wait "$pid" 2>/dev/null || :
  done
}}
trap stop_children EXIT
trap 'exit 143' TERM
trap 'exit 130' INT
status=0
wait -n "$first" "$second" || status=$?
if [[ "$status" -eq 0 ]]; then status=1; fi
exit "$status""#
    )
}

#[cfg(test)]
mod test {
    use std::{fs, process::Command};

    #[test]
    fn either_service_exit_stops_its_sibling_and_fails_the_container() {
        for (first_exits, exit_code) in [(true, 47), (false, 23), (true, 0)] {
            let fixture = tempfile::tempdir().unwrap();
            let pid_file = fixture.path().join("pid");
            let sleeper = format!("echo $BASHPID > '{}'; exec sleep 60", pid_file.display());
            let quitter = format!(
                "while [[ ! -f '{}' ]]; do sleep 0.01; done; exit {exit_code}",
                pid_file.display()
            );
            let (first, second) = if first_exits {
                (&quitter, &sleeper)
            } else {
                (&sleeper, &quitter)
            };
            let output = Command::new("bash")
                .arg("-c")
                .arg(super::command(first, second))
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(if exit_code == 0 { 1 } else { exit_code })
            );
            let pid = fs::read_to_string(pid_file).unwrap();
            assert!(
                !Command::new("kill")
                    .args(["-0", pid.trim()])
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
    }
}
