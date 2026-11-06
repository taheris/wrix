use petgraph::{Directed, Direction, Graph as DirectedGraph, graph::NodeIndex, visit::EdgeRef};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// layer graph contains an empty or NUL-containing path
    InvalidName,
    /// layer graph contains duplicate path: {0}
    Duplicate(String),
    /// layer graph path {path} references missing path {reference}
    MissingReference { path: String, reference: String },
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Name(String);

impl Name {
    fn parse(value: String) -> Result<Self, Error> {
        if value.is_empty() || value.contains('\0') {
            return Err(Error::InvalidName);
        }
        Ok(Self(value))
    }
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Deserialize)]
pub(super) struct RawNode {
    path: String,
    #[serde(rename = "narSize")]
    nar_size: u64,
    references: Vec<String>,
}

#[derive(Clone, Debug)]
pub(super) struct Node {
    pub(super) name: Name,
    pub(super) nar_size: u64,
}

#[derive(Clone, Debug)]
pub(super) struct Graph {
    pub(super) inner: DirectedGraph<Node, (), Directed>,
}

impl Graph {
    pub(super) fn empty() -> Self {
        Self {
            inner: DirectedGraph::new(),
        }
    }

    pub(super) fn parse(mut raw: Vec<RawNode>, excluded: &[String]) -> Result<Self, Error> {
        let excluded: BTreeSet<_> = excluded.iter().map(String::as_str).collect();
        raw.retain(|node| !excluded.contains(node.path.as_str()));
        // igraph's input conversion uses a stable ascending narSize sort.
        raw.sort_by_key(|node| node.nar_size);
        let mut result = Self::empty();
        let mut lookup = BTreeMap::new();
        let mut references = Vec::new();
        for node in raw {
            let name = Name::parse(node.path)?;
            if lookup.contains_key(&name) {
                return Err(Error::Duplicate(name.0));
            }
            let index = result.inner.add_node(Node {
                name: name.clone(),
                nar_size: node.nar_size,
            });
            lookup.insert(name, index);
            references.push((index, node.references));
        }
        for (source, targets) in references {
            let mut selected = BTreeSet::new();
            for target in targets {
                if excluded.contains(target.as_str()) {
                    continue;
                }
                let target = Name::parse(target)?;
                let Some(&index) = lookup.get(&target) else {
                    return Err(Error::MissingReference {
                        path: result.inner[source].name.0.clone(),
                        reference: target.0,
                    });
                };
                if index != source {
                    selected.insert(index);
                }
            }
            for target in selected {
                result.inner.add_edge(source, target, ());
            }
        }
        Ok(result)
    }

    pub(super) fn names(&self) -> Vec<String> {
        self.inner
            .node_weights()
            .map(|node| node.name.0.clone())
            .collect()
    }

    pub(super) fn find(&self, name: &str) -> Option<NodeIndex> {
        self.inner
            .node_indices()
            .find(|&index| self.inner[index].name.as_str() == name)
    }

    pub(super) fn roots(&self) -> Vec<NodeIndex> {
        self.inner.externals(Direction::Incoming).collect()
    }

    pub(super) fn reachable(
        &self,
        seeds: impl IntoIterator<Item = NodeIndex>,
        direction: Direction,
    ) -> BTreeSet<NodeIndex> {
        let mut visited = BTreeSet::new();
        let mut queue: VecDeque<_> = seeds.into_iter().collect();
        while let Some(index) = queue.pop_front() {
            if visited.insert(index) {
                queue.extend(self.inner.neighbors_directed(index, direction));
            }
        }
        visited
    }

    pub(super) fn subset(&self, selected: &BTreeSet<NodeIndex>) -> Self {
        let mut result = Self::empty();
        let mapping: BTreeMap<_, _> = self
            .inner
            .node_indices()
            .filter(|index| selected.contains(index))
            .map(|old| (old, result.inner.add_node(self.inner[old].clone())))
            .collect();
        for edge in self.inner.edge_references() {
            if let (Some(&source), Some(&target)) =
                (mapping.get(&edge.source()), mapping.get(&edge.target()))
            {
                result.inner.add_edge(source, target, ());
            }
        }
        result
    }

    pub(super) fn without(&self, removed: &BTreeSet<NodeIndex>) -> Self {
        self.subset(
            &self
                .inner
                .node_indices()
                .filter(|index| !removed.contains(index))
                .collect(),
        )
    }

    pub(super) fn without_incoming(&self, selected: &BTreeSet<NodeIndex>) -> Self {
        let mut copy = self.clone();
        copy.inner.retain_edges(|graph, edge| {
            graph
                .edge_endpoints(edge)
                .is_some_and(|(_, target)| !selected.contains(&target))
        });
        copy
    }

    pub(super) fn singleton(node: Node) -> Self {
        let mut result = Self::empty();
        result.inner.add_node(node);
        result
    }

    pub(super) fn merge(graphs: impl IntoIterator<Item = Self>) -> Self {
        let mut result = Self::empty();
        for graph in graphs {
            let mapping: BTreeMap<_, _> = graph
                .inner
                .node_indices()
                .map(|old| (old, result.inner.add_node(graph.inner[old].clone())))
                .collect();
            for edge in graph.inner.edge_references() {
                result
                    .inner
                    .add_edge(mapping[&edge.source()], mapping[&edge.target()], ());
            }
        }
        result
    }
}
