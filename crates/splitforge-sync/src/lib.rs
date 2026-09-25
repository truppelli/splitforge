//! # splitforge-sync
//!
//! Opt-in outbound integrations. Never on the read path (ADR-0006).
//!
//! **Status:** the RaceDay Connect publish contract and the mapping onto it
//! ([ADR-0039](../../../docs/adr/0039-raceday-connect-publishes-what-splitforge-derived.md)).
//! The outbox and the HTTP transport are the next slice. Nothing in this crate performs I/O
//! yet: every function here is a pure translation from what SplitForge derived to what
//! RaceDay Connect accepts, so it can be tested without a network, a database or a race.
//!
//! - [`raceday::contract`] — the wire format, field for field what RaceDay Connect's
//!   `/api/v1/timing` endpoints read.
//! - [`raceday::course`] — what the operator has to say about the course that SplitForge
//!   does not otherwise know: the distance in metres, and where each split sits.
//! - [`raceday::publish`] — the course manifest, live crossings (restated a runner at a
//!   time, so a crossing SplitForge no longer believes in leaves the board) and result
//!   revisions.
//! - [`raceday::delivery`] — what a response means for the outbox: delivered, retry,
//!   refused for good, or unpaired.
//!
//! ## Boundaries
//!
//! - **May depend on:** splitforge-domain, splitforge-export
//! - **Must never depend on:** splitforge-engine, splitforge-llrp
//!
//! These rules come from ADR-0001 and are tabulated in `docs/architecture.md`.
//! They are enforced by `crates/splitforge-testkit/tests/dependency_rules.rs`, not left to
//! review (ADR-0012).

// No panicking on any path reachable during an event. Publishing runs beside timing, and
// a panic in it must never be the thing that takes the timer down. See CONTRIBUTING.md,
// "Code standards".
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod raceday;
