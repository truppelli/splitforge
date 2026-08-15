//! # splitforge-llrp
//!
//! LLRP reader adapter: binary protocol parsing, connection lifecycle, and reconnect.
//!
//! **Status:** empty. This is the Milestone 0 planning scaffold — see
//! `docs/roadmap.md` for what lands here and when.
//!
//! ## Boundaries
//!
//! - **May depend on:** splitforge-reader, splitforge-domain
//! - **Must never depend on:** splitforge-engine, splitforge-storage, splitforge-api
//!
//! These rules come from ADR-0001 and are tabulated in `docs/architecture.md`.
//! Enforcing them in CI is an open question (Q6 in `docs/open-questions.md`).
