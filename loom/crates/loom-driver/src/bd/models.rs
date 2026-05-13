use serde::{Deserialize, Serialize};

use super::label::Label;
use crate::identifier::{BeadId, MoleculeId};

/// One bead as produced by `bd show --json` and `bd list --json`.
///
/// `bd` emits more fields than these (timestamps, owner, dependency lists,
/// metadata blobs); they are intentionally not modelled here. `serde`
/// ignores unknown fields by default, so the wrapper does not break when
/// `bd` adds new keys. Add fields when a caller needs them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bead {
    pub id: BeadId,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub status: String,
    #[serde(default)]
    pub priority: u8,
    #[serde(default, rename = "issue_type")]
    pub issue_type: String,
    #[serde(default)]
    pub labels: Vec<Label>,
}

/// One molecule row. Beads exposes `bd mol show --json`; the shape is the
/// same epic-shaped record as a bead with extra molecule metadata, so the
/// wrapper currently surfaces only the always-present fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Molecule {
    pub id: MoleculeId,
    pub title: String,
    #[serde(default)]
    pub status: String,
}

/// Output of `bd mol progress <id> --json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MolProgress {
    pub molecule_id: MoleculeId,
    #[serde(default)]
    pub molecule_title: String,
    pub completed: u32,
    pub in_progress: u32,
    pub total: u32,
    #[serde(default)]
    pub percent: f64,
    #[serde(default)]
    pub current_step_id: Option<String>,
}
