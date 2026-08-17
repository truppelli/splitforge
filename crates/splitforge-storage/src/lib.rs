//! # splitforge-storage
//!
//! SQLite persistence for SplitForge: schema, migrations, repositories, and backups.
//!
//! ## Two stores, one file
//!
//! [`SqliteJournal`] holds evidence and never rewrites it. [`ConfigStore`] holds
//! configuration and rewrites it constantly. They share a file, a schema, and an open path
//! (`connection`), and nothing else — the separation is the point, and it is why the type
//! that can `UPDATE` has no way to reach `raw_reads`.
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

mod backup;
mod config;
mod connection;
mod journal;
mod migrations;
mod results;
mod sidecar;
mod space;

use thiserror::Error;

pub use backup::{
    BackupReport, RestoreReport, create as create_backup, restore as restore_backup,
    sidecar_path_for,
};
pub use config::{AuditEntry, ConfigStore, ImportSummary, RaceSelection, parse_checkpoint_kind};
pub use journal::{RecoveryReport, SidecarStatus, SqliteJournal};
pub use migrations::{MIGRATIONS, Migration, SCHEMA_VERSION};
pub use results::{ResultStore, RevisionSummary};
pub use space::{DEFAULT_MIN_FREE_BYTES, DiskSpace, disk_space};

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

    /// The write-ahead sidecar could not be written, read, or decoded.
    ///
    /// On the append path this is fatal by design. The sidecar is written *before* the
    /// database transaction, so a failure here means the read has no durable copy
    /// anywhere — and reporting that to the caller is the whole contract. Swallowing it
    /// would leave the process quietly running without the backstop it claims to have.
    #[error("sidecar journal: {0}")]
    Sidecar(String),

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
