//! Ports: the traits adapters implement.
//!
//! Traits are not dependencies, so declaring them here keeps `splitforge-domain` free of
//! I/O while still letting the domain state what it needs. `splitforge-storage` implements
//! [`RawReadJournal`]; nothing in the domain knows SQLite exists.

use thiserror::Error;

use crate::read::{RawRead, StoredRawRead};

/// A failure in the raw read journal.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum JournalError {
    /// The underlying store failed. Carries the backend's message as text so the domain
    /// need not depend on any particular database crate.
    #[error("journal backend failure: {0}")]
    Backend(String),
    /// Something attempted to modify evidence.
    #[error("the raw read journal is append-only; {operation} is not permitted")]
    AppendOnlyViolation {
        /// The attempted operation.
        operation: String,
    },
    /// A stored row could not be decoded back into a [`RawRead`].
    #[error("stored read could not be decoded: {0}")]
    Corrupt(String),
}

/// Append-only storage for raw reads.
///
/// The contract that matters: [`RawReadJournal::append`] returns only after the read is
/// durable. A caller may not acknowledge, count, or forward a read before that point.
/// There is deliberately no `update` and no `delete` — see ADR-0005.
pub trait RawReadJournal {
    /// Appends one read, returning it with the fields storage assigns.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] if the read could not be made durable. A caller receiving
    /// an error must treat the read as lost and say so loudly; it must not proceed as
    /// though the read were recorded.
    fn append(&mut self, read: &RawRead) -> Result<StoredRawRead, JournalError>;

    /// Appends many reads in one transaction.
    ///
    /// Either all of them are durable, or none are.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] if the batch could not be committed.
    fn append_batch(&mut self, reads: &[RawRead]) -> Result<Vec<StoredRawRead>, JournalError>;

    /// Every read in the journal, in insertion order.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] if the journal could not be read or decoded.
    fn read_all(&self) -> Result<Vec<StoredRawRead>, JournalError>;

    /// Reads inserted after `after_seq`, in insertion order.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] if the journal could not be read or decoded.
    fn read_since(&self, after_seq: u64) -> Result<Vec<StoredRawRead>, JournalError>;

    /// How many reads the journal holds.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] if the journal could not be queried.
    fn count(&self) -> Result<u64, JournalError>;
}
