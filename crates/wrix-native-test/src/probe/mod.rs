pub mod cgroup;
pub mod csv;
#[cfg(target_os = "linux")]
pub mod network;
pub mod repair;
pub mod snapshot;
pub mod strict;
pub mod worker;

use std::{
    ffi::OsString,
    io::{self, Write},
    path::Path,
};
#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// usage: operator-probe fixed-repair|strict-plan|strict-host|strict-guest|cgroup|cgroup-mapped|network-plan|network-worker|worker ARGS
    Usage,
    /// operator snapshot failed: {0}
    Snapshot(#[from] snapshot::Error),
    /// historical repair verification failed: {0}
    Repair(#[from] repair::Error),
    /// disposable guest worker failed: {0}
    Worker(#[from] worker::Error),
    /// strict native Nix probe failed: {0}
    Strict(#[from] strict::Error),
    /// native disposable cgroup probe failed: {0}
    #[cfg(target_os = "linux")]
    Cgroup(#[from] cgroup::live::Error),
    /// native mapped cgroup probe failed: {0}
    #[cfg(target_os = "linux")]
    Mapped(#[from] cgroup::live::mapped::Error),
    /// disposable network worker failed: {0}
    #[cfg(target_os = "linux")]
    Network(#[from] network::Error),
    /// inert network derivation plan failed: {0}
    #[cfg(target_os = "linux")]
    NetworkPlan(#[from] network::plan::Error),
    /// operator probe output failed: {0}
    Io(#[from] io::Error),
    /// operator probe encoding failed: {0}
    Json(#[from] serde_json::Error),
}

pub fn run(args: &[OsString]) -> Result<(), Error> {
    #[cfg(target_os = "linux")]
    if let [operation, rest @ ..] = args
        && operation == "network-plan"
    {
        network::plan::entry(rest)?;
        return Ok(());
    }
    if let [
        operation,
        runtime_flag,
        runtime,
        nix_flag,
        nix,
        program_flag,
        program,
        system_flag,
        system,
        execute,
    ] = args
        && operation == "strict-host"
        && runtime_flag == "--container-runtime"
        && nix_flag == "--nix"
        && program_flag == "--program"
        && system_flag == "--system"
        && execute == "--run"
    {
        let program = strict::Program::parse(Path::new(program))?;
        let observation = strict::host(
            Path::new(runtime),
            Path::new(nix),
            &program,
            strict::System::parse(system)?,
        )?;
        let mut stdout = io::stdout().lock();
        serde_json::to_writer(&mut stdout, &observation)?;
        writeln!(stdout)?;
        if !observation.success {
            return Err(strict::Error::Refused.into());
        }
        return Ok(());
    }
    if let [
        operation,
        program_flag,
        program,
        marker_flag,
        marker,
        system_flag,
        system,
    ] = args
        && operation == "strict-plan"
        && program_flag == "--program"
        && marker_flag == "--marker"
        && system_flag == "--system"
    {
        let program = strict::Program::parse(Path::new(program))?;
        let plan = strict::plan(&program, Path::new(marker), strict::System::parse(system)?)?;
        let mut stdout = io::stdout().lock();
        serde_json::to_writer(&mut stdout, &plan)?;
        writeln!(stdout)?;
        return Ok(());
    }
    if let [
        operation,
        nix_flag,
        nix,
        program_flag,
        program,
        marker_flag,
        marker,
        system_flag,
        system,
        execute,
    ] = args
        && operation == "strict-guest"
        && nix_flag == "--nix"
        && program_flag == "--program"
        && marker_flag == "--marker"
        && system_flag == "--system"
        && execute == "--run"
    {
        let program = strict::Program::parse(Path::new(program))?;
        let observation = strict::run(
            Path::new(nix),
            &program,
            Path::new(marker),
            strict::System::parse(system)?,
        )?;
        let mut stdout = io::stdout().lock();
        serde_json::to_writer(&mut stdout, &observation)?;
        writeln!(stdout)?;
        if !observation.success {
            return Err(strict::Error::Refused.into());
        }
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    if let [operation, rest @ ..] = args
        && matches!(
            operation.to_str(),
            Some("mapped-bootstrap" | "mapped-delegate" | "mapped-job")
        )
    {
        let worker = cgroup::live::mapped::Worker::parse(operation)?;
        cgroup::live::mapped::worker(worker, rest)?;
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    if let [operation, root_flag, root, util_flag, util, execute] = args
        && operation == "cgroup-mapped"
        && root_flag == "--root"
        && util_flag == "--util"
        && execute == "--run"
    {
        let receipt = cgroup::live::mapped::run(Path::new(root), Path::new(util))?;
        let mut stdout = io::stdout().lock();
        serde_json::to_writer(&mut stdout, &receipt)?;
        writeln!(stdout)?;
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    if let [operation, rest @ ..] = args
        && operation == "network-worker"
    {
        network::run(rest)?;
        return Ok(());
    }
    if let [operation, rest @ ..] = args
        && operation == "worker"
    {
        worker::run(rest)?;
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    if let [operation, root_flag, root, execute] = args
        && operation == "cgroup"
        && root_flag == "--root"
        && execute == "--run"
    {
        let receipt = cgroup::live::run(Path::new(root))?;
        let mut stdout = io::stdout().lock();
        serde_json::to_writer(&mut stdout, &receipt)?;
        writeln!(stdout)?;
        return Ok(());
    }
    let [operation, flag, root, rest @ ..] = args else {
        return Err(Error::Usage);
    };
    if operation != "fixed-repair" || flag != "--root" {
        return Err(Error::Usage);
    }
    let mode = match rest {
        [] => None,
        [mode, bd_flag, bd, container_flag, container]
            if mode == "--read-only"
                && bd_flag == "--bd"
                && container_flag == "--container"
                && Path::new(bd).is_absolute()
                && Path::new(container).is_absolute() =>
        {
            Some((Path::new(bd), Path::new(container)))
        }
        _ => return Err(Error::Usage),
    };
    let directory = snapshot::Directory::open(Path::new(root))?;
    let plan = repair::Plan::load(&directory)?;
    let output = match mode {
        None => {
            serde_json::json!({"dry_run":true,"query":plan.query(),"container_arguments":["inspect","wrix-host-service"],"mutation":"none"})
        }
        Some((bd, container)) => serde_json::to_value(plan.run(bd, container)?)?,
    };
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &output)?;
    writeln!(stdout)?;
    Ok(())
}
