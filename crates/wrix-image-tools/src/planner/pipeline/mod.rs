use super::{graph::Graph, score};
use petgraph::{Direction, graph::NodeIndex};
use serde_json::Value;
use std::{collections::BTreeSet, num::NonZeroUsize};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// invalid layer pipeline operation or arguments: {0}
    Arguments(String),
    /// layer pipeline operation received an incompatible graph/list/group shape
    Shape,
    /// layer pipeline refers to an absent group: {0}
    MissingGroup(String),
    /// {0}
    Score(#[from] score::Error),
}

#[derive(Debug)]
pub(super) enum Tree {
    Graph(Graph),
    List(Vec<Self>),
    Groups(Vec<(String, Self)>),
}

impl Tree {
    pub(super) fn flatten(self) -> Vec<Graph> {
        match self {
            Self::Graph(graph) => vec![graph],
            Self::List(list) => list.into_iter().flat_map(Self::flatten).collect(),
            Self::Groups(groups) => groups
                .into_iter()
                .flat_map(|(_, tree)| tree.flatten())
                .collect(),
        }
    }
}

pub(super) enum SplitSpec {
    Path(String),
    Children(Vec<String>),
}

pub(super) enum Step {
    Flatten,
    Over(String, Box<Self>),
    SplitEvery(NonZeroUsize),
    Limit(NonZeroUsize),
    Remove(Vec<String>),
    Reverse,
    Reorder(Vec<String>),
    Subcomponent(Direction, Vec<String>),
    SplitPaths(Vec<SplitSpec>),
    Popularity,
    Map(Box<Self>),
    Pipe(Vec<Self>),
}

fn string(value: &Value) -> Result<String, Error> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::Arguments(value.to_string()))
}
fn strings(value: &Value, allow_single: bool) -> Result<Vec<String>, Error> {
    if allow_single && value.is_string() {
        return Ok(vec![string(value)?]);
    }
    value
        .as_array()
        .ok_or_else(|| Error::Arguments(value.to_string()))?
        .iter()
        .map(string)
        .collect()
}
fn count(value: &Value) -> Result<NonZeroUsize, Error> {
    let raw = value
        .as_u64()
        .ok_or_else(|| Error::Arguments(value.to_string()))?;
    let count = usize::try_from(raw).map_err(|_| Error::Arguments(value.to_string()))?;
    NonZeroUsize::new(count)
        .ok_or_else(|| Error::Arguments("layer counts must be positive".to_owned()))
}

impl Step {
    pub(super) fn parse(value: &Value) -> Result<Self, Error> {
        let array = value
            .as_array()
            .ok_or_else(|| Error::Arguments(value.to_string()))?;
        let Some((name, args)) = array.split_first() else {
            return Err(Error::Arguments("empty operation".to_owned()));
        };
        match (name.as_str(), args) {
            (Some("flatten"), []) => Ok(Self::Flatten),
            (Some("over"), [key, operation]) => {
                Ok(Self::Over(string(key)?, Box::new(Self::parse(operation)?)))
            }
            (Some("split_every"), [size]) => Ok(Self::SplitEvery(count(size)?)),
            (Some("limit_layers"), [size]) => Ok(Self::Limit(count(size)?)),
            (Some("remove_paths"), [paths]) => Ok(Self::Remove(strings(paths, true)?)),
            (Some("reverse"), []) => Ok(Self::Reverse),
            (Some("reorder"), [keys]) => Ok(Self::Reorder(strings(keys, false)?)),
            (Some("subcomponent_in"), [paths]) => Ok(Self::Subcomponent(
                Direction::Incoming,
                strings(paths, false)?,
            )),
            (Some("subcomponent_out"), [paths]) => Ok(Self::Subcomponent(
                Direction::Outgoing,
                strings(paths, false)?,
            )),
            (Some("split_paths"), [specs]) => {
                let specs = specs
                    .as_array()
                    .ok_or_else(|| Error::Arguments(specs.to_string()))?;
                let parsed = specs
                    .iter()
                    .map(|spec| {
                        if let Some(children) = spec
                            .as_object()
                            .and_then(|object| object.get("children_of"))
                        {
                            Ok(SplitSpec::Children(strings(children, true)?))
                        } else {
                            Ok(SplitSpec::Path(string(spec)?))
                        }
                    })
                    .collect::<Result<_, Error>>()?;
                Ok(Self::SplitPaths(parsed))
            }
            (Some("popularity_contest"), []) => Ok(Self::Popularity),
            (Some("map"), [operation]) => Ok(Self::Map(Box::new(Self::parse(operation)?))),
            (Some("pipe"), [operations]) => {
                let operations = operations
                    .as_array()
                    .ok_or_else(|| Error::Arguments(operations.to_string()))?;
                Ok(Self::Pipe(
                    operations
                        .iter()
                        .map(Self::parse)
                        .collect::<Result<_, _>>()?,
                ))
            }
            _ => Err(Error::Arguments(value.to_string())),
        }
    }

