//! Native implementation of dockerTools' reference-graph layering protocol.

pub mod auto;
mod graph;
mod pipeline;
mod score;

use pipeline::{Step, Tree};
use serde::Deserialize;
use serde_json::Value;
use std::{
    ffi::OsString,
    fs,
    io::{self, Write},
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// usage: `flatten_references_graph` `STRUCTURED_ATTRIBUTES_JSON`
    Usage,
    /// invalid reference-graph input JSON: {0}
    Json(#[from] serde_json::Error),
    /// {0}
    Graph(#[from] graph::Error),
    /// {0}
    Pipeline(#[from] pipeline::Error),
    /// layer planner I/O failed: {0}
    Io(#[from] io::Error),
    /// graph plotting is not supported by the native image layer planner; use the upstream developer tool
    Plotting,
}

#[derive(Deserialize)]
struct Input {
    graph: Vec<graph::RawNode>,
    pipeline: Vec<Value>,
    exclude_paths: Option<Vec<String>>,
}

pub fn plan_json(bytes: &[u8]) -> Result<Vec<Vec<String>>, Error> {
    let input: Input = serde_json::from_slice(bytes)?;
    let excluded = input.exclude_paths.unwrap_or_default();
    let graph = graph::Graph::parse(input.graph, &excluded)?;
    let steps = input
        .pipeline
        .iter()
        .map(Step::parse)
        .collect::<Result<Vec<_>, _>>()?;
    let tree = steps
        .iter()
        .try_fold(Tree::Graph(graph), |tree, step| step.apply(tree))?;
    if matches!(tree, Tree::Graph(_)) {
        return Err(pipeline::Error::Shape.into());
    }
    Ok(tree
        .flatten()
        .into_iter()
        .map(|graph| graph.names())
        .filter(|paths| !paths.is_empty())
        .collect())
}

pub fn run(args: impl IntoIterator<Item = OsString>) -> Result<(), Error> {
    let args: Vec<_> = args.into_iter().collect();
    let [path] = args.as_slice() else {
        return Err(Error::Usage);
    };
    if std::env::var("DEBUG_PLOT").as_deref() == Ok("True") {
        return Err(Error::Plotting);
    }
    let layers = plan_json(&fs::read(path)?)?;
    if std::env::var("DEBUG").as_deref() == Ok("True") {
        writeln!(
            io::stderr().lock(),
            "native layer planner: {} layers",
            layers.len()
        )?;
    }
    let mut output = io::stdout().lock();
    serde_json::to_writer_pretty(&mut output, &layers)?;
    writeln!(output)?;
    Ok(())
}

#[cfg(test)]
mod test;
