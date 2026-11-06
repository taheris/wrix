//! dockerTools' size-based default auto-layer algorithm, separate from the
//! custom pipeline's popularity ranking. Equal-closure-size ties are canonical.

use super::graph::{self, Graph};
use petgraph::{Direction, graph::NodeIndex};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    io::{self, Write},
    num::NonZeroUsize,
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// usage: wrix-auto-layer `GRAPH_JSON` `IGNORE_FILE` `POSITIVE_LAYER_LIMIT`
    Usage,
    /// invalid auto-layer limit
    Limit,
    /// default auto-layer path has no hash/name separator: {0}
    Path(String),
    /// auto-layer I/O failed: {0}
    Io(#[from] io::Error),
    /// invalid auto-layer graph JSON: {0}
    Json(#[from] serde_json::Error),
    /// {0}
    Graph(#[from] graph::Error),
}

fn path_key(path: &str) -> Result<(String, String), Error> {
    path.split_once('-')
        .map(|(hash, name)| (name.to_owned(), hash.to_owned()))
        .ok_or_else(|| Error::Path(path.to_owned()))
}

pub fn plan_json(
    bytes: &[u8],
    ignored: &BTreeSet<String>,
    limit: NonZeroUsize,
) -> Result<Vec<Vec<String>>, Error> {
    // Unlike a custom pipeline, the default algorithm excludes paths *after*
    // splitting: inherited paths still participate in dependency assignment.
    let raw: Vec<graph::RawNode> = serde_json::from_slice(bytes)?;
    let graph = Graph::parse(raw, &[])?;
    let keys = graph
        .inner
        .node_weights()
        .map(|node| path_key(node.name.as_str()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut assignment = vec![BTreeSet::new(); graph.inner.node_count()];
    // Graph construction stably sorts ascending by narSize, as upstream does.
    for primary in graph.inner.node_indices().rev() {
        let mut next = assignment.clone();
        next[primary.index()] = BTreeSet::from([primary]);
        let mut dependencies = graph.reachable([primary], Direction::Outgoing);
        dependencies.remove(&primary);
        let mut users = graph.reachable([primary], Direction::Incoming);
        users.remove(&primary);
        for &dependency in &dependencies {
            next[dependency.index()].retain(|index| !users.contains(index));
            if next[dependency.index()].is_disjoint(&dependencies) {
                next[dependency.index()].insert(primary);
            }
        }
        let groups: BTreeSet<_> = next.iter().collect();
        if groups.len() > limit.get() {
            break;
        }
        assignment = next;
    }
    let mut groups: BTreeMap<BTreeSet<NodeIndex>, Vec<NodeIndex>> = BTreeMap::new();
    for index in graph.inner.node_indices() {
        groups
            .entry(assignment[index.index()].clone())
            .or_default()
            .push(index);
    }
    let mut layers = Vec::new();
    for mut paths in groups.into_values() {
        paths.sort_by_key(|index| &keys[index.index()]);
        let closure = graph.reachable(paths.iter().copied(), Direction::Outgoing);
        // NodeIndex is u32 and each narSize is u64, so a closure sum fits u128.
        let closure_size: u128 = closure
            .iter()
            .map(|&index| u128::from(graph.inner[index].nar_size))
            .sum();
        let signature: Vec<_> = paths.iter().map(|index| &keys[index.index()]).collect();
        layers.push((closure_size, signature, paths));
    }
    // Upstream's hash-set iteration makes equal-size ordering nondeterministic.
    // Canonical path signatures preserve its ordering rule while fixing ties.
    layers.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    Ok(layers
        .into_iter()
        .map(|(_, _, paths)| {
            paths
                .into_iter()
                .map(|index| graph.inner[index].name.as_str().to_owned())
                .filter(|path| !ignored.contains(path))
                .collect::<Vec<_>>()
        })
        .filter(|paths| !paths.is_empty())
        .collect())
}

pub fn run(args: impl IntoIterator<Item = OsString>) -> Result<(), Error> {
    let args: Vec<_> = args.into_iter().collect();
    let [graph, ignore, limit] = args.as_slice() else {
        return Err(Error::Usage);
    };
    let limit = limit
        .to_str()
        .ok_or(Error::Limit)?
        .parse::<usize>()
        .map_err(|_| Error::Limit)?;
    let limit = NonZeroUsize::new(limit).ok_or(Error::Limit)?;
    let ignored = fs::read_to_string(ignore)?
        .lines()
        .map(str::trim)
        .map(str::to_owned)
        .collect();
    let layers = plan_json(&fs::read(graph)?, &ignored, limit)?;
    let mut output = io::stdout().lock();
    serde_json::to_writer(&mut output, &layers)?;
    writeln!(output)?;
    Ok(())
}

#[cfg(test)]
mod test;
