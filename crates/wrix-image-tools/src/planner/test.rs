mod property;

use super::*;
use serde_json::json;

fn run(graph: Value, pipeline: Value, excluded: Value) -> Vec<Vec<String>> {
    let input = Value::Object(serde_json::Map::from_iter([
        ("graph".to_owned(), graph),
        ("pipeline".to_owned(), pipeline),
        ("exclude_paths".to_owned(), excluded),
    ]));
    plan_json(&serde_json::to_vec(&input).unwrap()).unwrap()
}

fn fixture() -> Value {
    json!([
        {"path":"A", "narSize":2, "closureSize":1, "references":["A","C"]},
        {"path":"B", "narSize":4, "closureSize":3, "references":["C","D"]},
        {"path":"C", "narSize":6, "closureSize":5, "references":["C"]},
        {"path":"D", "narSize":8, "closureSize":7, "references":["D"]},
    ])
}

fn expected(layers: &[&[&str]]) -> Vec<Vec<String>> {
    layers
        .iter()
        .map(|paths| paths.iter().map(|path| (*path).to_owned()).collect())
        .collect()
}

#[test]
fn upstream_flatten_split_paths_and_nested_over_fixtures_match() {
    assert_eq!(
        run(fixture(), json!([["split_paths", ["B"]]]), Value::Null),
        expected(&[&["B", "D"], &["C"], &["A"]])
    );
    assert_eq!(
        run(
            fixture(),
            json!([
                ["split_paths", ["B"]],
                ["over", "main", ["subcomponent_in", ["B"]]]
            ]),
            Value::Null
        ),
        expected(&[&["B"], &["D"], &["C"], &["A"]])
    );
}

#[test]
fn upstream_excluded_paths_fixtures_match() {
    assert_eq!(
        run(fixture(), json!([["split_paths", ["B"]]]), json!(["A"])),
        expected(&[&["B", "C", "D"]])
    );
    assert_eq!(
        run(fixture(), json!([["split_paths", ["B"]]]), json!(["D"])),
        expected(&[&["B"], &["C"], &["A"]])
    );
}

#[test]
fn documented_popularity_recurrence_and_layer_merge_match() {
    let nodes = [
        ("A", vec!["B", "G"]),
        ("B", vec!["C", "E"]),
        ("C", vec!["D", "E"]),
        ("D", vec!["F"]),
        ("E", vec!["F"]),
        ("F", vec![]),
        ("G", vec![]),
    ];
    let graph: Vec<_> = nodes
        .iter()
        .map(|(path, references)| json!({"path":path,"references":references,"narSize":1}))
        .collect();
    assert_eq!(
        run(json!(graph), json!([["popularity_contest"]]), Value::Null),
        expected(&[&["F"], &["E"], &["D"], &["C"], &["B"], &["G"], &["A"]])
    );
    assert_eq!(
        run(
            json!(graph),
            json!([["popularity_contest"], ["limit_layers", 2]]),
            Value::Null
        ),
        expected(&[&["F"], &["E", "D", "C", "B", "G", "A"]])
    );
}

#[test]
fn package_name_precedes_hash_for_popularity_ties() {
    let graph = json!([
        {"path":"/nix/store/aaaa-zed", "narSize":1,"references":[]},
        {"path":"/nix/store/zzzz-alpha", "narSize":1,"references":[]},
    ]);
    assert_eq!(
        run(graph, json!([["popularity_contest"]]), Value::Null),
        expected(&[&["/nix/store/zzzz-alpha"], &["/nix/store/aaaa-zed"]])
    );
}