    pub(super) fn apply(&self, tree: Tree) -> Result<Tree, Error> {
        match (self, tree) {
            (Self::Flatten, Tree::Graph(_)) => Err(Error::Shape),
            (Self::Flatten, tree) => Ok(Tree::List(
                tree.flatten().into_iter().map(Tree::Graph).collect(),
            )),
            (Self::Popularity, Tree::Graph(graph)) => Ok(Tree::List(
                score::popularity(&graph)?
                    .into_iter()
                    .map(Tree::Graph)
                    .collect(),
            )),
            (Self::SplitEvery(count), Tree::Graph(graph)) => {
                let nodes: Vec<_> = graph.inner.node_indices().collect();
                Ok(Tree::List(
                    nodes
                        .chunks(count.get())
                        .map(|chunk| Tree::Graph(graph.subset(&chunk.iter().copied().collect())))
                        .collect(),
                ))
            }
            (Self::Limit(count), Tree::List(list)) => {
                let mut remaining = list.into_iter();
                let mut result: Vec<_> = remaining.by_ref().take(count.get() - 1).collect();
                let tail = remaining
                    .map(|tree| match tree {
                        Tree::Graph(graph) => Ok(graph),
                        _ => Err(Error::Shape),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                result.push(Tree::Graph(Graph::merge(tail)));
                Ok(Tree::List(result))
            }
            (Self::Remove(paths), Tree::Graph(graph)) => {
                let removed = paths.iter().filter_map(|path| graph.find(path)).collect();
                Ok(Tree::Graph(graph.without(&removed)))
            }
            (Self::Reverse, Tree::List(mut list)) => {
                list.reverse();
                Ok(Tree::List(list))
            }
            (Self::Over(key, operation), Tree::Groups(groups)) => {
                if !groups.iter().any(|(name, _)| name == key) {
                    return Err(Error::MissingGroup(key.clone()));
                }
                Ok(Tree::Groups(
                    groups
                        .into_iter()
                        .map(|(name, tree)| {
                            let value = if &name == key {
                                operation.apply(tree)?
                            } else {
                                tree
                            };
                            Ok((name, value))
                        })
                        .collect::<Result<_, Error>>()?,
                ))
            }
            (Self::Map(operation), Tree::List(list)) => Ok(Tree::List(
                list.into_iter()
                    .map(|tree| operation.apply(tree))
                    .collect::<Result<_, _>>()?,
            )),
            (Self::Reorder(keys), Tree::Groups(mut groups)) => {
                let mut result = Vec::new();
                for key in keys {
                    if let Some(index) = groups.iter().position(|(name, _)| name == key) {
                        result.push(groups.remove(index));
                    }
                }
                result.extend(groups);
                Ok(Tree::Groups(result))
            }
            (Self::Reorder(_), tree) => Ok(tree),
            (Self::Subcomponent(direction, paths), Tree::Graph(graph)) => {
                let selected =
                    graph.reachable(paths.iter().filter_map(|path| graph.find(path)), *direction);
                Ok(Tree::Groups(vec![
                    ("main".to_owned(), Tree::Graph(graph.subset(&selected))),
                    ("rest".to_owned(), Tree::Graph(graph.without(&selected))),
                ]))
            }
            (Self::SplitPaths(specs), Tree::Graph(graph)) => Ok(split_paths(&graph, specs)),
            (Self::Pipe(steps), tree) => steps.iter().try_fold(tree, |tree, step| step.apply(tree)),
            _ => Err(Error::Shape),
        }
    }
}

fn groups(main: Graph, common: Graph, rest: Graph) -> Tree {
    Tree::Groups(vec![
        ("main".to_owned(), Tree::Graph(main)),
        ("common".to_owned(), Tree::Graph(common)),
        ("rest".to_owned(), Tree::Graph(rest)),
    ])
}

fn split_paths(graph: &Graph, specs: &[SplitSpec]) -> Tree {
    let mut selected = BTreeSet::new();
    for spec in specs {
        match spec {
            SplitSpec::Path(path) => selected.extend(graph.find(path)),
            SplitSpec::Children(paths) => {
                for index in paths.iter().filter_map(|path| graph.find(path)) {
                    selected.extend(graph.inner.neighbors_directed(index, Direction::Outgoing));
                }
            }
        }
    }
    if selected.is_empty() {
        return groups(Graph::empty(), Graph::empty(), graph.clone());
    }
    let roots = graph.roots();
    if roots.len() == 1 && selected.contains(&roots[0]) {
        return groups(graph.clone(), Graph::empty(), Graph::empty());
    }
    let split = graph.reachable(selected.iter().copied(), Direction::Outgoing);
    let modified = graph.without_incoming(&selected);
    let rest = modified.reachable(
        roots.into_iter().filter(|index| !selected.contains(index)),
        Direction::Outgoing,
    );
    let common: BTreeSet<_> = split.intersection(&rest).copied().collect();
    let main: BTreeSet<_> = split.difference(&common).copied().collect();
    // A DAG has no orphan vertices here. Retaining the complement also handles
    // rootless cyclic components, which upstream's synthetic-root split loses.
    let remaining: BTreeSet<NodeIndex> = graph
        .inner
        .node_indices()
        .filter(|index| !split.contains(index))
        .collect();
    groups(
        modified.subset(&main),
        modified.subset(&common),
        modified.subset(&remaining),
    )
}
