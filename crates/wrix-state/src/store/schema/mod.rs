//! The single supported host control schema; schema changes require explicit versioning.

pub(super) const DDL: &str = r"CREATE TABLE authority (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                generation BLOB NOT NULL CHECK(length(generation) = 32),
                admission_initialized INTEGER NOT NULL CHECK(admission_initialized IN (0,1)),
                scope_policy INTEGER NOT NULL CHECK(scope_policy IN (0,1))
             );
             CREATE TABLE integration (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                slot TEXT NOT NULL, executable BLOB NOT NULL
             );
             CREATE TABLE binding (
                native_key TEXT PRIMARY KEY, slot TEXT NOT NULL, membership TEXT NOT NULL,
                primary_root BLOB NOT NULL, primary_evidence BLOB NOT NULL,
                UNIQUE(slot, membership)
             );
             CREATE TABLE participant (
                checkout BLOB PRIMARY KEY, native_key TEXT NOT NULL REFERENCES binding(native_key),
                evidence BLOB NOT NULL
             );
             CREATE TABLE namespace_metadata (
                native_key TEXT NOT NULL, owner TEXT NOT NULL, value BLOB NOT NULL,
                PRIMARY KEY(native_key, owner)
             );";
