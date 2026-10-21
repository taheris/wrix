//! Publication documents shared by their writer and the explicit importer.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Root {
    pub name: String,
    pub installable: String,
    #[serde(alias = "drvPath")]
    pub drv_path: String,
    #[serde(default, alias = "outPaths")]
    pub out_paths: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(default)]
    pub schema_version: Option<u8>,
    #[serde(default)]
    pub workspace_hash: Option<String>,
    pub roots: Vec<Root>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Pending {
    #[serde(default)]
    pub created_at_epoch: Option<u64>,
    #[serde(alias = "drvPath")]
    pub drv_path: String,
    #[serde(default, alias = "outPaths")]
    pub out_paths: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub dirty: bool,
    pub last_publish: Option<String>,
    pub last_prune: Option<String>,
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_prune_epoch: Option<u64>,
}
