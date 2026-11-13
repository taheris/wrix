//! Revision-bound interruption evidence never substitutes for native execution.
mod checkpoint;
pub mod entry;
mod supervisor;
mod witness;

use super::{cleanup, command, root};
use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// native interruption filesystem failed: {0}
    Io(#[from] io::Error),
    /// native interruption record failed: {0}
    Json(#[from] serde_json::Error),
    /// native interruption syscall failed: {0}
    System(#[from] rustix::io::Errno),
    /// {0}
    Cleanup(#[from] cleanup::Error),
    /// {0}
    Root(#[from] root::Error),
    /// {0}
    Command(#[from] command::Error),
    /// {0}
    Mount(#[from] super::mount::Error),
    /// {0}
    Process(#[from] super::process::Error),
    /// native interruption nonce generation failed: {0}
    Random(#[from] getrandom::Error),
    /// {0}
    Policy(&'static str),
    /// {0} exceeded its deadline
    Deadline(&'static str),
}

pub struct Options {
    pub tools: Option<PathBuf>,
    pub revision: String,
    pub output: Option<PathBuf>,
    pub ready_timeout: Duration,
    pub stop_timeout: Duration,
    pub command: Vec<std::ffi::OsString>,
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), Error> {
    let mut file = tempfile::Builder::new().prefix(".record-").tempfile_in(
        path.parent()
            .ok_or(Error::Policy("record has no private parent"))?,
    )?;
    serde_json::to_writer_pretty(file.as_file_mut(), value)?;
    file.write_all(b"\n")?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

struct Cancellation {
    runtime: tokio::runtime::Runtime,
    term: tokio::signal::unix::Signal,
    interrupt: tokio::signal::unix::Signal,
}
impl Cancellation {
    fn new() -> Result<Self, Error> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (term, interrupt) = {
            let _entry = runtime.enter();
            (
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?,
            )
        };
        Ok(Self {
            runtime,
            term,
            interrupt,
        })
    }
    fn check(&mut self) -> Result<(), Error> {
        let signalled = self.runtime.block_on(async {
            tokio::select! { biased;
                _ = self.term.recv()=>true,
                _ = self.interrupt.recv()=>true,
                () = tokio::time::sleep(Duration::from_millis(1))=>false,
            }
        });
        if signalled {
            Err(Error::Policy(
                "harness interrupted; stopping only its owned invocation",
            ))
        } else {
            Ok(())
        }
    }
}

struct Progress {
    started: Instant,
}
impl Progress {
    fn say(&self, message: impl std::fmt::Display) -> Result<(), Error> {
        writeln!(
            io::stdout().lock(),
            "[{:6.1}s] {message}",
            self.started.elapsed().as_secs_f64()
        )?;
        Ok(())
    }
}

struct Inventory<'a> {
    subject: &'a cleanup::Subject,
    before: Option<&'a witness::Snapshot>,
    witness: &'a mut witness::Witness,
    output: &'a Path,
}
impl Inventory<'_> {
    fn verify(
        &mut self,
        result: &mut serde_json::Map<String, serde_json::Value>,
        progress: &Progress,
    ) -> Result<(), Error> {
        let mut record = serde_json::json!({"verified":false});
        let outcome = (|| {
            progress.say(
                "Independently checking host and retained original Podman namespace after teardown",
            )?;
            let host = self.output.join("host-after.mountinfo");
            fs::write(&host, fs::read("/proc/self/mountinfo")?)?;
            record["host_mountinfo"] = serde_json::to_value(host)?;
            let after = self
                .witness
                .snapshot(&self.output.join("namespace-after.json"), progress)?;
            let before = self.before.ok_or(Error::Policy(
                "no pre-interruption namespace identity to verify against",
            ))?;
            witness::verify_after(self.subject, before, &after)?;
            record["verified"] = true.into();
            Ok::<_, Error>(())
        })();
        if let Err(error) = &outcome {
            record["error"] = error.to_string().into();
        }
        result.insert("resource_inventory".into(), record);
        outcome
    }
}

pub fn run(options: &Options) -> Result<bool, Error> {
    use std::os::unix::fs::DirBuilderExt;
    let output = if let Some(path) = &options.output {
        let absolute = if path.is_absolute() {
            path.clone()
        } else {
            std::env::current_dir()?.join(path)
        };
        let parent = absolute
            .parent()
            .ok_or(Error::Policy("output has no parent"))?;
        let parent = fs::canonicalize(parent)?;
        let selected = parent.join(
            absolute
                .file_name()
                .ok_or(Error::Policy("output has no name"))?,
        );
        fs::DirBuilder::new().mode(0o700).create(&selected)?;
        selected
    } else {
        let parent = std::env::current_dir()?.join(".loom/scratch/linux-acceptance");
        fs::create_dir_all(&parent)?;
        tempfile::Builder::new()
            .prefix(&format!(
                "interruption-{}-",
                chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now())
                    .format("%Y%m%d-%H%M%S")
            ))
            .tempdir_in(parent)?
            .keep()
    };
    let registry = output.join("registry");
    fs::DirBuilder::new().mode(0o700).create(&registry)?;
    let nonce = checkpoint::Nonce::random()?;
    if options.command.len() < 3 {
        return Err(Error::Policy(
            "specify bash, run-native-test.sh and the native test command",
        ));
    }
    let mut invocation = options.command[..2].to_vec();
    invocation.extend([
        std::ffi::OsString::from("--registry"),
        registry.clone().into_os_string(),
    ]);
    invocation.extend_from_slice(&options.command[2..]);
    let progress = Progress {
        started: Instant::now(),
    };
    let mut cancellation = Cancellation::new()?;
    let mut result = serde_json::Map::new();
    for (name, value) in [
        ("version", serde_json::json!(1)),
        ("outcome", serde_json::json!("failed")),
        (
            "command",
            serde_json::to_value(
                invocation
                    .iter()
                    .map(|arg| {
                        arg.to_str()
                            .ok_or(Error::Policy("invalid UTF-8 invocation metadata"))
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            )?,
        ),
        ("nonce", serde_json::to_value(&nonce)?),
        ("registry", serde_json::to_value(&registry)?),
        ("output", serde_json::to_value(&output)?),
        ("revision", options.revision.clone().into()),
        (
            "host_namespace",
            serde_json::to_value(fs::read_link("/proc/self/ns/mnt")?)?,
        ),
        (
            "started_at",
            chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now())
                .to_rfc3339()
                .into(),
        ),
    ] {
        result.insert(name.into(), value);
    }
    let mut supervisor = None;
    let mut observation: Option<(
        cleanup::Subject,
        witness::Witness,
        Option<witness::Snapshot>,
    )> = None;
    let mut passed = false;
    let outcome = (|| {
        let revision = command::checked(
            std::process::Command::new("git").args(["rev-parse", "HEAD"]),
            Duration::from_secs(45),
        )?;
        result.insert(
            "checkout_revision".into(),
            String::from_utf8(revision)
                .map_err(|_| Error::Policy("invalid checkout revision"))?
                .trim()
                .to_owned()
                .into(),
        );
        if options.revision.len() != 40
            || !options
                .revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(Error::Policy(
                "native acceptance requires a clean Git-filtered, revision-bound app build",
            ));
        }
        progress.say(format_args!(
            "Starting native interruption test; full diagnostics: {}",
            output.display()
        ))?;
        supervisor = Some(supervisor::Supervisor::spawn(
            &invocation,
            &nonce,
            File::create(output.join("native.log"))?,
            options.stop_timeout,
        )?);
        let owner = supervisor
            .as_mut()
            .ok_or(Error::Policy("missing started native supervisor"))?;
        result.insert("supervisor_pid".into(), owner.id().into());
        let deadline = progress.started + options.ready_timeout;
        let mut next_report = Instant::now();
        let mut last_phase = None;
        let root = loop {
            cancellation.check()?;
            if owner.exited()?.is_some() {
                result.insert("native_exit".into(), owner.finish()?.into());
                return Err(Error::Policy(
                    "native test exited before verified readiness",
                ));
            }
            let readiness = checkpoint::observe(&registry, &nonce, |root, phase| {
                result.insert("fixture".into(), serde_json::to_value(root)?);
                if last_phase != Some(phase) {
                    last_phase = Some(phase);
                    result.insert("setup_phase".into(), phase.as_str().into());
                    progress.say(format_args!("Native setup: {}", phase.as_str()))?;
                }
                Ok(())
            })?;
            if let Some(root) = readiness {
                break root;
            }
            if Instant::now() >= deadline {
                return Err(Error::Deadline(
                    "Waiting for pool and two workers (readiness deadline applies)",
                ));
            }
            if Instant::now() >= next_report {
                progress.say(format_args!("Waiting for pool and two workers (readiness deadline applies); current setup: {}",last_phase.map_or("fixture initialization",checkpoint::Phase::as_str)))?;
                next_report = Instant::now() + Duration::from_secs(15);
            }
            thread::sleep(Duration::from_millis(100));
        };
        result.insert("fixture".into(), serde_json::to_value(root.path())?);
        result.insert(
            "ready_elapsed_seconds".into(),
            progress.started.elapsed().as_secs_f64().into(),
        );
        let subject = cleanup::Subject::new(root, options.tools.clone())?;
        progress
            .say("Workers ready; checking native objects and pinning original Podman namespace")?;
        let witness =
            witness::Witness::spawn(&subject, File::create(output.join("namespace.log"))?)?;
        observation = Some((subject, witness, None));
        let (subject, witness, before) = observation
            .as_mut()
            .ok_or(Error::Policy("missing namespace observation"))?;
        let snapshot = witness.snapshot(&output.join("namespace-before.json"), &progress)?;
        *before = Some(snapshot);
        witness::verify_before(
            subject,
            before
                .as_ref()
                .ok_or(Error::Policy("missing namespace snapshot"))?,
        )?;
        progress.say("Verified native readiness and original Podman namespace; sending SIGTERM to owned supervisor")?;
        owner.signal()?;
        result.insert("signal".into(), "SIGTERM".into());
        result.insert(
            "signal_elapsed_seconds".into(),
            progress.started.elapsed().as_secs_f64().into(),
        );
        let deadline = Instant::now() + options.stop_timeout;
        while owner.exited()?.is_none() {
            cancellation.check()?;
            if Instant::now() >= deadline {
                return Err(Error::Deadline("Waiting for graceful native teardown"));
            }
            thread::sleep(Duration::from_millis(100));
        }
        result.insert(
            "native_exit".into(),
            owner
                .exited()?
                .ok_or(Error::Policy("native exit disappeared"))?
                .into(),
        );
        result.insert(
            "native_exit_elapsed_seconds".into(),
            progress.started.elapsed().as_secs_f64().into(),
        );
        let helpers = owner.stop_helpers(deadline, &output, &progress)?;
        result.insert(
            "assertion_helpers_terminated".into(),
            serde_json::to_value(helpers)?,
        );
        let code = owner.finish()?;
        result.insert("native_exit".into(), code.into());
        if code != 143 {
            return Err(Error::Policy(
                "native supervisor did not report expected intentional SIGTERM exit 143",
            ));
        }
        let before = before.as_ref().ok_or(Error::Policy(
            "no pre-interruption namespace identity to verify against",
        ))?;
        Inventory {
            subject,
            before: Some(before),
            witness,
            output: &output,
        }
        .verify(&mut result, &progress)?;
        witness.close(&progress)?;
        passed = true;
        progress.say("PASS: no fixture containers, images, host mounts or exports in the original Podman namespace")?;
        Ok::<_, Error>(())
    })();
    if let Err(error) = outcome {
        passed = false;
        result.insert("error".into(), error.to_string().into());
        progress.say(format_args!(
            "FAIL: {error}; retaining registry, files and full logs"
        ))?;
    }
    if let Some(owner) = supervisor.as_mut() {
        if let Err(error) = owner.capture_helpers("before-abort", &output, &progress) {
            passed = false;
            result.insert("helper_diagnostics_error".into(), error.to_string().into());
        }
        if let Err(error) = owner.abort(&progress) {
            passed = false;
            result.insert("supervisor_cleanup_error".into(), error.to_string().into());
        }
        if let Some(code) = owner.code() {
            result.insert("native_exit".into(), code.into());
        }
    }
    if let Some((subject, witness, before)) = observation.as_mut()
        && !witness.closed()
    {
        let inventory = Inventory {
            subject,
            before: before.as_ref(),
            witness,
            output: &output,
        }
        .verify(&mut result, &progress);
        if let Err(error) = inventory {
            passed = false;
            progress.say(format_args!("Final inventory failed: {error}"))?;
        }
        if let Err(error) = witness.close(&progress) {
            passed = false;
            result.insert("witness_cleanup_error".into(), error.to_string().into());
        }
    }
    result.insert(
        "outcome".into(),
        if passed { "passed" } else { "failed" }.into(),
    );
    result.insert(
        "elapsed_seconds".into(),
        progress.started.elapsed().as_secs_f64().into(),
    );
    write_json(&output.join("result.json"), &result)?;
    fs::write(output.join("exit"), if passed { "0\n" } else { "1\n" })?;
    progress.say(format_args!(
        "Evidence and retained fixture registry: {}",
        output.display()
    ))?;
    Ok(passed)
}
