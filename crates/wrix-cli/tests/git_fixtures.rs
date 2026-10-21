mod common;

use std::process::Command;

use common::{
    TestResult, setup_repo, write_capturing_git, write_online_success_git, write_tracing_git,
};

#[test]
fn git_fixtures_work_without_bash_on_child_path() -> TestResult {
    let repo = setup_repo("git-fixture-interpreter")?;
    let root = tempfile::tempdir()?;
    let capture = root.path().join("capture");
    let mode = root.path().join("mode");
    let wrappers = [
        write_online_success_git(&root.path().join("online"))?,
        write_tracing_git(&root.path().join("tracing"), &capture)?,
        write_capturing_git(&root.path().join("capturing"), &mode, &capture)?,
    ];
    for wrapper in wrappers {
        let output = Command::new(wrapper.join("git"))
            .env_clear()
            .env("PATH", root.path().join("absent"))
            .current_dir(repo.path())
            .args(["rev-parse", "--path-format=absolute", "--show-toplevel"])
            .output()?;
        assert!(
            output.status.success(),
            "{}: {}",
            wrapper.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout)?.trim(),
            repo.path()
                .canonicalize()?
                .to_str()
                .ok_or("non-UTF-8 path")?
        );
    }
    Ok(())
}
