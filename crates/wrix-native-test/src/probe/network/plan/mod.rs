use super::{Hash, Mode, Private, Public};
use crate::probe::strict::{self, Program, System};
use std::{
    ffi::OsString,
    io::{self, Write},
    path::{Component, Path, PathBuf},
};
#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// invalid inert network derivation plan arguments or certificate store path
    Usage,
    /// network plan input failed: {0}
    Input(#[from] super::Error),
    /// network plan immutable builder or quoting failed: {0}
    Builder(#[from] strict::Error),
    /// network plan encoding failed: {0}
    Json(#[from] serde_json::Error),
    /// network plan output failed: {0}
    Io(#[from] io::Error),
}
struct Certificate {
    file: PathBuf,
    root: PathBuf,
}
impl Certificate {
    fn parse(path: &Path) -> Result<Self, Error> {
        let parts: Vec<_> = path
            .strip_prefix("/nix/store")
            .map_err(|_| Error::Usage)?
            .components()
            .collect();
        let [
            Component::Normal(package),
            Component::Normal(etc),
            Component::Normal(ssl),
            Component::Normal(certs),
            Component::Normal(bundle),
        ] = parts.as_slice()
        else {
            return Err(Error::Usage);
        };
        let bytes = package.as_encoded_bytes();
        if bytes.len() <= 33
            || bytes[32] != b'-'
            || !bytes[..32]
                .iter()
                .all(|byte| b"0123456789abcdfghijklmnpqrsvwxyz".contains(byte))
            || !bytes[33..]
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || b"+-._?=".contains(byte))
            || !package.to_str().ok_or(Error::Usage)?.contains("nss-cacert")
            || *etc != "etc"
            || *ssl != "ssl"
            || *certs != "certs"
            || *bundle != "ca-bundle.crt"
        {
            return Err(Error::Usage);
        }
        Ok(Self {
            file: path.to_owned(),
            root: Path::new("/nix/store").join(package),
        })
    }
}
#[derive(serde::Serialize)]
pub struct Plan {
    ordinary: String,
    fixed_output: String,
    native_execution: bool,
}
struct Inputs<'a> {
    program: &'a Program,
    certificate: Certificate,
    public: Public,
    private: Private,
    expected: Hash,
    system: System,
}
impl Inputs<'_> {
    fn expression(&self, mode: Mode) -> Result<String, Error> {
        let root = strict::quote(self.program.root()?.to_str().ok_or(Error::Usage)?)?;
        let certificate = strict::quote(self.certificate.root.to_str().ok_or(Error::Usage)?)?;
        let ca = strict::quote(self.certificate.file.to_str().ok_or(Error::Usage)?)?;
        let public = strict::quote(self.public.0.as_str())?;
        let private = strict::quote(self.private.0.as_str())?;
        let expected = strict::quote(&self.expected.0)?;
        let (name, worker, fixed) = match mode {
            Mode::Pure => ("wrix-npfo4-real-pure-network-v6", "pure", String::new()),
            Mode::Fixed => (
                "wrix-npfo4-real-fod-network-v6",
                "fixed",
                format!(
                    "outputHashAlgo=\"sha256\"; outputHashMode=\"flat\"; outputHash={expected};"
                ),
            ),
        };
        Ok(format!(
            "let probe=builtins.storePath {root}; ca=builtins.storePath {certificate}; in derivation {{ name=\"{name}\"; system=\"{}\"; builder=probe + \"/bin/wrix-native-test\"; certificateRoot=ca; args=[\"operator-probe\" \"network-worker\" \"{worker}\" {ca} {public} {private} {expected}]; {fixed} }}",
            self.system.wire()
        ))
    }
    fn plan(&self) -> Result<Plan, Error> {
        Ok(Plan {
            ordinary: self.expression(Mode::Pure)?,
            fixed_output: self.expression(Mode::Fixed)?,
            native_execution: false,
        })
    }
}
pub fn entry(args: &[OsString]) -> Result<(), Error> {
    let [
        program_flag,
        program,
        ca_flag,
        ca,
        public_flag,
        public,
        private_flag,
        private,
        hash_flag,
        hash,
        system_flag,
        system,
    ] = args
    else {
        return Err(Error::Usage);
    };
    if program_flag != "--program"
        || ca_flag != "--ca"
        || public_flag != "--public"
        || private_flag != "--private"
        || hash_flag != "--sha256"
        || system_flag != "--system"
    {
        return Err(Error::Usage);
    }
    let program = Program::parse(Path::new(program))?;
    let inputs = Inputs {
        program: &program,
        certificate: Certificate::parse(Path::new(ca))?,
        public: Public::parse(public.to_str().ok_or(Error::Usage)?)?,
        private: Private::parse(private.to_str().ok_or(Error::Usage)?)?,
        expected: Hash::parse(hash.to_str().ok_or(Error::Usage)?)?,
        system: System::parse(system)?,
    };
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &inputs.plan()?)?;
    writeln!(stdout)?;
    Ok(())
}
#[cfg(test)]
mod test;
