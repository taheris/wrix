use crate::{fixture, runner_policy};
use std::{
    ffi::OsString,
    io::{self, Read},
    path::Path,
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// usage: wrix-verify-tools runner-policy `LOOM_TOML` | stamp-rustc SOURCE COMPILER
    Usage,
    /// verifier inventory I/O failed: {0}
    Io(#[from] io::Error),
    /// {0}
    Policy(#[from] runner_policy::Error),
    /// {0}
    Fixture(#[from] fixture::Error),
    /// image query found no matching member
    NoMember,
    /// {0}
    Image(#[from] crate::artifact::image::command::Error),
    /// {0}
    Shared(#[from] crate::artifact::image::shared::Error),
    /// {0}
    Permissions(#[from] crate::artifact::image::permissions::Error),
    /// {0}
    BuilderAccount(#[from] crate::builder_account::Error),
}

pub fn run(args: impl IntoIterator<Item = OsString>) -> Result<(), Error> {
    let args: Vec<_> = args.into_iter().collect();
    match args.as_slice() {
        [name, passwd, group, configured, trusted] if name == "builder-build-users" => {
            let configured = configured.to_str().ok_or(Error::Usage)?;
            let trusted = trusted.to_str().ok_or(Error::Usage)?;
            crate::builder_account::run(Path::new(passwd), Path::new(group), configured, trusted)?;
            Ok(())
        }
        [name, cases, archive] if name == "private-nix-artifacts" => {
            crate::artifact::image::permissions::profile::inspect(
                Path::new(cases),
                Path::new(archive),
            )?;
            Ok(())
        }
        [name, cases, permissions] if name == "shared-nix-image" => {
            use std::io::Write;
            let count =
                crate::artifact::image::shared::run(Path::new(cases), Path::new(permissions))?;
            writeln!(
                io::stdout().lock(),
                "shared Nix image contracts: PASS ({count} actual artifacts, negative controls)"
            )?;
            Ok(())
        }
        [name, source] if name == "native-nix-permissions" => {
            crate::artifact::image::permissions::run(
                Path::new(source),
                crate::artifact::image::permissions::Format::Oci,
            )?;
            Ok(())
        }
        [name, flag, source] if name == "native-nix-permissions" && flag == "--archive" => {
            crate::artifact::image::permissions::run(
                Path::new(source),
                crate::artifact::image::permissions::Format::Archive,
            )?;
            Ok(())
        }
        [name, rest @ ..] if name == "image-artifact" => {
            if crate::artifact::image::Image::run(rest, &mut io::stdout().lock())? {
                Ok(())
            } else {
                Err(Error::NoMember)
            }
        }
        [name, path] if name == "runner-policy" => {
            let mut inventory = String::new();
            io::stdin().lock().read_to_string(&mut inventory)?;
            runner_policy::audit(Path::new(path), &inventory)?;
            Ok(())
        }
        [name, source, compiler] if name == "stamp-rustc" => {
            fixture::stamp_rustc(Path::new(source), Path::new(compiler))?;
            Ok(())
        }
        _ => Err(Error::Usage),
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn missing_extra_and_unknown_arguments_fail_before_stdin_is_read() {
        for args in [
            vec![],
            vec!["runner-policy"],
            vec!["unknown", "path"],
            vec!["runner-policy", "path", "extra"],
        ] {
            assert!(matches!(
                run(args.into_iter().map(OsString::from)),
                Err(Error::Usage)
            ));
        }
    }
}
