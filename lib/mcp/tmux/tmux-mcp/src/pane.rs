//! Pane state management.

use displaydoc::Display;
use serde::Serialize;
use std::collections::HashMap;
use std::fmt;
use thiserror::Error;

const PANE_ID_PREFIX: &str = "debug-";

/// Pane identifier parse failure.
#[derive(Debug, Clone, Display, Error, PartialEq, Eq)]
pub enum PaneIdError {
    /// Pane id '{value}' must use debug-N with N greater than zero
    InvalidFormat { value: String },
    /// Pane id sequence exhausted
    SequenceExhausted,
}

/// Identifier for a pane managed by this server.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct PaneId(String);

impl PaneId {
    fn from_sequence(sequence: u64) -> Result<Self, PaneIdError> {
        if sequence == 0 {
            return Err(PaneIdError::InvalidFormat {
                value: format!("{PANE_ID_PREFIX}{sequence}"),
            });
        }

        Ok(Self(format!("{PANE_ID_PREFIX}{sequence}")))
    }

    /// Parse and validate a pane identifier.
    pub fn parse(value: impl Into<String>) -> Result<Self, PaneIdError> {
        let value = value.into();
        let Some(suffix) = value.strip_prefix(PANE_ID_PREFIX) else {
            return Err(PaneIdError::InvalidFormat { value });
        };

        if suffix.is_empty() {
            return Err(PaneIdError::InvalidFormat { value });
        }

        let Ok(parsed) = suffix.parse::<u64>() else {
            return Err(PaneIdError::InvalidFormat { value });
        };

        if parsed == 0 {
            return Err(PaneIdError::InvalidFormat { value });
        }

        Ok(Self(value))
    }

    /// Borrow the validated identifier string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AsRef<str> for PaneId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// Status of a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneStatus {
    /// Process is still running.
    Running,
    /// Process has exited and remains visible for post-mortem capture.
    Exited,
}

impl PaneStatus {
    /// Convert status to its wire representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            PaneStatus::Running => "running",
            PaneStatus::Exited => "exited",
        }
    }
}

impl fmt::Display for PaneStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", (*self).as_str())
    }
}

/// State of a single pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneState {
    /// Unique pane identifier.
    pub id: PaneId,
    /// Human-readable name.
    pub name: String,
    /// Current status of the pane.
    pub status: PaneStatus,
    /// Command that was executed in the pane.
    pub command: String,
}

impl PaneState {
    /// Create a new `PaneState`.
    pub const fn new(id: PaneId, name: String, command: String) -> Self {
        Self {
            id,
            name,
            status: PaneStatus::Running,
            command,
        }
    }

    /// Update the pane status.
    pub const fn set_status(&mut self, status: PaneStatus) {
        self.status = status;
    }

    /// Check if the pane is running.
    #[cfg(test)]
    pub fn is_running(&self) -> bool {
        self.status == PaneStatus::Running
    }

    /// Check if the pane has exited.
    #[cfg(test)]
    pub fn is_exited(&self) -> bool {
        self.status == PaneStatus::Exited
    }
}

/// Manages all pane state for the MCP server.
#[derive(Debug)]
pub struct PaneManager {
    panes: HashMap<PaneId, PaneState>,
    next_id: u64,
}

impl PaneManager {
    /// Create a new `PaneManager`.
    pub fn new() -> Self {
        Self {
            panes: HashMap::new(),
            next_id: 1,
        }
    }

    /// Generate a unique pane ID in debug-N format.
    pub fn generate_id(&mut self) -> Result<PaneId, PaneIdError> {
        let id = PaneId::from_sequence(self.next_id)?;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(PaneIdError::SequenceExhausted)?;
        Ok(id)
    }

    /// Register a new pane with the manager.
    pub fn create_pane(
        &mut self,
        command: &str,
        name: Option<&str>,
    ) -> Result<PaneId, PaneIdError> {
        let id = self.generate_id()?;
        let display_name = name.unwrap_or_else(|| id.as_str()).to_string();
        let state = PaneState::new(id.clone(), display_name, command.to_string());
        self.panes.insert(id.clone(), state);
        Ok(id)
    }

