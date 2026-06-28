//! Pane state management
//!
//! This module tracks the state of all tmux panes created by the MCP server.
//! It provides unique ID generation, status tracking, and pane lifecycle management.

use std::collections::HashMap;

/// Status of a pane
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneStatus {
    /// Process is still running
    Running,
    /// Process has exited (pane remains visible per tmux remain-on-exit)
    Exited,
}

impl PaneStatus {
    /// Convert status to string representation
    pub const fn as_str(self) -> &'static str {
        match self {
            PaneStatus::Running => "running",
            PaneStatus::Exited => "exited",
        }
    }
}

impl std::fmt::Display for PaneStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", (*self).as_str())
    }
}

/// State of a single pane
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneState {
    /// Unique pane identifier (debug-N format)
    pub id: String,
    /// Human-readable name (may be same as id if not specified)
    pub name: String,
    /// Current status of the pane
    pub status: PaneStatus,
    /// Command that was executed in the pane
    pub command: String,
}

impl PaneState {
    /// Create a new `PaneState`
    pub const fn new(id: String, name: String, command: String) -> Self {
        Self {
            id,
            name,
            status: PaneStatus::Running,
            command,
        }
    }

    /// Update the pane status
    pub const fn set_status(&mut self, status: PaneStatus) {
        self.status = status;
    }

    /// Check if the pane is running
    #[cfg(test)]
    pub fn is_running(&self) -> bool {
        self.status == PaneStatus::Running
    }

    /// Check if the pane has exited
    #[cfg(test)]
    pub fn is_exited(&self) -> bool {
        self.status == PaneStatus::Exited
    }
}

/// Manages all pane state for the MCP server
#[derive(Debug)]
pub struct PaneManager {
    /// Map of pane ID to pane state
    panes: HashMap<String, PaneState>,
    /// Counter for generating unique IDs
    next_id: u64,
}

impl PaneManager {
    /// Create a new `PaneManager`
    pub fn new() -> Self {
        Self {
            panes: HashMap::new(),
            next_id: 1,
        }
    }

    /// Generate a unique pane ID in debug-N format
    pub fn generate_id(&mut self) -> String {
        let id = format!("debug-{}", self.next_id);
        self.next_id += 1;
        id
    }

    /// Register a new pane with the manager
    ///
    /// Returns the pane ID that was used (either generated or the provided name)
    pub fn create_pane(&mut self, command: &str, name: Option<&str>) -> String {
        let id = self.generate_id();
        let display_name = name.unwrap_or(&id).to_string();

        let state = PaneState::new(id.clone(), display_name, command.to_string());
        self.panes.insert(id.clone(), state);

        id
    }

    /// Get a pane by its ID
    #[cfg(test)]
    pub fn get(&self, pane_id: &str) -> Option<&PaneState> {
        self.panes.get(pane_id)
    }

    /// Get a mutable reference to a pane by its ID
    #[cfg(test)]
    pub fn get_mut(&mut self, pane_id: &str) -> Option<&mut PaneState> {
        self.panes.get_mut(pane_id)
    }

    /// Check if a pane exists
    pub fn contains(&self, pane_id: &str) -> bool {
        self.panes.contains_key(pane_id)
    }

    /// Remove a pane from tracking (called when pane is killed)
    pub fn remove(&mut self, pane_id: &str) -> Option<PaneState> {
        self.panes.remove(pane_id)
    }

    /// Update a pane's status
    pub fn update_status(&mut self, pane_id: &str, status: PaneStatus) -> bool {
        self.panes.get_mut(pane_id).is_some_and(|pane| {
            pane.set_status(status);
            true
        })
    }

    /// Get all panes as an iterator
    pub fn iter(&self) -> impl Iterator<Item = &PaneState> {
        self.panes.values()
    }

