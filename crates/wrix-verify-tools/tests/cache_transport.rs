#[cfg(test)]
#[path = "../src/cache_transport/fixture/mod.rs"]
mod fixture;
#[cfg(test)]
mod test {
    use super::fixture;
    use std::{ffi::OsString, fs, path::PathBuf, process::Command};
    const TOOL: &str = env!("CARGO_BIN_EXE_wrix-verify-tools");
    const PATHS: [&str; 4] = [
        "crates/wrix-sandbox/src/command/launch.rs",
        "crates/wrix-service/src/lifecycle/owner/host.rs",
        "crates/wrix-service/src/dolt/server/mod.rs",
        "lib/services/image.nix",
    ];
    fn quote(value: &str) -> String {
        format!(
            "'{}'",
            value
                .replace('\\', "\\\\")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t")
        )
    }
    fn check(sources: [String; 4], diagnostic: Option<String>) {
        let root = tempfile::Builder::new()
            .prefix("wrix cache ' transport ")
            .tempdir()
            .unwrap();
        for (name, source) in PATHS.into_iter().zip(sources) {
            let path = root.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, source).unwrap();
        }
        let result = Command::new(TOOL)
            .arg("cache-transport")
            .arg(root.path())
            .output()
            .unwrap();
        assert_eq!(
            result.status.code(),
            Some(i32::from(diagnostic.is_some())),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(result.stdout, b"");
        assert_eq!(
            result.stderr,
            diagnostic
                .map_or_else(String::new, |value| format!("FAIL: {value}\n"))
                .as_bytes()
        );
        match (
            std::env::var_os("WRIX_TEST_ORIGINAL_CACHE_PYTHON"),
            std::env::var_os("WRIX_TEST_ORIGINAL_CACHE_SCRIPT"),
        ) {
            (None, None) => {}
            (Some(python), Some(script)) => {
                let original = Command::new(python)
                    .arg("-I")
                    .arg(script)
                    .arg(root.path())
                    .output()
                    .unwrap();
                assert_eq!(
                    (original.status.code(), original.stdout, original.stderr),
                    (result.status.code(), result.stdout, result.stderr)
                );
            }
            _ => panic!("both original oracle inputs must be explicitly supplied together"),
        }
    }
    #[test]
    fn original_http_fixture_accepts_with_exact_empty_standard_streams() {
        check(fixture::source(), None);
    }
    #[test]
    fn every_original_required_token_has_exact_native_cli_failure_semantics() {
        let cases = [
            (
                0,
                "project-cache launcher path",
                "format!(\"http://{sandbox_host}:{}\", endpoint.port)",
            ),
            (
                0,
                "project-cache launcher path",
                "extra-substituters = {url}",
            ),
            (
                0,
                "project-cache launcher path",
                "extra-trusted-public-keys = {}",
            ),
            (0, "project-cache launcher path", "public_key.as_str()"),
            (
                0,
                "project-cache launcher path",
                "builders-use-substitutes = true",
            ),
            (
                1,
                "service container run path",
                "\"127.0.0.1:{}:8080\",\n            shared.cache_port(selected.port)",
            ),
            (
                1,
                "service container run path",
                "let layout = Layout::current()?",
            ),
            (
                1,
                "service container run path",
                "for mount in layout.mounts()",
            ),
            (
                1,
                "service container run path",
                ".arg(\"wrix-shared-service\")",
            ),
            (
                2,
                "service container command",
                "wrix_cache::backend::layout::runtime::COMMAND",
            ),
            (3, "service image contents", "celler"),
        ];
        for (index, label, needle) in cases {
            let mut source = fixture::source();
            source[index] = source[index].replace(needle, "");
            check(source, Some(format!("{label}: missing {}", quote(needle))));
        }
    }
    #[test]
    fn all_original_forbidden_transport_mutations_have_identical_cli_diagnostics() {
        let regions = [
            (
                0,
                "project-cache launcher path",
                "    fn load_project_cache() {",
            ),
            (
                1,
                "service container run path",
                "pub(in crate::lifecycle) fn ensure() {",
            ),
            (2, "service container command", "    let mut celler ="),
            (3, "service image contents", "  stableContents = ["),
            (3, "service image contents", "  contents = stableContents"),
            (3, "service image contents", "  rawImage = {"),
        ];
        for (index, label, marker) in regions {
            for needle in [
                "file://",
                "unix://",
                "nix-daemon",
                "/nix/var/nix/daemon-socket",
                ":/nix/store",
                "harmonia",
                "nix-serve",
            ] {
                let mut source = fixture::source();
                source[index] = source[index].replace(marker, &format!("{marker}\n\"{needle}\""));
                check(
                    source,
                    Some(format!("{label}: unexpected {}", quote(needle))),
                );
            }
        }
    }
    #[test]
    fn missing_original_region_boundaries_fail_closed_through_the_cli() {
        let regions = [
            (
                0,
                "project-cache launcher path",
                "    fn load_project_cache",
                "    fn load_dolt",
            ),
            (
                1,
                "service container run path",
                "pub(in crate::lifecycle) fn ensure(",
                "\npub(in crate::lifecycle) fn sql_ready(",
            ),
            (
                2,
                "service container command",
                "    let mut celler =",
                "    let mut terminate =",
            ),
            (
                3,
                "service image contents",
                "  stableContents = [",
                "  sourceKind =",
            ),
        ];
        for (index, label, start, end) in regions {
            for (position, needle) in [("start", start), ("end", end)] {
                let mut source = fixture::source();
                source[index] = source[index].replace(needle, "");
                check(
                    source,
                    Some(format!(
                        "{label}: missing {position} marker {}",
                        quote(needle)
                    )),
                );
            }
        }
    }
    #[test]
    fn wrong_arguments_never_start_reading_an_unselected_source_tree() {
        for args in [
            vec![OsString::from("cache-transport")],
            vec![
                "cache-transport".into(),
                PathBuf::from("/missing-source").into_os_string(),
                "extra".into(),
            ],
        ] {
            assert!(
                !Command::new(TOOL)
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
    }
}
