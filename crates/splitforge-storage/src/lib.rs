//! # splitforge-storage
//!
//! SQLite persistence for SplitForge: schema, migrations, repositories, and backups.
//!
//! ## Boundaries
//!
//! - **May depend on:** splitforge-domain
//! - **Must never depend on:** splitforge-engine, splitforge-llrp, splitforge-api
//!
//! These rules come from ADR-0001 and are tabulated in `docs/architecture.md`.
//! They are enforced by `crates/splitforge-testkit/tests/dependency_rules.rs`, not left to
//! review (ADR-0012).
//!
//! ## Durability contract
//!
//! [`SqliteJournal::append`] returns only once the read is durable — WAL mode with
//! `synchronous = FULL`, so a committed transaction has been fsynced before the call
//! returns. A caller may not acknowledge, count, or forward a read before that point.
//!
//! Power loss during a write is an expected event on a battery-powered Pi, not an edge
//! case, so this cost is accepted deliberately (ADR-0003).

// No panicking on any path reachable during an event: a corrupt frame, a missing
// field, or an out-of-range value must become an error the caller can act on, never a
// timer that stops mid-race. Test code is exempt, where panicking on the unexpected is
// the point. See CONTRIBUTING.md, "Code standards".
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod journal;
mod migrations;

use thiserror::Error;

pub use journal::SqliteJournal;
pub use migrations::{MIGRATIONS, Migration, SCHEMA_VERSION};

/// A storage-layer failure.
#[derive(Debug, Error)]
pub enum StorageError {
    /// SQLite reported an error.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// A stored value could not be decoded back into a domain type.
    ///
    /// Indicates corruption or a schema/binary mismatch, both of which are worth shouting
    /// about rather than papering over with a default.
    #[error("stored value could not be decoded: {0}")]
    Decode(String),

    /// A timestamp fell outside the representable range.
    #[error("timestamp out of representable range")]
    TimestampOutOfRange,

    /// The database was created by a newer build than this one.
    #[error("database schema version {found} is newer than this build supports ({supported})")]
    SchemaTooNew {
        /// Version found in the database.
        found: i64,
        /// Version this binary understands.
        supported: i64,
    },
}

impl From<StorageError> for splitforge_domain::JournalError {
    fn from(error: StorageError) -> Self {
        match error {
            StorageError::Decode(message) => Self::Corrupt(message),
            StorageError::Sqlite(rusqlite::Error::SqliteFailure(_, Some(ref message)))
                if message.contains("append-only") =>
            {
                Self::AppendOnlyViolation {
                    operation: message.clone(),
                }
            }
            other => Self::Backend(other.to_string()),
        }
    }
}
