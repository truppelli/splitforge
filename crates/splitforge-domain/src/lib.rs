//! # splitforge-domain
//!
//! Pure domain types, invariants, and port traits for SplitForge.
//!
//! **Status:** empty. This is the Milestone 0 planning scaffold — see
//! `docs/roadmap.md` for what lands here and when.
//!
//! ## Boundaries
//!
//! - **May depend on:** nothing else in this workspace
//! - **Must never depend on:** any I/O crate — no database, HTTP, async runtime, or filesystem
//!
//! These rules come from ADR-0001 and are tabulated in `docs/architecture.md`.
//! Enforcing them in CI is an open question (Q6 in `docs/open-questions.md`).
