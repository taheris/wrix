use crate::runner_policy;
use std::{
    ffi::OsString,
    io::{self, Read},
    path::Path,
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// usage: wrix-verify-tools runner-policy `LOOM_TOML` (verifier inventory on stdin)
    Usage,
    /// verifier inventory I/O failed: {0}
    Io(#[from] io::Error),
    /// {0}
    Policy(#[from] runner_policy::Error),
}

pub fn run(args: impl IntoIterator<Item = OsString>) -> Result<(), Error> {
    let args: Vec<_> = args.into_iter().collect();
    let [name, path] = args.as_slice() else {
        return Err(Error::Usage);
    };
    if name != "runner-policy" {
        return Err(Error::Usage);
    }
    let mut inventory = String::new();
    io::stdin().lock().read_to_string(&mut inventory)?;
    runner_policy::audit(Path::new(path), &inventory)?;
    Ok(())
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
