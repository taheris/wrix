use super::*;

fn recipe(name: &str) -> Recipe {
    Recipe::parse(&format!(
        "/nix/store/00000000000000000000000000000000-{name}.drv"
    ))
    .unwrap()
}

fn output(name: &str) -> StorePath {
    StorePath::parse(&format!(
        "/nix/store/00000000000000000000000000000000-{name}"
    ))
    .unwrap()
}

#[test]
fn completed_outputs_stop_traversal_before_obsolete_build_inputs() {
    let leaf = recipe("leaf");
    let root = recipe("root");
    let leaf_output = output("leaf");
    let root_output = output("root");
    let source = output("source");
    let graph = Graph(BTreeMap::from([
        (
            root.clone(),
            Node {
                inputs: BTreeSet::from([leaf.clone()]),
                sources: BTreeSet::from([source.clone()]),
                outputs: BTreeSet::from([root_output.clone()]),
            },
        ),
        (
            leaf,
            Node {
                inputs: BTreeSet::new(),
                sources: BTreeSet::new(),
                outputs: BTreeSet::from([leaf_output.clone()]),
            },
        ),
    ]));
    let mut queries = 0;
    let found = graph
        .frontier(BTreeSet::from([root.clone()]), |paths| {
            queries += 1;
            Ok(paths.clone())
        })
        .unwrap();
    assert_eq!(queries, 1);
    assert_eq!(found, BTreeSet::from([root_output.clone()]));
    let found = graph
        .frontier(BTreeSet::from([root]), |paths| {
            Ok(paths
                .iter()
                .filter(|path| **path != root_output)
                .cloned()
                .collect())
        })
        .unwrap();
    assert_eq!(found, BTreeSet::from([leaf_output, source]));
}

#[test]
fn shared_dependencies_are_queried_once_and_partial_output_sets_remain_protected() {
    let leaf = recipe("leaf");
    let first = recipe("first");
    let second = recipe("second");
    let present = output("present");
    let missing = output("missing");
    let source = output("source");
    let make = || Node {
        inputs: BTreeSet::from([leaf.clone()]),
        sources: BTreeSet::from([source.clone()]),
        outputs: BTreeSet::from([present.clone(), missing.clone()]),
    };
    let graph = Graph(BTreeMap::from([
        (first.clone(), make()),
        (second.clone(), make()),
        (
            leaf.clone(),
            Node {
                inputs: BTreeSet::new(),
                sources: BTreeSet::new(),
                outputs: BTreeSet::from([output("leaf")]),
            },
        ),
    ]));
    let mut queries = Vec::new();
    let found = graph
        .frontier(BTreeSet::from([first, second]), |paths| {
            queries.push(paths.clone());
            Ok(paths
                .iter()
                .filter(|path| **path != missing)
                .cloned()
                .collect())
        })
        .unwrap();
    assert_eq!(queries.len(), 2);
    assert_eq!(found, BTreeSet::from([present, source, output("leaf")]));
    assert!(
        graph
            .frontier(BTreeSet::from([recipe("foreign")]), |paths| Ok(
                paths.clone()
            ))
            .is_err()
    );
}

#[test]
fn malformed_graphs_and_dynamic_outputs_refuse_before_validity_queries() {
    let temporary = tempfile::tempdir().unwrap();
    let graph = temporary.path().join("graph");
    let outputs = temporary.path().join("outputs");
    let key = recipe("root")
        .0
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let value = serde_json::json!({"version": 4, "derivations": { (&key): { "inputs": {"drvs": {}, "srcs": []} } }});
    std::fs::write(&graph, serde_json::to_vec(&value).unwrap()).unwrap();
    std::fs::write(
        &outputs,
        serde_json::to_vec(&serde_json::json!({ (&key): [output("root").0] })).unwrap(),
    )
    .unwrap();
    assert!(Graph::load(&graph, &outputs).is_ok());
    for invalid in [
        serde_json::json!({"version": 5, "derivations": {}}),
        serde_json::json!({"version": 4, "derivations": { (&key): { "inputs": {"drvs": { (&key): {"dynamicOutputs": {"out": {}}} }, "srcs": []} } }}),
        serde_json::json!({"version": 4, "derivations": { (&key): { "inputs": {"drvs": {}, "srcs": ["/tmp/foreign"]} } }}),
    ] {
        std::fs::write(&graph, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(Graph::load(&graph, &outputs).is_err());
    }
    assert!(Recipe::parse("/tmp/foreign.drv").is_err());
    assert!(Recipe::parse("/nix/store/00000000000000000000000000000000-not-a-recipe").is_err());
}
