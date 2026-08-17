//! # splitforge-domain
//!
//! Pure domain types, invariants, and port traits for SplitForge.
//!
//! ## Boundaries
//!
//! - **May depend on:** nothing else in this workspace
//! - **Must never depend on:** any I/O crate — no database, HTTP, async runtime, or filesystem
//!
//! These rules come from ADR-0001 and are tabulated in `docs/architecture.md`.
//! They are enforced by `crates/splitforge-testkit/tests/dependency_rules.rs`, not left to
//! review (ADR-0012).
//!
//! ## The one idea this crate encodes
//!
//! Data is either **evidence** or **derivation**, and the two are never confused:
//!
//! | | Evidence | Derivation |
//! |---|---|---|
//! | Types | [`RawRead`], [`StoredRawRead`] | [`AcceptedRead`], [`TimingEvent`], [`Derivation`] |
//! | Mutability | Append-only, never rewritten | Regenerated freely |
//! | Identifiers | Random v4, minted once | Deterministic v5, reproducible |
//!
//! Every derived record references the evidence that produced it, so a finish time can be
//! walked back to the bytes a reader sent. See `docs/timing-model.md`.

// No panicking on any path reachable during an event: a corrupt frame, a missing
// field, or an out-of-range value must become an error the caller can act on, never a
// timer that stops mid-race. Test code is exempt, where panicking on the unexpected is
// the point. See CONTRIBUTING.md, "Code standards".
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod derived;
mod entity;
mod error;
mod ids;
mod policy;
mod ports;
mod read;
mod result;

pub use derived::{
    AcceptedRead, Derivation, RejectedRead, RejectionReason, TimingEvent, TimingEventOrigin,
};
pub use entity::{
    AntennaMap, Checkpoint, CheckpointKind, ChipAssignment, ChipRegistry, Event, Participant, Race,
    RaceConfig, RaceSession, Reader, SessionAction,
};
pub use error::DomainError;
pub use ids::{
    AcceptedReadId, Bib, CheckpointId, ChipId, EventId, ParticipantId, RaceId, RawReadId, ReaderId,
    ResultRevisionId, SPLITFORGE_NAMESPACE, StatusDeclarationId, TimingEventId,
};
pub use policy::{
    CheckpointPolicy, DEFAULT_MIN_INTERVAL_MS, SelectionRule, TimestampTrust, TimingPolicy,
};
pub use ports::{JournalError, RawReadJournal};
pub use read::{DeviceClockState, FallbackReason, RawRead, StoredRawRead, TimestampSource};
pub use result::{
    ResultEntry, ResultFlag, ResultRevision, ResultStatus, RevisionStatus, ScoringPolicy,
    StartMode, StatusDeclaration, StatusSource,
};
