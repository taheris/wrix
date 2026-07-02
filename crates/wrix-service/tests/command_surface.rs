use std::process::ExitCode;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[test]
fn service_surface_is_reached_through_wrix_root() -> TestResult {
    let service = run_wrix(&["service", "--help"])?;
    assert_eq!(service.code, ExitCode::SUCCESS);
    assert!(service.stderr.is_empty());
    assert!(service.stdout.contains("Usage: wrix service <command>"));
    assert!(
        service
            .stdout
            .contains("dolt <status|socket|port|host|attach|gc|wait>")
    );
    assert!(
        service
            .stdout
            .contains("cache <status|publish|warm|prune|rotate-key>")
    );

    let beads = run_wrix(&["beads", "--help"])?;
    assert_eq!(beads.code, ExitCode::SUCCESS);
    assert!(beads.stderr.is_empty());
    assert!(beads.stdout.contains("Usage: wrix beads <command>"));
    assert!(beads.stdout.contains("push"));

    for legacy in ["beads-dolt", "beads-push", "wrix-svc", "example-beads"] {
        let rejected = run_wrix(&[legacy, "--help"])?;
        assert_eq!(rejected.code, ExitCode::FAILURE);
        assert!(rejected.stdout.is_empty());
        assert!(rejected.stderr.contains("unknown command:"));
        assert!(!rejected.stderr.contains("Usage: wrix service"));
    }

    Ok(())
}

struct RunResult {
    code: ExitCode,
    stdout: String,
    stderr: String,
}

fn run_wrix(args: &[&str]) -> TestResult<RunResult> {
    let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = wrix_cli::command::run(&args, &mut stdout, &mut stderr)?;
    Ok(RunResult {
        code,
        stdout: String::from_utf8(stdout)?,
        stderr: String::from_utf8(stderr)?,
    })
}
