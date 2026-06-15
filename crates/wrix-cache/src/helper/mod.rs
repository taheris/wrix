use std::{
    env,
    io::{self, Write},
    process::ExitCode,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Helper {
    Hook,
    Publish,
    Serve,
}

impl Helper {
    const fn binary_name(self) -> &'static str {
        match self {
            Self::Hook => "wrix-cache-hook",
            Self::Publish => "wrix-cache-publish",
            Self::Serve => "wrix-cache-serve",
        }
    }

    const fn purpose(self) -> &'static str {
        match self {
            Self::Hook => "Run the project cache post-build hook.",
            Self::Publish => "Publish project cache paths.",
            Self::Serve => "Serve the project cache over HTTP.",
        }
    }
}

pub fn main(helper: Helper) -> ExitCode {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    match run(helper, &args, &mut stdout, &mut stderr) {
        Ok(code) => code,
        Err(error) => {
            if writeln!(stderr, "{}: {error}", helper.binary_name()).is_err() {
                return ExitCode::FAILURE;
            }
            ExitCode::FAILURE
        }
    }
}

fn run(
    helper: Helper,
    args: &[String],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<ExitCode> {
    if args.is_empty()
        || args
            .first()
            .is_some_and(|arg| arg == "--help" || arg == "-h")
    {
        write_help(helper, stdout)?;
        return Ok(ExitCode::SUCCESS);
    }
    writeln!(
        stderr,
        "{} accepts no public arguments yet; pass --help for usage.",
        helper.binary_name()
    )?;
    Ok(ExitCode::FAILURE)
}

fn write_help(helper: Helper, stdout: &mut impl Write) -> io::Result<()> {
    writeln!(
        stdout,
        "{}\n\nUsage: {} [--help]",
        helper.purpose(),
        helper.binary_name()
    )
}
