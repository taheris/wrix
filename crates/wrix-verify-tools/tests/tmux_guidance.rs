//! Execute the shipped guide through independent native shell calls.
#![cfg(test)]

use std::{
    env, fs,
    io::{self, Write},
    path::PathBuf,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

fn blocks(guide: &str) -> Vec<&str> {
    guide
        .split("```bash\n")
        .skip(1)
        .map(|part| {
            part.split_once("\n```")
                .expect("closed Bash documentation fence")
                .0
        })
        .collect()
}

struct Targets {
    directory: PathBuf,
    socket: String,
    pane: String,
}

struct Workflow {
    workspace: tempfile::TempDir,
    targets: Option<Targets>,
}

impl Workflow {
    fn new() -> Self {
        Self {
            workspace: tempfile::tempdir().unwrap(),
            targets: None,
        }
    }

    fn call(&self, command: &str) -> Output {
        let mut process = Command::new("bash");
        process
            .args(["-c", &format!("set -euo pipefail\n{command}")])
            .current_dir(self.workspace.path())
            .env("WRIX_TEST_TMUX_PROBE", env::current_exe().unwrap());
        if let Some(targets) = &self.targets {
            process
                .env("workflow_dir", &targets.directory)
                .env("socket", &targets.socket)
                .env("pane", &targets.pane);
        }
        let mut child = process
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("documented shell call exceeded ten seconds: {command}");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        child.wait_with_output().unwrap()
    }

    fn run(&self, command: &str) -> String {
        let result = self.call(command);
        assert!(result.status.success(), "{command}\n{result:?}");
        String::from_utf8(result.stdout).unwrap()
    }

    fn capture(&self) -> String {
        self.run("tmux -S \"$socket\" capture-pane -p -t \"$pane\" -S -1000")
    }

    fn until(&self, mut predicate: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if predicate(self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("documented workflow did not become ready within five seconds\n{}\n{}",
            self.run("tmux -S \"$socket\" display-message -p -t \"$pane\" \"command=#{pane_current_command} dead=#{pane_dead} exit=#{pane_dead_status}\""),
            self.capture());
    }
}

impl Drop for Workflow {
    fn drop(&mut self) {
        if let Some(targets) = &self.targets {
            let result = self.call("tmux -S \"$socket\" kill-server");
            assert!(
                result.status.success(),
                "private workflow server cleanup failed: {result:?}"
            );
            fs::remove_dir_all(&targets.directory).unwrap();
        }
    }
}

#[test]
fn bash_fences_extract_only_the_documented_commands() {
    assert_eq!(
        blocks(
            "before\n```bash\nprintf first\n```\n```text\nignored\n```\n```bash\nprintf second\n```\nafter"
        ),
        ["printf first", "printf second"]
    );
}

#[test]
#[should_panic(expected = "closed Bash documentation fence")]
fn unterminated_documented_command_is_not_accepted() {
    blocks("```bash\nprintf incomplete");
}

#[test]
#[ignore = "child workload for the real tmux Ctrl-C scenario, not standalone acceptance"]
fn interrupt_probe() {
    // The leading newline separates this marker from libtest's progress prefix.
    println!("\ninterrupt-ready");
    io::stdout().flush().unwrap();
    std::thread::sleep(Duration::from_secs(100));
}

#[test]
#[ignore = "requires the actual Nix-packaged tmux, curl and documented HTTP server"]
fn documented_commands_support_native_debugging() {
    let guide = fs::read_to_string(env::var_os("WRIX_TEST_TMUX_GUIDE").unwrap()).unwrap();
    let blocks = blocks(&guide);
    assert_eq!(blocks.len(), 6);
    let mut workflow = Workflow::new();
    let created = workflow.run(&format!(
        "{}\nprintf 'TARGETS=%s %s %s\\n' \"$workflow_dir\" \"$socket\" \"$pane\"",
        blocks[0]
    ));
    let targets: Vec<_> = created
        .split_once("TARGETS=")
        .unwrap()
        .1
        .split_whitespace()
        .collect();
    assert_eq!(targets.len(), 3);
    workflow.targets = Some(Targets {
        directory: targets[0].into(),
        socket: targets[1].into(),
        pane: targets[2].into(),
    });
    workflow.run(blocks[1]);
    workflow.until(|workflow| {
        workflow
            .call("curl --fail --silent --max-time 1 http://127.0.0.1:8765/")
            .status
            .success()
    });
    assert!(workflow.run(blocks[2]).contains("dead=0"));
    workflow.until(|workflow| workflow.capture().contains("GET /"));
    workflow.run(blocks[3]);
    workflow.until(|workflow| workflow.run("tmux -S \"$socket\" display-message -p -t \"$pane\" \"#{pane_dead}:#{pane_dead_status}\"").trim() == "1:7");
    assert!(workflow.capture().contains("fast failure"));
    let literal: Vec<_> = blocks[4].lines().collect();
    workflow.run(literal[0]);
    workflow.until(|workflow| {
        let output = workflow.run("tmux -S \"$socket\" capture-pane -p -t \"$pane\"");
        let prompt = output.trim_end();
        prompt.ends_with('$') || prompt.ends_with('#')
    });
    workflow.run(literal[1]);
    assert!(
        !workflow
            .capture()
            .lines()
            .any(|line| line == "literal Enter C-c")
    );
    workflow.run(literal[2]);
    workflow.until(|workflow| {
        workflow
            .capture()
            .lines()
            .any(|line| line == "literal Enter C-c")
    });
    // Native dev-only workload replaces the former owned inline Python probe.
    workflow.run(r#"tmux -S "$socket" send-keys -t "$pane" -l -- '"$WRIX_TEST_TMUX_PROBE" --ignored --exact interrupt_probe --nocapture'; tmux -S "$socket" send-keys -t "$pane" Enter"#);
    workflow.until(|workflow| {
        workflow
            .capture()
            .lines()
            .any(|line| line == "interrupt-ready")
    });
    workflow.run(literal[3]);
    workflow.run(r#"tmux -S "$socket" send-keys -t "$pane" -l -- 'printf "interrupt-status=%s\n" "$?"'; tmux -S "$socket" send-keys -t "$pane" Enter"#);
    workflow.until(|workflow| {
        workflow
            .capture()
            .lines()
            .any(|line| line == "interrupt-status=130")
    });
    workflow.run("tmux -S \"$socket\" new-session -d -s unrelated \"sleep 100\"");
    workflow.run(blocks[5]);
    assert!(
        !workflow
            .call("tmux -S \"$socket\" has-session -t \"=debug\"")
            .status
            .success()
    );
    workflow.run("tmux -S \"$socket\" has-session -t \"=unrelated\"");
}