#[test]
fn split_reverse_remove_and_reorder_keep_the_pipeline_contract() {
    assert_eq!(
        run(
            fixture(),
            json!([["split_every", 2], ["reverse"]]),
            Value::Null
        ),
        expected(&[&["C", "D"], &["A", "B"]])
    );
    assert_eq!(
        run(
            fixture(),
            json!([["remove_paths", "C"], ["split_every", 2]]),
            Value::Null
        ),
        expected(&[&["A", "B"], &["D"]])
    );
    assert_eq!(
        run(
            fixture(),
            json!([
                ["split_paths", ["B"]],
                ["reorder", ["missing", "rest", "common", "rest"]]
            ]),
            Value::Null
        ),
        expected(&[&["A"], &["C"], &["B", "D"]])
    );
}

#[test]
fn map_and_nested_pipe_apply_to_each_partition() {
    assert_eq!(
        run(
            fixture(),
            json!([
                ["split_every", 2],
                ["map", ["pipe", [["split_every", 1], ["reverse"]]]]
            ]),
            Value::Null
        ),
        expected(&[&["B"], &["A"], &["D"], &["C"]])
    );
}

#[test]
fn subcomponents_ignore_missing_names_and_preserve_rest() {
    assert_eq!(
        run(
            fixture(),
            json!([["subcomponent_out", ["A", "missing"]]]),
            Value::Null
        ),
        expected(&[&["A", "C"], &["B", "D"]])
    );
    assert_eq!(
        run(fixture(), json!([["subcomponent_in", ["C"]]]), Value::Null),
        expected(&[&["A", "B", "C"], &["D"]])
    );
    assert_eq!(
        run(
            fixture(),
            json!([["subcomponent_out", ["missing"]]]),
            Value::Null
        ),
        expected(&[&["A", "B", "C", "D"]])
    );
}

#[test]
fn children_of_split_specs_accept_single_and_multiple_parent_names() {
    let expected = run(fixture(), json!([["split_paths", ["C"]]]), Value::Null);
    assert_eq!(
        run(
            fixture(),
            json!([["split_paths",[{"children_of":"A"}]]]),
            Value::Null
        ),
        expected
    );
    assert_eq!(
        run(
            fixture(),
            json!([["split_paths",[{"children_of":["A","missing"]}]]]),
            Value::Null
        ),
        expected
    );
}

#[test]
fn empty_graphs_and_fully_excluded_graphs_emit_no_empty_layers() {
    assert_eq!(
        run(
            json!([]),
            json!([["popularity_contest"], ["limit_layers", 1]]),
            Value::Null
        ),
        Vec::<Vec<String>>::new()
    );
    assert_eq!(
        run(
            fixture(),
            json!([["popularity_contest"], ["limit_layers", 2]]),
            json!(["A", "B", "C", "D"])
        ),
        Vec::<Vec<String>>::new()
    );
}

#[test]
fn graph_valued_final_pipelines_match_upstream_shape_errors() {
    for pipeline in [
        json!([]),
        json!([["flatten"]]),
        json!([["remove_paths", []]]),
    ] {
        let input = json!({"graph": fixture(), "pipeline": pipeline});
        assert!(matches!(
            plan_json(&serde_json::to_vec(&input).unwrap()),
            Err(Error::Pipeline(pipeline::Error::Shape))
        ));
    }
}

#[test]
fn invalid_graphs_pipeline_shapes_and_counts_fail_closed() {
    for input in [
        json!({"graph":[],"pipeline":[["unknown"]]}),
        json!({"graph":[],"pipeline":[["limit_layers",0]]}),
        json!({"graph":[],"pipeline":[["split_every",0]]}),
        json!({"graph":[],"pipeline":[["popularity_contest",1]]}),
        json!({"graph":[],"pipeline":[["over","main",["popularity_contest"]]]}),
        json!({"graph":[{"path":"A","narSize":1,"references":["missing"]}],"pipeline":[]}),
        json!({"graph":[{"path":"A","narSize":1,"references":[]},{"path":"A","narSize":1,"references":[]}],"pipeline":[]}),
    ] {
        assert!(
            plan_json(&serde_json::to_vec(&input).unwrap()).is_err(),
            "{input}"
        );
    }
}
