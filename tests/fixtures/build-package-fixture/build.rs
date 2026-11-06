use std::{env, error::Error, io, path::PathBuf, process::Command};

const EXPECTED_RUSTC: &str = "@EXPECTED_RUSTC@";

fn resolve_rustc(rustc: &str) -> Result<PathBuf, io::Error> {
    let path = PathBuf::from(rustc);
    if path.components().count() > 1 {
        return Ok(path);
    }
    env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .map(|directory| directory.join(rustc))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "RUSTC is not on PATH"))
}

fn main() -> Result<(), Box<dyn Error>> {
    let rustc = env::var("RUSTC")?;
    let selected = resolve_rustc(&rustc)?;
    if EXPECTED_RUSTC != concat!("@EXPECTED_", "RUSTC@")
        && selected != PathBuf::from(EXPECTED_RUSTC)
    {
        return Err(io::Error::other(format!(
            "Cargo selected {}, expected {EXPECTED_RUSTC}",
            selected.display()
        ))
        .into());
    }
    let output = Command::new(&selected)
        .args(["--print", "sysroot"])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("selected rustc failed to report its sysroot").into());
    }
    let sysroot = String::from_utf8(output.stdout)?;
    println!("cargo:rustc-env=BUILD_RUSTC={}", selected.display());
    println!("cargo:rustc-env=BUILD_RUSTC_SYSROOT={}", sysroot.trim());
    Ok(())
}
