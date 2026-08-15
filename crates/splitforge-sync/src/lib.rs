//! # splitforge-sync
//!
//! Opt-in outbound integrations. Never on the read path (ADR-0006).
//!
//! **Status:** empty. This is the Milestone 0 planning scaffold — see
//! `docs/roadmap.md` for what lands here and when.
//!
//! ## Boundaries
//!
//! - **May depend on:** splitforge-domain, splitforge-export
//! - **Must never depend on:** splitforge-engine, splitforge-llrp
//!
//! These rules come from ADR-0001 and are tabulated in `docs/architecture.md`.
//! They are enforced by `crates/splitforge-testkit/tests/dependency_rules.rs`, not left to
//! review (ADR-0012).
