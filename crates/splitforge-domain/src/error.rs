//! Domain errors.

use thiserror::Error;

use crate::ids::ChipId;

/// An invariant of the domain model was violated.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DomainError {
    /// One chip is assigned to more than one participant at the same instant.
    ///
    /// Not recoverable by guessing. Ambiguous chip assignment produces wrong results for
    /// a specific named person, so it is surfaced at configuration time rather than
    /// resolved silently at derivation time.
    #[error("chip {chip} is assigned to more than one participant at the same time")]
    OverlappingChipAssignment {
        /// The chip with conflicting assignments.
        chip: ChipId,
    },
}
