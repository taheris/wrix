use std::ffi::OsString;

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// usage: wrix-native-test native-fixture-cleanup ARGS | native-interruption ARGS
    Usage,
    /// {0}
    Cleanup(#[from] crate::native_fixture::cleanup::Error),
    /// {0}
    #[cfg(target_os = "linux")]
    Interruption(#[from] crate::native_fixture::interruption::Error),
}
pub fn run(args: impl IntoIterator<Item = OsString>) -> Result<(), Error> {
    let args: Vec<_> = args.into_iter().collect();
    match args.as_slice() {
        [name, rest @ ..] if name == "native-fixture-cleanup" => {
            crate::native_fixture::cleanup::entry::run(rest)?;
            Ok(())
        }
        #[cfg(target_os = "linux")]
        [name, rest @ ..] if name == "native-interruption" => {
            crate::native_fixture::interruption::entry::run(rest)?;
            Ok(())
        }
        _ => Err(Error::Usage),
    }
}
#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn unknown_or_missing_native_operation_fails_before_effects() {
        for args in [vec![], vec![OsString::from("unknown")]] {
            assert!(matches!(run(args), Err(Error::Usage)));
        }
    }
}
