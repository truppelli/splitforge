//! # splitforge-edge
//!
//! Raspberry Pi production service: the composition root that wires every adapter.
//!
//! **Status:** empty. This is the Milestone 0 planning scaffold — see
//! `docs/roadmap.md` for what lands here and when.
//!
//! ## Boundaries
//!
//! - **May depend on:** every crate in the workspace
//! - **Must never depend on:** nothing — this is the only crate that knows all concrete implementations
//!
//! These rules come from ADR-0001 and are tabulated in `docs/architecture.md`.
//! Enforcing them in CI is an open question (Q6 in `docs/open-questions.md`).

fn main() {
    println!("splitforge-edge is not implemented yet. See docs/roadmap.md.");
}
