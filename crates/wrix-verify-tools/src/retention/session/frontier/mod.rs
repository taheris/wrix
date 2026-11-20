//! Retain valid pending build frontiers, not the full historical recipe closure.
use super::{check, create_file};
use crate::retention::{
    BATCH_SIZE, Cache, Error, StorePath, exists, initialize, private_directory, regular,
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs::File,
    io::{self, BufReader},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Recipe(PathBuf);

impl Recipe {
    fn parse(value: &str) -> Result<Self, Error> {
        let path = store(value);
        let output = path
            .strip_suffix(".drv")
            .ok_or_else(|| Error::Store(path.clone()))?;
        StorePath::parse(output)?;
        Ok(Self(PathBuf::from(path)))
    }
}

fn store(value: &str) -> String {
    if value.starts_with("/nix/store/") {
        value.to_owned()
    } else {
        format!("/nix/store/{value}")
    }
}

#[derive(Deserialize)]
struct RawGraph {
    version: u32,
    derivations: BTreeMap<String, RawNode>,
}

#[derive(Deserialize)]
struct RawNode {
    inputs: RawInputs,
}

#[derive(Deserialize)]
struct RawInputs {
    drvs: BTreeMap<String, RawDependency>,
    srcs: BTreeSet<String>,
}

#[derive(Deserialize)]
struct RawDependency {
    #[serde(rename = "dynamicOutputs")]
    dynamic: BTreeMap<String, serde_json::Value>,
}

struct Node {
    inputs: BTreeSet<Recipe>,
    sources: BTreeSet<StorePath>,
    outputs: BTreeSet<StorePath>,
}

struct Graph(BTreeMap<Recipe, Node>);

fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, Error> {
    regular(path)?;
    let file = File::open(path)?;
    if file.metadata()?.len() > LIMIT {
        return Err(Error::Foreign(path.to_owned()));
    }
    serde_json::from_reader(BufReader::new(file))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error).into())
}

impl Graph {
    fn load(graph: &Path, outputs: &Path) -> Result<Self, Error> {
        let raw: RawGraph = read(graph)?;
        if !matches!(raw.version, 3 | 4) {
            return Err(Error::Foreign(graph.to_owned()));
        }
        let mut outputs: BTreeMap<String, Vec<String>> = read(outputs)?;
        if outputs.keys().ne(raw.derivations.keys()) {
            return Err(Error::Foreign(graph.to_owned()));
        }
        let mut nodes = BTreeMap::new();
        for (name, node) in raw.derivations {
            if node
                .inputs
                .drvs
                .values()
                .any(|dependency| !dependency.dynamic.is_empty())
            {
                return Err(Error::Foreign(graph.to_owned()));
            }
            let recipe = Recipe::parse(&name)?;
            for source in node.inputs.srcs.iter().filter(|path| {
                Path::new(path)
                    .extension()
                    .is_some_and(|extension| extension == "drv")
            }) {
                Recipe::parse(source)?;
            }
            let sources = node
                .inputs
                .srcs
                .iter()
                .filter(|path| {
                    Path::new(path)
                        .extension()
                        .is_none_or(|extension| extension != "drv")
                })
                .map(|path| StorePath::parse(&store(path)))
                .collect::<Result<_, _>>()?;
            let inputs = node
                .inputs
                .drvs
                .keys()
                .map(|path| Recipe::parse(path))
                .collect::<Result<_, _>>()?;
            let paths = outputs
                .remove(&name)
                .ok_or_else(|| Error::Foreign(graph.to_owned()))?;
            if paths.is_empty() {
                return Err(Error::Foreign(graph.to_owned()));
            }
            let outputs = paths
                .iter()
                .map(|path| StorePath::parse(path))
                .collect::<Result<_, _>>()?;
            nodes.insert(
                recipe,
                Node {
                    inputs,
                    sources,
                    outputs,
                },
            );
        }
        if nodes
            .values()
            .any(|node| node.inputs.iter().any(|input| !nodes.contains_key(input)))
        {
            return Err(Error::Foreign(graph.to_owned()));
        }
        Ok(Self(nodes))
    }