    /// Get a pane by its ID.
    #[cfg(test)]
    pub fn get(&self, pane_id: &PaneId) -> Option<&PaneState> {
        self.panes.get(pane_id)
    }

    /// Get a mutable reference to a pane by its ID.
    #[cfg(test)]
    pub fn get_mut(&mut self, pane_id: &PaneId) -> Option<&mut PaneState> {
        self.panes.get_mut(pane_id)
    }

    /// Check if a pane exists.
    pub fn contains(&self, pane_id: &PaneId) -> bool {
        self.panes.contains_key(pane_id)
    }

    /// Remove a pane from tracking.
    pub fn remove(&mut self, pane_id: &PaneId) -> Option<PaneState> {
        self.panes.remove(pane_id)
    }

    /// Update a pane's status.
    pub fn update_status(&mut self, pane_id: &PaneId, status: PaneStatus) -> bool {
        self.panes.get_mut(pane_id).is_some_and(|pane| {
            pane.set_status(status);
            true
        })
    }

    /// Get all panes as an iterator.
    pub fn iter(&self) -> impl Iterator<Item = &PaneState> {
        self.panes.values()
    }

    /// Get the number of tracked panes.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.panes.len()
    }

    /// Check if there are no tracked panes.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.panes.is_empty()
    }

    /// Get all pane IDs.
    #[cfg(test)]
    pub fn pane_ids(&self) -> Vec<PaneId> {
        self.panes.keys().cloned().collect()
    }
}

