use super::*;

fn a() -> String {
    format!("/nix/store/{}-configuration.js", "a".repeat(32))
}
fn b() -> String {
    format!("/nix/store/{}-tool?native-name", "b".repeat(32))
}
fn paths(values: &[String]) -> Vec<StorePath> {
    values
        .iter()
        .cloned()
        .map(|value| StorePath::parse(value).unwrap())
        .collect()
}
fn run(candidates: &[String], closure: &[String]) -> Result<Vec<String>, Error> {
    resolve(&paths(candidates), &paths(closure).into_iter().collect())
        .map(|values| values.into_iter().map(|path| path.0).collect())
}

// Ported from test_configuration_roots.py: all six original scenarios remain.
#[test]
fn exact_sorted_unique_roots() {
    assert_eq!(run(&[b(), a(), a()], &[b(), a()]).unwrap(), [a(), b()]);
}
#[test]
fn annotation_uses_native_closure_name() {
    assert_eq!(
        run(&[format!("{}?annotation", a())], &[a()]).unwrap(),
        [a()]
    );
}
#[test]
fn question_mark_in_native_name_is_preserved() {
    assert_eq!(run(&[b()], &[b()]).unwrap(), [b()]);
}
#[test]
fn absent_literal_is_not_implicit_authority() {
    assert!(matches!(run(&[a()], &[b()]), Err(Error::OutsideClosure(_))));
}
#[test]
fn no_closure_or_data_paths_are_silently_selected() {
    assert_eq!(run(&[], &[a(), b()]).unwrap(), Vec::<String>::new());
}
#[test]
fn malformed_tokens_and_graphs_fail() {
    assert!(StorePath::parse(format!("{}/../peer", a())).is_err());
    assert!(parse_closure("/tmp/peer").is_err());
    assert!(parse_references(b"{}").is_err());
    assert!(parse_references(b"[null]").is_err());
}
#[test]
fn longest_native_prefix_wins_without_name_guessing() {
    let native = format!("{}?native", a());
    assert_eq!(
        run(&[format!("{native}?annotation")], &[a(), native.clone()]).unwrap(),
        [native]
    );
}
#[test]
fn store_path_grammar_is_complete_and_ascii_only() {
    for path in [
        "/tmp/not-a-store-root".to_owned(),
        format!("/nix/store/{}-name", "e".repeat(32)),
        format!("/nix/store/{}-", "a".repeat(32)),
        format!("{}/file", a()),
        format!("{}λ", a()),
        format!("{}\n", a()),
    ] {
        assert!(StorePath::parse(path).is_err());
    }
    assert!(StorePath::parse(format!("/nix/store/{}-aZ09+._?=-", "0".repeat(32))).is_ok());
}
#[test]
fn publication_is_sorted_deduplicated_and_never_overwrites() {
    let output = tempfile::tempdir().unwrap();
    let closure = paths(&[a(), b()]).into_iter().collect();
    publish(&paths(&[b(), a(), a()]), &closure, &[], output.path()).unwrap();
    for path in paths(&[a(), b()]) {
        let link = output
            .path()
            .join("share/wrix/image-inputs")
            .join(path.as_path().file_name().unwrap());
        assert_eq!(fs::read_link(link).unwrap(), path.as_path());
    }
    assert!(publish(&[], &closure, &[], output.path()).is_err());
}
#[test]
fn missing_native_sources_fail_before_publication() {
    let output = tempfile::tempdir().unwrap();
    assert!(matches!(
        publish(&[], &BTreeSet::new(), &paths(&[a()]), output.path()),
        Err(Error::Io(_))
    ));
    assert!(!output.path().join("share/wrix/image-inputs").exists());
}