    fn frontier(
        &self,
        roots: BTreeSet<Recipe>,
        mut valid: impl FnMut(&BTreeSet<StorePath>) -> Result<BTreeSet<StorePath>, Error>,
    ) -> Result<BTreeSet<StorePath>, Error> {
        let mut waiting = roots;
        let mut seen = BTreeSet::new();
        let mut frontier = BTreeSet::new();
        while !waiting.is_empty() {
            let nodes = waiting
                .into_iter()
                .map(|recipe| {
                    seen.insert(recipe.clone());
                    self.0.get(&recipe).ok_or(Error::Foreign(recipe.0))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let candidates = nodes
                .iter()
                .flat_map(|node| node.outputs.iter().cloned())
                .collect();
            let present = valid(&candidates)?;
            frontier.extend(present.iter().cloned());
            waiting = BTreeSet::new();
            for node in nodes {
                if !node.outputs.is_subset(&present) {
                    frontier.extend(node.sources.iter().cloned());
                    waiting.extend(node.inputs.difference(&seen).cloned());
                }
            }
        }
        Ok(frontier)
    }
}

fn validity(paths: &BTreeSet<StorePath>, nix: &Path) -> Result<BTreeSet<StorePath>, Error> {
    let paths: Vec<_> = paths.iter().collect();
    let mut valid = BTreeSet::new();
    for batch in paths.chunks(BATCH_SIZE) {
        let result = Command::new(nix)
            .args(["--check-validity", "--print-invalid"])
            .args(batch.iter().map(|path| &path.0))
            .stderr(Stdio::inherit())
            .output()?;
        if !result.status.success() {
            return Err(Error::Nix);
        }
        let text = std::str::from_utf8(&result.stdout)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let invalid = text
            .lines()
            .map(StorePath::parse)
            .collect::<Result<BTreeSet<_>, _>>()?;
        if invalid.iter().any(|path| !batch.contains(&path)) {
            return Err(Error::Nix);
        }
        valid.extend(
            batch
                .iter()
                .filter(|path| !invalid.contains(**path))
                .map(|path| (*path).clone()),
        );
    }
    Ok(valid)
}

fn capture(directory: &Path, nix: &Path, recipes: &BTreeSet<Recipe>) -> Result<(), Error> {
    let graph = directory.join("graph.json");
    let outputs = directory.join("outputs.json");
    if exists(&graph)? || exists(&outputs)? {
        return Ok(());
    }
    let file = create_file(&graph, b"")?;
    if !Command::new(nix)
        .args(["derivation", "show", "--recursive"])
        .args(recipes.iter().map(|recipe| &recipe.0))
        .stdout(file)
        .status()?
        .success()
    {
        return Err(Error::Nix);
    }
    let raw: RawGraph = read(&graph)?;
    if !matches!(raw.version, 3 | 4) {
        return Err(Error::Foreign(graph));
    }
    let nodes = raw
        .derivations
        .keys()
        .map(|key| Recipe::parse(key).map(|recipe| recipe.0))
        .collect::<Result<Vec<_>, _>>()?;
    let nodes_path = directory.join("nodes.json");
    serde_json::to_writer(create_file(&nodes_path, b"")?, &nodes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let expression = r#"let paths = builtins.fromJSON (builtins.readFile (builtins.getEnv "WRIX_CI_FRONTIER_NODES")); in builtins.listToAttrs (map (path: let d = import (builtins.storePath path); in { name = builtins.baseNameOf path; value = map (output: output.outPath) d.all; }) paths)"#;
    if !Command::new(nix)
        .args([
            "eval",
            "--offline",
            "--option",
            "allow-import-from-derivation",
            "false",
            "--impure",
            "--json",
            "--expr",
            expression,
        ])
        .env("WRIX_CI_FRONTIER_NODES", nodes_path)
        .stdout(create_file(&outputs, b"")?)
        .status()?
        .success()
    {
        return Err(Error::Nix);
    }
    Ok(())
}

fn prepare(directory: &Path) -> Result<PathBuf, Error> {
    let pending = directory.join("pending");
    if !exists(&pending)? {
        let parent = directory
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| Error::Foreign(directory.to_owned()))?;
        private_directory(parent)?;
        let temporary = tempfile::Builder::new()
            .prefix(".ci-pending-init-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(parent)?;
        let candidate = temporary.path().join("pending");
        initialize(&candidate)?;
        match rustix::fs::renameat_with(
            rustix::fs::CWD,
            &candidate,
            rustix::fs::CWD,
            &pending,
            rustix::fs::RenameFlags::NOREPLACE,
        ) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(error) => return Err(io::Error::from(error).into()),
        }
    }
    Ok(pending)
}

/// Capture actual dispatcher recipes once, then refresh validity without re-evaluation.
pub fn protect(
    directory: &Path,
    descriptor: &str,
    program: &std::ffi::OsStr,
    nix: &Path,
    nix_store: &Path,
    recipes: &[OsString],
) -> Result<(), Error> {
    check(directory, descriptor, program)?;
    let roots = recipes
        .iter()
        .map(|path| {
            path.to_str()
                .ok_or_else(|| Error::Foreign(PathBuf::from(path)))
                .and_then(Recipe::parse)
        })
        .collect::<Result<_, _>>()?;
    let pending = prepare(directory)?;
    let cache = Cache::open(&pending)?;
    let desired = if recipes.is_empty() {
        BTreeSet::new()
    } else {
        capture(directory, nix, &roots)?;
        Graph::load(
            &directory.join("graph.json"),
            &directory.join("outputs.json"),
        )?
        .frontier(roots, |paths| validity(paths, nix_store))?
    };
    cache.replace(&desired, nix_store, &mut io::stderr().lock())
}

#[cfg(test)]
mod test;
