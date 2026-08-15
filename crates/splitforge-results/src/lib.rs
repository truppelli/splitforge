//! # splitforge-results
//!
//! Result revisions, placement ranking, and DNS/DNF/DQ status handling.
//!
//! **Status:** empty. This is the Milestone 0 planning scaffold — see
//! `docs/roadmap.md` for what lands here and when.
//!
//! ## Boundaries
//!
//! - **May depend on:** splitforge-domain
//! - **Must never depend on:** splitforge-llrp, splitforge-api, splitforge-sync
//!
//! These rules come from ADR-0001 and are tabulated in `docs/architecture.md`.
//! They are enforced by `crates/splitforge-testkit/tests/dependency_rules.rs`, not left to
//! review (ADR-0012).
