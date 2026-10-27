//! Admission and realized inventory for only the selected native derivation graph.
use super::{Error, Result, Root, nix_bin, process_failed};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub const PREFIX: &str = "selected-graph:";

#[derive(Deserialize)]
#[serde(untagged)]
enum Inventory {
    Versioned {
        derivations: BTreeMap<String, Derivation>,
        version: u32,
    },
    Legacy(BTreeMap<String, Derivation>),
}

#[derive(Deserialize)]
struct Derivation {
    outputs: BTreeMap<String, Output>,
}

#[derive(Deserialize)]
struct Output {
    path: Option<String>,
}

pub fn discover(selected: &[Root], cwd: &Path, scope: &str) -> Result<Vec<Root>> {
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    let output = Command::new(nix_bin()?)
        .current_dir(cwd)
        .args(["derivation", "show", "--recursive"])
        .args(selected.iter().map(|root| root.drv_path.as_str()))
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(process_failed("nix derivation show", &output.stderr));
    }
    parse(&output.stdout, scope)
}

fn parse(bytes: &[u8], scope: &str) -> Result<Vec<Root>> {
    let inventory: Inventory = serde_json::from_slice(bytes).map_err(|source| Error::Json {
        path: "selected derivation graph".into(),
        source,
    })?;
    let (derivations, relative) = match inventory {
        Inventory::Versioned {
            derivations,
            version: 4,
        } => (derivations, true),
        Inventory::Versioned { .. } => return Err(Error::GraphInventory),
        Inventory::Legacy(derivations) => (derivations, false),
    };
    derivations
        .into_iter()
        .map(|(drv, derivation)| {
            let drv_path = store_path(&drv, relative)?;
            if Path::new(&drv_path)
                .extension()
                .is_none_or(|extension| extension != "drv")
            {
                return Err(Error::GraphInventory);
            }
            let out_paths = derivation
                .outputs
                .into_values()
                .filter_map(|output| output.path)
                .map(|path| store_path(&path, relative))
                .collect::<Result<Vec<_>>>()?;
            Ok(Root {
                name: format!("{PREFIX}{scope}"),
                installable: drv_path.clone(),
                drv_path,
                out_paths,
            })
        })
        .collect()
}

fn store_path(value: &str, relative: bool) -> Result<String> {
    let path = if relative {
        Path::new("/nix/store").join(value)
    } else {
        PathBuf::from(value)
    };
    if path.parent() != Some(Path::new("/nix/store")) || (relative && value.contains('/')) {
        return Err(Error::GraphInventory);
    }
    path.into_os_string()
        .into_string()
        .map_err(|_| Error::GraphInventory)
}

pub fn realized(roots: &[Root]) -> Result<Vec<String>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let candidates = roots
        .iter()
        .flat_map(|root| root.out_paths.iter())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let mut realized = Vec::new();
    for path in candidates {
        if runtime.block_on(crate::backend::paths_realized(std::slice::from_ref(&path)))? {
            realized.push(path);
        }
    }
    Ok(realized)
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn parses_native_legacy_and_version_four_graphs() {
        for bytes in [
            br#"{"/nix/store/aaa-root.drv":{"outputs":{"out":{"path":"/nix/store/bbb-root"},"dynamic":{}}}}"#.as_slice(),
            br#"{"version":4,"derivations":{"aaa-root.drv":{"outputs":{"out":{"path":"bbb-root"},"dynamic":{}}}}}"#.as_slice(),
        ] {
            let roots = parse(bytes, "scope").unwrap();
            assert_eq!(roots[0].drv_path, "/nix/store/aaa-root.drv");
            assert_eq!(roots[0].out_paths, ["/nix/store/bbb-root"]);
        }
    }
    #[test]
    fn rejects_graph_paths_outside_native_store() {
        assert!(
            parse(
                br#"{"version":4,"derivations":{"../root.drv":{"outputs":{}}}}"#,
                "scope"
            )
            .is_err()
        );
    }
}
