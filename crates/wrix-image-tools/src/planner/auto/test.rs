use super::*;
use serde_json::{Value, json};

fn run(graph: &Value, ignored: &[&str], limit: usize) -> Vec<Vec<String>> {
    plan_json(
        &serde_json::to_vec(graph).unwrap(),
        &ignored.iter().map(|path| (*path).to_owned()).collect(),
        NonZeroUsize::new(limit).unwrap(),
    )
    .unwrap()
}
fn fixture() -> Value {
    json!([
        {"path":"hash-A","narSize":10,"references":["hash-C"]},
        {"path":"hash-B","narSize":20,"references":["hash-C"]},
        {"path":"hash-C","narSize":1,"references":["hash-C"]},
    ])
}
fn expected(layers: &[&[&str]]) -> Vec<Vec<String>> {
    layers
        .iter()
        .map(|layer| layer.iter().map(|path| (*path).to_owned()).collect())
        .collect()
}

#[test]
fn default_algorithm_is_size_based_not_custom_popularity_ranking() {
    assert_eq!(
        run(&fixture(), &[], 1),
        expected(&[&["hash-A", "hash-B", "hash-C"]])
    );
    assert_eq!(
        run(&fixture(), &[], 2),
        expected(&[&["hash-A"], &["hash-B", "hash-C"]])
    );
    assert_eq!(
        run(&fixture(), &[], 3),
        expected(&[&["hash-C"], &["hash-A"], &["hash-B"]])
    );
}
#[test]
fn ignored_paths_are_filtered_after_dependency_assignment() {
    assert_eq!(
        run(&fixture(), &["hash-C"], 3),
        expected(&[&["hash-A"], &["hash-B"]])
    );
    assert_eq!(
        run(&fixture(), &["hash-A"], 2),
        expected(&[&["hash-B", "hash-C"]])
    );
    assert_eq!(
        run(&fixture(), &["hash-A", "hash-B", "hash-C"], 2),
        Vec::<Vec<String>>::new()
    );
}
#[test]
fn default_path_sort_uses_name_then_hash_as_a_tuple() {
    let graph = json!([
        {"path":"aaaa-a-z","narSize":1,"references":[]},
        {"path":"zzzz-a","narSize":1,"references":[]},
    ]);
    assert_eq!(run(&graph, &[], 1), expected(&[&["zzzz-a", "aaaa-a-z"]]));
}
#[test]
fn default_empty_graph_and_rootless_cycles_remain_complete() {
    assert_eq!(run(&json!([]), &[], 1), Vec::<Vec<String>>::new());
    let graph = json!([
        {"path":"hash-A","narSize":10,"references":["hash-B"]},
        {"path":"hash-B","narSize":20,"references":["hash-A"]},
    ]);
    for limit in 1..5 {
        assert_eq!(run(&graph, &[], limit), expected(&[&["hash-A", "hash-B"]]));
    }
}
#[test]
fn closure_size_order_does_not_overflow_u64() {
    let graph = json!([
        {"path":"hash-A","narSize":u64::MAX,"references":[]},
        {"path":"hash-B","narSize":u64::MAX,"references":["hash-A"]},
    ]);
    assert_eq!(run(&graph, &[], 2), expected(&[&["hash-A"], &["hash-B"]]));
}
#[test]
fn equal_closure_size_layer_order_is_canonical() {
    let graph = json!([
        {"path":"hash-B","narSize":1,"references":[]},
        {"path":"hash-A","narSize":1,"references":[]},
    ]);
    assert_eq!(run(&graph, &[], 2), expected(&[&["hash-A"], &["hash-B"]]));
}
