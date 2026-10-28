use std::{fs, path::Path};

pub fn install(root: &Path) -> super::TestResult {
    let path = std::env::var_os("PATH").ok_or("missing fixture PATH")?;
    let flock = std::env::split_paths(&path)
        .map(|directory| directory.join("flock"))
        .find(|program| program.is_file())
        .ok_or("missing external flock")?;
    let compiler =
        std::env::var_os("WRIX_TEST_SCCACHE").ok_or("missing patched native compiler")?;
    let bin = root.join("bin");
    fs::write(
        bin.join("flock"),
        format!(
            "#!/usr/bin/env bash\nset -euo pipefail\nif [[ \"${{!#}}\" == sccache ]]; then exec '{}' \"$@\"; fi\nexec sleep 240\n",
            flock.display()
        ),
    )?;
    fs::write(
        bin.join("sccache"),
        format!(
            "#!/usr/bin/env bash\nset -euo pipefail\nexec '{}' \"$@\"\n",
            Path::new(&compiler).display()
        ),
    )?;
    for name in ["flock", "sccache"] {
        super::set_mode(&bin.join(name), 0o700)?;
    }
    Ok(())
}
