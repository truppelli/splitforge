//! # splitforge-cli
//!
//! Configuration, diagnostics, exports, backup/restore, and admin commands.
//!
//! **Status:** empty. This is the Milestone 0 planning scaffold — see
//! `docs/roadmap.md` for what lands here and when.
//!
//! ## Boundaries
//!
//! - **May depend on:** any crate except splitforge-llrp internals
//! - **Must never depend on:** nothing specific — but it is a boundary crate, so anyhow stops here
//!
//! These rules come from ADR-0001 and are tabulated in `docs/architecture.md`.
//! Enforcing them in CI is an open question (Q6 in `docs/open-questions.md`).
