//! External Nix CLI fixture for selected publisher graphs and upstream replies.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::symlink,
    path::PathBuf,
    process::ExitCode,
};

use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// publisher fixture I/O failed: {0}
    Io(#[from] io::Error),
    /// publisher fixture document failed: {0}
    Json(#[from] serde_json::Error),
    /// missing publisher fixture environment: {0}
    Environment(&'static str),
    /// invalid publisher fixture arguments
    Arguments,
    /// missing selected fixture output
    Output,
    /// invalid upstream fixture mode
    Mode,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Upstream {
    Normal,
    Partial,
    Unavailable,
    Malformed,
}

impl Upstream {
    fn parse(value: &std::ffi::OsStr) -> Result<Self, Error> {
        match value.to_str() {
            Some("normal") => Ok(Self::Normal),
            Some("partial") => Ok(Self::Partial),
            Some("unavailable") => Ok(Self::Unavailable),
            Some("malformed") => Ok(Self::Malformed),
            _ => Err(Error::Mode),
        }
    }
}

#[derive(Deserialize)]
struct Inventory {
    roots: Vec<Root>,
}

#[derive(Deserialize)]
struct Root {
    drv_path: String,
    installable: String,
    out_paths: Vec<String>,
}

impl Root {
    fn output(&self) -> Result<&str, Error> {
        self.out_paths
            .first()
            .map(String::as_str)
            .ok_or(Error::Output)
    }
}

fn environment(
    get: &impl Fn(&str) -> Option<OsString>,
    key: &'static str,
) -> Result<OsString, Error> {
    get(key).ok_or(Error::Environment(key))
}

fn inventory(get: &impl Fn(&str) -> Option<OsString>) -> Result<Vec<Root>, Error> {
    let path = PathBuf::from(environment(get, "WRIX_CACHE_ROOTS_FILE")?);
    Ok(serde_json::from_slice::<Inventory>(&fs::read(path)?)?.roots)
}

fn json_line(output: &mut impl Write, value: &Value) -> Result<(), Error> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    Ok(())
}

fn dispatch(
    args: &[OsString],
    get: impl Fn(&str) -> Option<OsString>,
    output: &mut impl Write,
    diagnostic: &mut impl Write,
) -> Result<ExitCode, Error> {
    let args = args
        .iter()
        .map(|arg| arg.to_str().ok_or(Error::Arguments))
        .collect::<Result<Vec<_>, _>>()?;
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(environment(&get, "WRIX_TEST_NIX_LOG")?)?;
    writeln!(log, "{}", args.join(" "))?;
    drop(log);
    if args == ["config", "show", "post-build-hook"] {
        output.write_all(b"/nix/store/fixture/bin/wrix-cache-post-build-hook\n")?;
        return Ok(ExitCode::SUCCESS);
    }
    if args.starts_with(&["derivation", "show", "--recursive"]) {
        let mut graph = BTreeMap::new();
        for root in inventory(&get)? {
            if args[3..].contains(&root.drv_path.as_str()) {
                graph.insert(
                    root.drv_path.clone(),
                    json!({"outputs":{"out":{"path":root.output()?}}}),
                );
            }
        }
        json_line(output, &serde_json::to_value(graph)?)?;
        return Ok(ExitCode::SUCCESS);
    }
    if args.starts_with(&["build", "--json", "--out-link"]) {
        if get("WRIX_TEST_BUILD_FAIL").is_some_and(|value| value == "1") {
            return Ok(ExitCode::FAILURE);
        }
        if args.len() <= 4 {
            return Err(Error::Arguments);
        }
        let mut built = Vec::new();
        for root in inventory(&get)? {
            if args[4..].contains(&root.installable.as_str()) {
                let index = built.len();
                let link = if index == 0 {
                    args[3].to_owned()
                } else {
                    format!("{}-{index}", args[3])
                };
                symlink(root.output()?, link)?;
                built.push(json!({"drvPath":root.drv_path,"outputs":{"out":root.output()?}}));
            }
        }
        json_line(output, &Value::Array(built))?;
        return Ok(ExitCode::SUCCESS);
    }
    if args.starts_with(&["path-info", "--store"]) {
        let Some(flag) = args.get(3) else {
            return Err(Error::Arguments);
        };
        if *flag == "--json" {
            let mode = Upstream::parse(&environment(&get, "WRIX_TEST_UPSTREAM")?)?;
            if mode == Upstream::Unavailable {
                return Ok(ExitCode::FAILURE);
            }
            if mode == Upstream::Malformed {
                output.write_all(b"invalid JSON\n")?;
                return Ok(ExitCode::SUCCESS);
            }
            let hit = environment(&get, "WRIX_TEST_UPSTREAM_HIT")?;
            let mut paths = BTreeMap::new();
            for path in &args[4..] {
                paths.insert(
                    *path,
                    if std::ffi::OsStr::new(path) == hit {
                        json!({})
                    } else {
                        Value::Null
                    },
                );
            }
            json_line(output, &serde_json::to_value(paths)?)?;
            return Ok(if mode == Upstream::Normal {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            });
        }
    }
    diagnostic.write_all(b"unexpected Nix invocation\n")?;
    Ok(ExitCode::from(2))
}

pub fn run(args: &[OsString]) -> Result<ExitCode, Error> {
    dispatch(
        args,
        |key| std::env::var_os(key),
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    )
}

#[cfg(test)]
mod test {
    use super::*;
    use std::{collections::BTreeMap, path::Path};

    struct Fixture {
        root: tempfile::TempDir,
        environment: BTreeMap<&'static str, OsString>,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let roots = root.path().join("roots.json");
            fs::write(&roots, json!({"roots":[
                {"drv_path":"/nix/store/a.drv","installable":".#a","out_paths":["/nix/store/a"]},
                {"drv_path":"/nix/store/b.drv","installable":".#b","out_paths":["/nix/store/b"]}
            ]}).to_string()).unwrap();
            let environment = BTreeMap::from([
                (
                    "WRIX_TEST_NIX_LOG",
                    root.path().join("log").into_os_string(),
                ),
                ("WRIX_CACHE_ROOTS_FILE", roots.into_os_string()),
                ("WRIX_TEST_UPSTREAM_HIT", OsString::from("/nix/store/a")),
                ("WRIX_TEST_UPSTREAM", OsString::from("normal")),
            ]);
            Self { root, environment }
        }

        fn run(&self, args: &[&str]) -> (ExitCode, Vec<u8>, Vec<u8>) {
            let mut output = Vec::new();
            let mut diagnostic = Vec::new();
            let args = args.iter().map(OsString::from).collect::<Vec<_>>();
            let code = dispatch(
                &args,
                |key| self.environment.get(key).cloned(),
                &mut output,
                &mut diagnostic,
            )
            .unwrap();
            (code, output, diagnostic)
        }
    }

    #[test]
    fn graph_contains_only_selected_derivations_and_commands_are_logged() {
        let fixture = Fixture::new();
        let (code, output, diagnostic) =
            fixture.run(&["derivation", "show", "--recursive", "/nix/store/b.drv"]);
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(diagnostic, [] as [u8; 0]);
        let actual: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(
            actual,
            json!({"/nix/store/b.drv":{"outputs":{"out":{"path":"/nix/store/b"}}}})
        );
        assert_eq!(
            fs::read_to_string(fixture.root.path().join("log")).unwrap(),
            "derivation show --recursive /nix/store/b.drv\n"
        );
    }

    #[test]
    fn build_preserves_inventory_order_in_result_links() {
        let fixture = Fixture::new();
        let link = fixture.root.path().join("result");
        let (code, output, _) = fixture.run(&[
            "build",
            "--json",
            "--out-link",
            link.to_str().unwrap(),
            ".#b",
            ".#a",
        ]);
        assert_eq!(code, ExitCode::SUCCESS);
        let actual: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(actual[0]["drvPath"], "/nix/store/a.drv");
        assert_eq!(actual[1]["drvPath"], "/nix/store/b.drv");
        assert_eq!(fs::read_link(link).unwrap(), Path::new("/nix/store/a"));
        assert_eq!(
            fs::read_link(fixture.root.path().join("result-1")).unwrap(),
            Path::new("/nix/store/b")
        );
    }

    #[test]
    fn existing_result_links_are_not_replaced() {
        let fixture = Fixture::new();
        let link = fixture.root.path().join("result");
        symlink("/nix/store/retained", &link).unwrap();
        let args = [
            "build",
            "--json",
            "--out-link",
            link.to_str().unwrap(),
            ".#a",
        ]
        .map(OsString::from);
        let error = dispatch(
            &args,
            |key| fixture.environment.get(key).cloned(),
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(matches!(error, Error::Io(error) if error.kind() == io::ErrorKind::AlreadyExists));
        assert_eq!(
            fs::read_link(link).unwrap(),
            Path::new("/nix/store/retained")
        );
    }

    #[test]
    fn failed_build_returns_before_creating_links_or_reading_inventory() {
        let mut fixture = Fixture::new();
        fixture
            .environment
            .insert("WRIX_TEST_BUILD_FAIL", "1".into());
        fs::remove_file(fixture.environment["WRIX_CACHE_ROOTS_FILE"].clone()).unwrap();
        let link = fixture.root.path().join("result");
        let (code, output, diagnostic) = fixture.run(&[
            "build",
            "--json",
            "--out-link",
            link.to_str().unwrap(),
            ".#a",
        ]);
        assert_eq!(code, ExitCode::FAILURE);
        assert_eq!(output, [] as [u8; 0]);
        assert_eq!(diagnostic, [] as [u8; 0]);
        assert!(!link.is_symlink());
    }

    #[test]
    fn upstream_modes_retain_success_partial_unavailable_and_malformed_results() {
        let mut fixture = Fixture::new();
        for (mode, expected) in [
            ("normal", ExitCode::SUCCESS),
            ("partial", ExitCode::FAILURE),
            ("unavailable", ExitCode::FAILURE),
            ("malformed", ExitCode::SUCCESS),
        ] {
            fixture
                .environment
                .insert("WRIX_TEST_UPSTREAM", mode.into());
            let (code, output, diagnostic) = fixture.run(&[
                "path-info",
                "--store",
                "https://fixture.invalid",
                "--json",
                "/nix/store/a",
                "/nix/store/b",
            ]);
            assert_eq!(code, expected);
            assert_eq!(diagnostic, [] as [u8; 0]);
            match mode {
                "unavailable" => assert_eq!(output, [] as [u8; 0]),
                "malformed" => assert_eq!(output, b"invalid JSON\n"),
                _ => assert_eq!(
                    serde_json::from_slice::<Value>(&output).unwrap(),
                    json!({"/nix/store/a":{},"/nix/store/b":null})
                ),
            }
        }
    }

    #[test]
    fn hook_discovery_and_unexpected_commands_retain_exact_output_and_status() {
        let fixture = Fixture::new();
        assert_eq!(
            fixture.run(&["config", "show", "post-build-hook"]),
            (
                ExitCode::SUCCESS,
                b"/nix/store/fixture/bin/wrix-cache-post-build-hook\n".to_vec(),
                Vec::new()
            )
        );
        assert_eq!(
            fixture.run(&["unexpected"]),
            (
                ExitCode::from(2),
                Vec::new(),
                b"unexpected Nix invocation\n".to_vec()
            )
        );
    }
}
