//! System-boundary differential verifier; not installed in image build tools.

use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    env,
    ffi::OsString,
    fs,
    io::{self, Write},
    path::Path,
    process::{Command, Output},
    time::Instant,
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
enum Error {
    /// usage: planner-differential custom|auto `REFERENCE_EXECUTABLE` `NATIVE_EXECUTABLE`
    Usage,
    /// differential verifier requires PATH
    Environment,
    /// differential verifier I/O failed: {0}
    Io(#[from] io::Error),
    /// differential verifier JSON failed: {0}
    Json(#[from] serde_json::Error),
    /// layer planner outputs differ: {0}
    Difference(String),
    /// differential case {ordinal} failed: {source}
    Case { ordinal: usize, source: Box<Self> },
}

#[derive(Clone, Copy)]
enum Mode {
    Custom,
    Auto,
}

fn path(index: usize) -> String {
    format!("hash{index:03}-P{index:03}")
}

fn graph(seed: u8) -> Value {
    let count = usize::from(seed % 14);
    let mut state = u64::from(seed);
    let mut graph = Vec::new();
    for source in 0..count {
        let mut references = vec![path(source)];
        for target in (source + 1)..count {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            if state.is_multiple_of(4) {
                references.push(path(target));
            }
        }
        graph
            .push(json!({"path":path(source), "narSize":state % 200 + 1, "references":references}));
    }
    json!(graph)
}

fn pipelines() -> Vec<Value> {
    let first = path(0);
    vec![
        json!([["popularity_contest"], ["limit_layers", 1]]),
        json!([["popularity_contest"], ["limit_layers", 3]]),
        json!([["popularity_contest"], ["limit_layers", 127]]),
        json!([["split_every", 3]]),
        json!([["split_every", 2], ["reverse"]]),
        json!([
            ["subcomponent_out", [first, "missing"]],
            ["flatten"],
            ["map", ["popularity_contest"]],
            ["flatten"],
            ["limit_layers", 4]
        ]),
        json!([
            ["split_paths", [first]],
            [
                "over",
                "rest",
                ["pipe", [["popularity_contest"], ["limit_layers", 3]]]
            ],
            ["reorder", ["rest", "common", "main"]],
            ["flatten"],
            ["map", ["split_every", 2]],
            ["flatten"],
            ["limit_layers", 7]
        ]),
        json!([["split_paths",[{"children_of":first}]],["flatten"],["map",["popularity_contest"]],["flatten"],["limit_layers",4]]),
        json!([
            ["remove_paths", path(1)],
            ["popularity_contest"],
            ["limit_layers", 3]
        ]),
        json!([
            ["subcomponent_in", [first]],
            [
                "over",
                "main",
                ["pipe", [["popularity_contest"], ["limit_layers", 2]]]
            ],
            ["reorder", ["rest", "main"]],
            ["flatten"],
            ["limit_layers", 4]
        ]),
        json!([
            ["popularity_contest"],
            ["reorder", ["missing"]],
            ["limit_layers", 4]
        ]),
        json!([["pipe", [["popularity_contest"], ["limit_layers", 2]]]]),
    ]
}

fn invoke(executable: &OsString, arguments: &[OsString], home: &Path) -> Result<Output, Error> {
    Ok(Command::new(executable)
        .args(arguments)
        .env_clear()
        .env("PATH", env::var_os("PATH").ok_or(Error::Environment)?)
        .env("HOME", home)
        .env("TMPDIR", home)
        .output()?)
}

fn compare(reference: &Output, native: &Output, mode: Mode) -> Result<(), Error> {
    if !reference.status.success() || !native.status.success() {
        return Err(Error::Difference(format!(
            "reference {}: {}; native {}: {}",
            reference.status,
            String::from_utf8_lossy(&reference.stderr),
            native.status,
            String::from_utf8_lossy(&native.stderr)
        )));
    }
    let mut expected: Vec<Vec<String>> = serde_json::from_slice(&reference.stdout)?;
    let mut actual: Vec<Vec<String>> = serde_json::from_slice(&native.stdout)?;
    if matches!(mode, Mode::Auto) {
        // Upstream explicitly declares default outer layer order nonsemantic;
        // its equal-closure-size hash-set ties are nondeterministic. Compare
        // partitions exactly, including path order within each layer. Native
        // closure-size ordering and canonical ties have separate Rust tests.
        expected.sort();
        actual.sort();
    }
    if expected != actual {
        return Err(Error::Difference(format!(
            "reference {expected:?}; native {actual:?}"
        )));
    }
    let paths: Vec<_> = actual.iter().flatten().collect();
    if paths.iter().copied().collect::<BTreeSet<_>>().len() != paths.len() {
        return Err(Error::Difference(
            "duplicate path in native output".to_owned(),
        ));
    }
    if actual.iter().any(Vec::is_empty) {
        return Err(Error::Difference("empty native layer".to_owned()));
    }
    Ok(())
}

fn run() -> Result<(), Error> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    let [mode, reference, native] = args.as_slice() else {
        return Err(Error::Usage);
    };
    let mode = match mode.to_str() {
        Some("custom") => Mode::Custom,
        Some("auto") => Mode::Auto,
        _ => return Err(Error::Usage),
    };
    let temporary = tempfile::tempdir()?;
    let input = temporary.path().join("input.json");
    let ignored = temporary.path().join("ignored");
    let started = Instant::now();
    let mut cases = 0;
    for seed in 0..32_u8 {
        let graph = graph(seed);
        let excluded = if seed.is_multiple_of(3) {
            vec![path(1)]
        } else {
            Vec::new()
        };
        let variants = match mode {
            Mode::Custom => pipelines(),
            Mode::Auto => vec![json!(1), json!(3), json!(127)],
        };
        for variant in variants {
            let arguments = match mode {
                Mode::Custom => {
                    fs::write(
                        &input,
                        serde_json::to_vec(
                            &json!({"graph":graph,"pipeline":variant,"exclude_paths":excluded}),
                        )?,
                    )?;
                    vec![input.clone().into_os_string()]
                }
                Mode::Auto => {
                    fs::write(&input, serde_json::to_vec(&graph)?)?;
                    fs::write(&ignored, excluded.join("\n"))?;
                    vec![
                        input.clone().into_os_string(),
                        ignored.clone().into_os_string(),
                        variant.to_string().into(),
                    ]
                }
            };
            let left = invoke(reference, &arguments, temporary.path())?;
            let right = invoke(native, &arguments, temporary.path())?;
            compare(&left, &right, mode).map_err(|source| Error::Case {
                ordinal: cases,
                source: Box::new(source),
            })?;
            cases += 1;
        }
    }
    serde_json::to_writer(
        io::stdout().lock(),
        &json!({"cases":cases,"elapsed_ms":started.elapsed().as_millis()}),
    )?;
    writeln!(io::stdout().lock())?;
    Ok(())
}

struct Failure(Error);
impl std::fmt::Debug for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, formatter)
    }
}
fn main() -> Result<(), Failure> {
    run().map_err(Failure)
}

#[cfg(test)]
mod test {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    fn output(bytes: &[u8], status: i32) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(status),
            stdout: bytes.to_vec(),
            stderr: Vec::new(),
        }
    }
    #[test]
    fn differential_verifier_rejects_missing_duplicate_reordered_and_failed_outputs() {
        let expected = output(br#"[["A"],["B"]]"#, 0);
        for actual in [
            output(br#"[["A"]]"#, 0),
            output(br#"[["A"],["A"]]"#, 0),
            output(br#"[["B"],["A"]]"#, 0),
            output(b"[]", 256),
        ] {
            assert!(compare(&expected, &actual, Mode::Custom).is_err());
        }
        assert!(compare(&expected, &expected, Mode::Custom).is_ok());
    }
    #[test]
    fn default_comparison_ignores_only_outer_layer_order_not_partition_membership() {
        let expected = output(br#"[["A"],["B"]]"#, 0);
        assert!(compare(&expected, &output(br#"[["B"],["A"]]"#, 0), Mode::Auto).is_ok());
        assert!(compare(&expected, &output(br#"[["A","B"]]"#, 0), Mode::Auto).is_err());
    }
}