    /// Get the number of tracked panes
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.panes.len()
    }

    /// Check if there are no tracked panes
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.panes.is_empty()
    }

    /// Get all pane IDs
    #[cfg(test)]
    pub fn pane_ids(&self) -> Vec<String> {
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

    // --- PaneStatus Tests ---

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

    // --- PaneState Tests ---

    #[test]
    fn test_pane_state_new() {
        let state = PaneState::new(
            "debug-1".to_string(),
            "server".to_string(),
            "cargo run".to_string(),
        );

        assert_eq!(state.id, "debug-1");
        assert_eq!(state.name, "server");
        assert_eq!(state.command, "cargo run");
        assert_eq!(state.status, PaneStatus::Running);
    }

    #[test]
    fn test_pane_state_initial_status_is_running() {
        let state = PaneState::new(
            "debug-1".to_string(),
            "test".to_string(),
            "echo hello".to_string(),
        );

        assert!(state.is_running());
        assert!(!state.is_exited());
    }

    #[test]
    fn test_pane_state_set_status() {
        let mut state = PaneState::new(
            "debug-1".to_string(),
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
            "debug-1".to_string(),
            "test".to_string(),
            "echo hello".to_string(),
        );

        // Initial state: Running
        assert_eq!(state.status, PaneStatus::Running);

        // Transition to Exited
        state.set_status(PaneStatus::Exited);
        assert_eq!(state.status, PaneStatus::Exited);

        // Can transition back to Running (e.g., if process restarts)
        state.set_status(PaneStatus::Running);
        assert_eq!(state.status, PaneStatus::Running);
    }

    #[test]
    fn test_pane_state_clone() {
        let state = PaneState::new(
            "debug-1".to_string(),
            "server".to_string(),
            "cargo run".to_string(),
        );

        let cloned = state.clone();
        assert_eq!(state, cloned);
    }

    // --- PaneManager ID Generation Tests ---

    #[test]
    fn test_manager_generate_id_format() {
        let mut manager = PaneManager::new();

        let id1 = manager.generate_id();
        assert!(id1.starts_with("debug-"));
    }

    #[test]
    fn test_manager_generate_id_sequential() {
        let mut manager = PaneManager::new();

        let id1 = manager.generate_id();
        let id2 = manager.generate_id();
        let id3 = manager.generate_id();

        assert_eq!(id1, "debug-1");
        assert_eq!(id2, "debug-2");
        assert_eq!(id3, "debug-3");
    }

    #[test]
    fn test_manager_generate_id_unique() {
        let mut manager = PaneManager::new();
        let mut ids = std::collections::HashSet::new();

        for _ in 0..100 {
            let id = manager.generate_id();
            assert!(ids.insert(id), "Generated duplicate ID");
        }
    }

    // --- PaneManager Pane Creation Tests ---

    #[test]
    fn test_manager_create_pane_without_name() {
        let mut manager = PaneManager::new();

        let id = manager.create_pane("cargo run", None);

        assert_eq!(id, "debug-1");
        let pane = manager.get(&id).unwrap();
        assert_eq!(pane.name, "debug-1"); // Name defaults to ID
        assert_eq!(pane.command, "cargo run");
    }

    #[test]
    fn test_manager_create_pane_with_name() {
        let mut manager = PaneManager::new();

        let id = manager.create_pane("cargo run", Some("server"));

        assert_eq!(id, "debug-1");
        let pane = manager.get(&id).unwrap();
        assert_eq!(pane.name, "server"); // Custom name
        assert_eq!(pane.command, "cargo run");
    }

    #[test]
    fn test_manager_create_multiple_panes() {
        let mut manager = PaneManager::new();

        let id1 = manager.create_pane("cargo run", Some("server"));
        let id2 = manager.create_pane("bash", Some("client"));
        let id3 = manager.create_pane("tail -f log", None);

        assert_eq!(manager.len(), 3);

        let pane1 = manager.get(&id1).unwrap();
        let pane2 = manager.get(&id2).unwrap();
        let pane3 = manager.get(&id3).unwrap();

        assert_eq!(pane1.name, "server");
        assert_eq!(pane2.name, "client");
        assert_eq!(pane3.name, "debug-3");
    }

    // --- PaneManager Lookup Tests ---

    #[test]
    fn test_manager_get_existing_pane() {
        let mut manager = PaneManager::new();
        let id = manager.create_pane("cargo run", Some("server"));

        let pane = manager.get(&id);
        assert!(pane.is_some());
        assert_eq!(pane.unwrap().name, "server");
    }

    #[test]
    fn test_manager_get_nonexistent_pane() {
        let manager = PaneManager::new();

        let pane = manager.get("debug-999");
        assert!(pane.is_none());
    }

    #[test]
    fn test_manager_get_mut() {
        let mut manager = PaneManager::new();
        let id = manager.create_pane("cargo run", Some("server"));

        let pane = manager.get_mut(&id).unwrap();
        pane.set_status(PaneStatus::Exited);

        // Verify change persisted
        assert!(manager.get(&id).unwrap().is_exited());
    }

    #[test]
    fn test_manager_contains() {
        let mut manager = PaneManager::new();
        let id = manager.create_pane("cargo run", None);

        assert!(manager.contains(&id));
        assert!(!manager.contains("nonexistent"));
    }

    // --- PaneManager Remove Tests ---

    #[test]
    fn test_manager_remove_existing_pane() {
        let mut manager = PaneManager::new();
        let id = manager.create_pane("cargo run", Some("server"));

        assert!(manager.contains(&id));

        let removed = manager.remove(&id);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().name, "server");

        assert!(!manager.contains(&id));
    }

    #[test]
    fn test_manager_remove_nonexistent_pane() {
        let mut manager = PaneManager::new();

        let removed = manager.remove("debug-999");
        assert!(removed.is_none());
    }

    #[test]
    fn test_manager_remove_does_not_affect_other_panes() {
        let mut manager = PaneManager::new();
        let id1 = manager.create_pane("cargo run", Some("server"));
        let id2 = manager.create_pane("bash", Some("client"));

        manager.remove(&id1);

        assert!(!manager.contains(&id1));
        assert!(manager.contains(&id2));
        assert_eq!(manager.len(), 1);
    }

    // --- PaneManager Status Update Tests ---

    #[test]
    fn test_manager_update_status_existing() {
        let mut manager = PaneManager::new();
        let id = manager.create_pane("cargo run", None);

        assert!(manager.get(&id).unwrap().is_running());

        let result = manager.update_status(&id, PaneStatus::Exited);
        assert!(result);
        assert!(manager.get(&id).unwrap().is_exited());
    }

    #[test]
    fn test_manager_update_status_nonexistent() {
        let mut manager = PaneManager::new();

        let result = manager.update_status("debug-999", PaneStatus::Exited);
        assert!(!result);
    }

    // --- PaneManager Iteration Tests ---

    #[test]
    fn test_manager_iter() {
        let mut manager = PaneManager::new();
        manager.create_pane("cargo run", Some("server"));
        manager.create_pane("bash", Some("client"));

        assert_eq!(manager.iter().count(), 2);
    }

    #[test]
    fn test_manager_pane_ids() {
        let mut manager = PaneManager::new();
        let id1 = manager.create_pane("cargo run", None);
        let id2 = manager.create_pane("bash", None);

        let ids = manager.pane_ids();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&id1));
        assert!(ids.contains(&id2));
    }

    // --- PaneManager Capacity Tests ---

    #[test]
    fn test_manager_len() {
        let mut manager = PaneManager::new();
        assert_eq!(manager.len(), 0);

        manager.create_pane("cargo run", None);
        assert_eq!(manager.len(), 1);

        manager.create_pane("bash", None);
        assert_eq!(manager.len(), 2);
    }

    #[test]
    fn test_manager_is_empty() {
        let mut manager = PaneManager::new();
        assert!(manager.is_empty());

        let id = manager.create_pane("cargo run", None);
        assert!(!manager.is_empty());

        manager.remove(&id);
        assert!(manager.is_empty());
    }

    // --- State Transition Tests ---

    #[test]
    fn test_full_pane_lifecycle() {
        let mut manager = PaneManager::new();

        // Create pane
        let id = manager.create_pane("RUST_LOG=debug cargo run", Some("server"));
        assert_eq!(manager.len(), 1);

        // Verify initial state
        let pane = manager.get(&id).unwrap();
        assert!(pane.is_running());
        assert_eq!(pane.command, "RUST_LOG=debug cargo run");

        // Process exits
        manager.update_status(&id, PaneStatus::Exited);
        assert!(manager.get(&id).unwrap().is_exited());

        // Pane is still there (for output capture)
        assert!(manager.contains(&id));

        // Kill pane (explicit cleanup)
        let removed = manager.remove(&id);
        assert!(removed.is_some());
        assert!(manager.is_empty());
    }

    #[test]
    fn test_multiple_panes_independent_status() {
        let mut manager = PaneManager::new();

        let id1 = manager.create_pane("server", Some("server"));
        let id2 = manager.create_pane("client", Some("client"));

        // Both start running
        assert!(manager.get(&id1).unwrap().is_running());
        assert!(manager.get(&id2).unwrap().is_running());

        // Client exits, server still running
        manager.update_status(&id2, PaneStatus::Exited);

        assert!(manager.get(&id1).unwrap().is_running());
        assert!(manager.get(&id2).unwrap().is_exited());
    }

    // --- Default Trait Test ---

    #[test]
    fn test_manager_default() {
        let manager = PaneManager::default();
        assert!(manager.is_empty());
    }
}