impl Default for PaneManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane_id(value: &str) -> PaneId {
        PaneId::parse(value).unwrap()
    }

    #[test]
    fn test_pane_id_parse_accepts_generated_format() {
        let id = pane_id("debug-42");
        assert_eq!(id.as_str(), "debug-42");
        assert_eq!(id.to_string(), "debug-42");
    }

    #[test]
    fn test_pane_id_parse_rejects_invalid_values() {
        assert!(PaneId::parse("pane-1").is_err());
        assert!(PaneId::parse("debug-").is_err());
        assert!(PaneId::parse("debug-zero").is_err());
        assert!(PaneId::parse("debug-0").is_err());
    }

    #[test]
    fn test_pane_status_as_str() {
        assert_eq!(PaneStatus::Running.as_str(), "running");
        assert_eq!(PaneStatus::Exited.as_str(), "exited");
    }

    #[test]
    fn test_pane_status_display() {
        assert_eq!(format!("{}", PaneStatus::Running), "running");
        assert_eq!(format!("{}", PaneStatus::Exited), "exited");
    }

    #[test]
    fn test_pane_status_equality() {
        assert_eq!(PaneStatus::Running, PaneStatus::Running);
        assert_eq!(PaneStatus::Exited, PaneStatus::Exited);
        assert_ne!(PaneStatus::Running, PaneStatus::Exited);
    }

    #[test]
    fn test_pane_state_new() {
        let id = pane_id("debug-1");
        let state = PaneState::new(id.clone(), "server".to_string(), "cargo run".to_string());

        assert_eq!(state.id, id);
        assert_eq!(state.name, "server");
        assert_eq!(state.command, "cargo run");
        assert_eq!(state.status, PaneStatus::Running);
    }

    #[test]
    fn test_pane_state_initial_status_is_running() {
        let state = PaneState::new(
            pane_id("debug-1"),
            "test".to_string(),
            "echo hello".to_string(),
        );

        assert!(state.is_running());
        assert!(!state.is_exited());
    }

    #[test]
    fn test_pane_state_set_status() {
        let mut state = PaneState::new(
            pane_id("debug-1"),
            "test".to_string(),
            "echo hello".to_string(),
        );

        assert!(state.is_running());
        state.set_status(PaneStatus::Exited);
        assert!(state.is_exited());
        assert!(!state.is_running());
    }

    #[test]
    fn test_pane_state_status_transitions() {
        let mut state = PaneState::new(
            pane_id("debug-1"),
            "test".to_string(),
            "echo hello".to_string(),
        );

        assert_eq!(state.status, PaneStatus::Running);
        state.set_status(PaneStatus::Exited);
        assert_eq!(state.status, PaneStatus::Exited);
        state.set_status(PaneStatus::Running);
        assert_eq!(state.status, PaneStatus::Running);
    }

    #[test]
    fn test_pane_state_clone() {
        let state = PaneState::new(
            pane_id("debug-1"),
            "server".to_string(),
            "cargo run".to_string(),
        );

        let cloned = state.clone();
        assert_eq!(state, cloned);
    }

    #[test]
    fn test_manager_generate_id_format() {
        let mut manager = PaneManager::new();
        let id1 = manager.generate_id().unwrap();
        assert!(id1.as_str().starts_with("debug-"));
    }

    #[test]
    fn test_manager_generate_id_sequential() {
        let mut manager = PaneManager::new();

        let id1 = manager.generate_id().unwrap();
        let id2 = manager.generate_id().unwrap();
        let id3 = manager.generate_id().unwrap();

        assert_eq!(id1.as_str(), "debug-1");
        assert_eq!(id2.as_str(), "debug-2");
        assert_eq!(id3.as_str(), "debug-3");
    }

    #[test]
    fn test_manager_generate_id_unique() {
        let mut manager = PaneManager::new();
        let mut ids = std::collections::HashSet::new();

        for _ in 0..100 {
            let id = manager.generate_id().unwrap();
            assert!(ids.insert(id), "Generated duplicate ID");
        }
    }

    #[test]
    fn test_manager_create_pane_without_name() {
        let mut manager = PaneManager::new();
        let id = manager.create_pane("cargo run", None).unwrap();

        assert_eq!(id.as_str(), "debug-1");
        let pane = manager.get(&id).unwrap();
        assert_eq!(pane.name, "debug-1");
        assert_eq!(pane.command, "cargo run");
    }

    #[test]
    fn test_manager_create_pane_with_name() {
        let mut manager = PaneManager::new();
        let id = manager.create_pane("cargo run", Some("server")).unwrap();

        assert_eq!(id.as_str(), "debug-1");
        let pane = manager.get(&id).unwrap();
        assert_eq!(pane.name, "server");
        assert_eq!(pane.command, "cargo run");
    }

    #[test]
    fn test_manager_create_multiple_panes() {
        let mut manager = PaneManager::new();

        let id1 = manager.create_pane("cargo run", Some("server")).unwrap();
        let id2 = manager.create_pane("bash", Some("client")).unwrap();
        let id3 = manager.create_pane("tail -f log", None).unwrap();

        assert_eq!(manager.len(), 3);
        assert_eq!(manager.get(&id1).unwrap().name, "server");
        assert_eq!(manager.get(&id2).unwrap().name, "client");
        assert_eq!(manager.get(&id3).unwrap().name, "debug-3");
    }

    #[test]
    fn test_manager_get_existing_pane() {
        let mut manager = PaneManager::new();
        let id = manager.create_pane("cargo run", Some("server")).unwrap();

        let pane = manager.get(&id);
        assert!(pane.is_some());
        assert_eq!(pane.unwrap().name, "server");
    }

    #[test]
    fn test_manager_get_nonexistent_pane() {
        let manager = PaneManager::new();
        let pane = manager.get(&pane_id("debug-999"));
        assert!(pane.is_none());
    }

    #[test]
    fn test_manager_get_mut() {
        let mut manager = PaneManager::new();
        let id = manager.create_pane("cargo run", Some("server")).unwrap();

        let pane = manager.get_mut(&id).unwrap();
        pane.set_status(PaneStatus::Exited);

        assert!(manager.get(&id).unwrap().is_exited());
    }

    #[test]
    fn test_manager_contains() {
        let mut manager = PaneManager::new();
        let id = manager.create_pane("cargo run", None).unwrap();

        assert!(manager.contains(&id));
        assert!(!manager.contains(&pane_id("debug-999")));
    }

    #[test]
    fn test_manager_remove_existing_pane() {
        let mut manager = PaneManager::new();
        let id = manager.create_pane("cargo run", Some("server")).unwrap();

        assert!(manager.contains(&id));
        let removed = manager.remove(&id);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().name, "server");
        assert!(!manager.contains(&id));
    }

    #[test]
    fn test_manager_remove_nonexistent_pane() {
        let mut manager = PaneManager::new();
        let removed = manager.remove(&pane_id("debug-999"));
        assert!(removed.is_none());
    }

    #[test]
    fn test_manager_remove_does_not_affect_other_panes() {
        let mut manager = PaneManager::new();
        let id1 = manager.create_pane("cargo run", Some("server")).unwrap();
        let id2 = manager.create_pane("bash", Some("client")).unwrap();

        manager.remove(&id1);

        assert!(!manager.contains(&id1));
        assert!(manager.contains(&id2));
        assert_eq!(manager.len(), 1);
    }

    #[test]
    fn test_manager_update_status_existing() {
        let mut manager = PaneManager::new();
        let id = manager.create_pane("cargo run", None).unwrap();

        assert!(manager.get(&id).unwrap().is_running());
        let result = manager.update_status(&id, PaneStatus::Exited);
        assert!(result);
        assert!(manager.get(&id).unwrap().is_exited());
    }

    #[test]
    fn test_manager_update_status_nonexistent() {
        let mut manager = PaneManager::new();
        let result = manager.update_status(&pane_id("debug-999"), PaneStatus::Exited);
        assert!(!result);
    }

    #[test]
    fn test_manager_iter() {
        let mut manager = PaneManager::new();
        manager.create_pane("cargo run", Some("server")).unwrap();
        manager.create_pane("bash", Some("client")).unwrap();

        assert_eq!(manager.iter().count(), 2);
    }

    #[test]
    fn test_manager_pane_ids() {
        let mut manager = PaneManager::new();
        let id1 = manager.create_pane("cargo run", None).unwrap();
        let id2 = manager.create_pane("bash", None).unwrap();

        let ids = manager.pane_ids();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&id1));
        assert!(ids.contains(&id2));
    }

    #[test]
    fn test_manager_len() {
        let mut manager = PaneManager::new();
        assert_eq!(manager.len(), 0);

        manager.create_pane("cargo run", None).unwrap();
        assert_eq!(manager.len(), 1);

        manager.create_pane("bash", None).unwrap();
        assert_eq!(manager.len(), 2);
    }

    #[test]
    fn test_manager_is_empty() {
        let mut manager = PaneManager::new();
        assert!(manager.is_empty());

        let id = manager.create_pane("cargo run", None).unwrap();
        assert!(!manager.is_empty());

        manager.remove(&id);
        assert!(manager.is_empty());
    }

    #[test]
    fn test_full_pane_lifecycle() {
        let mut manager = PaneManager::new();

        let id = manager
            .create_pane("RUST_LOG=debug cargo run", Some("server"))
            .unwrap();
        assert_eq!(manager.len(), 1);

        let pane = manager.get(&id).unwrap();
        assert!(pane.is_running());
        assert_eq!(pane.command, "RUST_LOG=debug cargo run");

        manager.update_status(&id, PaneStatus::Exited);
        assert!(manager.get(&id).unwrap().is_exited());
        assert!(manager.contains(&id));

        let removed = manager.remove(&id);
        assert!(removed.is_some());
        assert!(manager.is_empty());
    }

    #[test]
    fn test_multiple_panes_independent_status() {
        let mut manager = PaneManager::new();

        let id1 = manager.create_pane("server", Some("server")).unwrap();
        let id2 = manager.create_pane("client", Some("client")).unwrap();

        assert!(manager.get(&id1).unwrap().is_running());
        assert!(manager.get(&id2).unwrap().is_running());

        manager.update_status(&id2, PaneStatus::Exited);

        assert!(manager.get(&id1).unwrap().is_running());
        assert!(manager.get(&id2).unwrap().is_exited());
    }

    #[test]
    fn test_manager_default() {
        let manager = PaneManager::default();
        assert!(manager.is_empty());
    }
}
