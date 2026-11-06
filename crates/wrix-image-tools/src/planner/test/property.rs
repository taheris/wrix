use super::*;
use std::collections::BTreeSet;

fn generated(seed: u64, count: usize, cyclic: bool) -> Value {
    let mut state = seed;
    let mut nodes = Vec::new();
    for source in 0..count {
        let mut references = Vec::new();
        for target in 0..count {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            if (cyclic || source < target) && state.is_multiple_of(5) {
                references.push(format!("P{target:03}"));
            }
        }
        references.push(format!("P{source:03}"));
        nodes.push(
            json!({"path":format!("P{source:03}"), "narSize":source % 3, "references":references}),
        );
    }
    json!(nodes)
}

fn assert_membership(layers: &[Vec<String>], expected: &BTreeSet<String>, limit: usize) {
    assert!(layers.len() <= limit);
    assert!(layers.iter().all(|layer| !layer.is_empty()));
    let flattened: Vec<_> = layers.iter().flatten().cloned().collect();
    assert_eq!(flattened.len(), expected.len(), "duplicate or missing path");
    assert_eq!(flattened.into_iter().collect::<BTreeSet<_>>(), *expected);
}

#[test]
fn generated_dags_retain_all_paths_layer_bounds_exclusions_and_permutation_stability() {
    for seed in 0..256_u64 {
        let count = usize::try_from(seed % 14).unwrap();
        let graph = generated(seed, count, false);
        let limit = usize::try_from(seed % 8 + 1).unwrap();
        let pipeline = json!([["popularity_contest"], ["limit_layers", limit]]);
        let expected: BTreeSet<_> = (0..count).map(|index| format!("P{index:03}")).collect();
        let layers = run(graph.clone(), pipeline.clone(), Value::Null);
        assert_membership(&layers, &expected, limit);
        let mut reversed = graph.as_array().unwrap().clone();
        reversed.reverse();
        assert_eq!(
            run(json!(reversed), pipeline.clone(), Value::Null),
            layers,
            "seed {seed}"
        );
        let excluded: Vec<_> = expected
            .iter()
            .enumerate()
            .filter(|(index, _)| index % 3 == 0)
            .map(|(_, path)| path.clone())
            .collect();
        let retained = expected
            .difference(&excluded.iter().cloned().collect())
            .cloned()
            .collect();
        assert_membership(&run(graph, pipeline, json!(excluded)), &retained, limit);
    }
}

#[test]
fn generated_cyclic_and_rootless_graphs_retain_paths_deterministically_in_all_partitions() {
    for seed in 0..128_u64 {
        let count = usize::try_from(seed % 12).unwrap();
        let graph = generated(seed, count, true);
        let expected = (0..count).map(|index| format!("P{index:03}")).collect();
        let pipeline = json!([["popularity_contest"], ["limit_layers", 3]]);
        let layers = run(graph.clone(), pipeline.clone(), Value::Null);
        assert_membership(&layers, &expected, 3);
        let mut reversed = graph.as_array().unwrap().clone();
        reversed.reverse();
        assert_eq!(
            run(json!(reversed), pipeline, Value::Null),
            layers,
            "seed {seed}"
        );
        let partitioned = run(graph, json!([["split_paths", ["P000"]]]), Value::Null);
        assert_membership(&partitioned, &expected, 3);
    }
}

#[test]
fn dense_dag_counts_exceeding_fixed_integer_width_do_not_collapse_popularity_ties() {
    let count = 150;
    let graph: Vec<_> = (0..count).map(|source| json!({"path":format!("P{source:03}"), "narSize":1, "references": ((source+1)..count).map(|target| format!("P{target:03}")).collect::<Vec<_>>()})).collect();
    let layers = run(json!(graph), json!([["popularity_contest"]]), Value::Null);
    let expected: Vec<_> = (0..count)
        .rev()
        .map(|index| vec![format!("P{index:03}")])
        .collect();
    assert_eq!(layers, expected);
}
