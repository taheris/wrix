use std::{fs, io, path::Path};

const MARKER: &str = "\"@EXPECTED_RUSTC@\"";

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// fixture source I/O failed: {0}
    Io(#[from] io::Error),
    /// expected one quoted compiler marker, found {0}
    Markers(usize),
    /// expected compiler must be an absolute UTF-8 path
    Compiler,
}

pub fn stamp_rustc(source: &Path, compiler: &Path) -> Result<(), Error> {
    let compiler = compiler
        .to_str()
        .filter(|_| compiler.is_absolute())
        .ok_or(Error::Compiler)?;
    let contents = fs::read_to_string(source)?;
    let count = contents.matches(MARKER).count();
    if count != 1 {
        return Err(Error::Markers(count));
    }
    fs::write(
        source,
        contents.replacen(MARKER, &format!("{compiler:?}"), 1),
    )?;
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use std::{os::unix::fs::PermissionsExt, process::Command};

    const BUILD_SCRIPT: &str =
        include_str!("../../../../tests/fixtures/build-package-fixture/build.rs");

    fn compile(source: &Path, output: &Path) {
        let result = Command::new("rustc")
            .args(["--edition=2024", "--crate-name", "fixture_build"])
            .arg(source)
            .arg("-o")
            .arg(output)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    #[test]
    fn stamped_build_script_accepts_only_the_selected_compiler() {
        let root = tempfile::tempdir().unwrap();
        let compiler = root.path().join("selected-rustc");
        fs::write(&compiler, "#!/usr/bin/env bash\nset -euo pipefail\ntest \"$*\" = '--print sysroot'\nprintf '/owned/sysroot\\n'\n").unwrap();
        fs::set_permissions(&compiler, fs::Permissions::from_mode(0o700)).unwrap();
        let source = root.path().join("build.rs");
        fs::write(&source, BUILD_SCRIPT).unwrap();
        stamp_rustc(&source, &compiler).unwrap();
        let executable = root.path().join("build-script");
        compile(&source, &executable);
        let accepted = Command::new(&executable)
            .env("RUSTC", &compiler)
            .output()
            .unwrap();
        assert!(
            accepted.status.success(),
            "{}",
            String::from_utf8_lossy(&accepted.stderr)
        );
        assert!(
            String::from_utf8(accepted.stdout)
                .unwrap()
                .contains("BUILD_RUSTC_SYSROOT=/owned/sysroot")
        );
        let rejected = Command::new(&executable)
            .env("RUSTC", root.path().join("wrong-rustc"))
            .output()
            .unwrap();
        assert!(!rejected.status.success());
        assert!(
            String::from_utf8(rejected.stderr)
                .unwrap()
                .contains("Cargo selected")
        );
    }

    #[test]
    fn marker_count_and_relative_compiler_fail_without_changing_source() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("build.rs");
        for text in ["no marker".into(), format!("{MARKER} {MARKER}")] {
            fs::write(&source, &text).unwrap();
            assert!(matches!(
                stamp_rustc(&source, Path::new("/owned/rustc")),
                Err(Error::Markers(_))
            ));
            assert_eq!(fs::read_to_string(&source).unwrap(), text);
        }
        fs::write(&source, BUILD_SCRIPT).unwrap();
        assert!(matches!(
            stamp_rustc(&source, Path::new("rustc")),
            Err(Error::Compiler)
        ));
        assert_eq!(fs::read_to_string(&source).unwrap(), BUILD_SCRIPT);
    }

    #[test]
    fn compiler_path_is_escaped_as_a_rust_string_literal() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("main.rs");
        fs::write(
            &source,
            format!("fn main() {{ print!(\"{{}}\", {MARKER}); }}"),
        )
        .unwrap();
        let compiler = "/owned/two \"quoted\" words\\λ/rustc";
        stamp_rustc(&source, Path::new(compiler)).unwrap();
        let executable = root.path().join("literal");
        compile(&source, &executable);
        let output = Command::new(executable).output().unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap(), compiler);
    }
}
