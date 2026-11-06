use super::graph::Graph;
use num_bigint::BigUint;
use petgraph::{
    Direction,
    algo::{condensation, toposort},
};
use std::collections::BTreeMap;

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// invalid layer graph: strongly connected component condensation remained cyclic
    CondensationCycle,
    /// invalid layer graph: a path was absent from its component map
    MissingComponent,
}

fn package_name(path: &str) -> String {
    path.split_once('-')
        .map_or_else(|| path.to_owned(), |(hash, name)| format!("{name}-{hash}"))
}

/// Match upstream's memoized popularity recurrence without retaining quadratic
/// dictionaries of descendants. Each target propagates counts toward roots;
/// arbitrary precision preserves ordering even on exponentially branching DAGs.
/// Cycles are ranked as strongly connected components, then by package name.
pub(super) fn popularity(graph: &Graph) -> Result<Vec<Graph>, Error> {
    let condensed = condensation(graph.inner.clone(), true);
    let order = toposort(&condensed, None).map_err(|_| Error::CondensationCycle)?;
    let roots: Vec<_> = condensed.externals(Direction::Incoming).collect();
    let mut scores = vec![BigUint::default(); condensed.node_count()];
    for target in condensed.node_indices() {
        let mut counts = vec![BigUint::default(); condensed.node_count()];
        counts[target.index()] = BigUint::from(1_u8);
        for &index in order.iter().rev() {
            if counts[index.index()] == BigUint::default() {
                continue;
            }
            if index != target {
                counts[index.index()] += 1_u8;
            }
            let count = counts[index.index()].clone();
            for parent in condensed.neighbors_directed(index, Direction::Incoming) {
                counts[parent.index()] += &count;
            }
        }
        scores[target.index()] = roots.iter().map(|index| &counts[index.index()]).sum();
    }
    let component: BTreeMap<_, _> = condensed
        .node_indices()
        .flat_map(|index| {
            condensed[index]
                .iter()
                .map(move |node| (node.name.clone(), index))
        })
        .collect();
    let mut ranked = graph
        .inner
        .node_weights()
        .map(|node| {
            let index = component.get(&node.name).ok_or(Error::MissingComponent)?;
            Ok((
                scores[index.index()].clone(),
                package_name(node.name.as_str()),
                node.clone(),
            ))
        })
        .collect::<Result<Vec<_>, Error>>()?;
    ranked.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    Ok(ranked
        .into_iter()
        .map(|(_, _, node)| Graph::singleton(node))
        .collect())
}
