//! # splitforge-api
//!
//! The local API: a health endpoint served over a Unix domain socket.
//!
//! ## Boundaries
//!
//! - **May depend on:** splitforge-domain, splitforge-engine, splitforge-results, splitforge-export
//! - **Must never depend on:** splitforge-llrp
//!
//! These rules come from ADR-0001 and are tabulated in `docs/architecture.md`.
//! They are enforced by `crates/splitforge-testkit/tests/dependency_rules.rs`, not left to
//! review (ADR-0012).
//!
//! ## It does not bind a port
//!
//! ADR-0021 settles the question this crate was blocked on for two milestones. The API
//! listens on a filesystem socket and performs **no authentication of its own**, because
//! the authentication problem is better solved by not being reachable: a process that binds
//! no port cannot be called by anything on an event LAN, so there is no token to leak and
//! no certificate to expire on race morning. Access control is the socket's file
//! permissions, and remote access is the SSH the Pi already runs.
//!
//! There is deliberately no code path in this crate that opens a TCP listener. Not on
//! `0.0.0.0`, and not on `127.0.0.1` either — a loopback listener is one line away from a
//! LAN listener, and the absence of the code is the control.
//!
//! ## It does not open the database
//!
//! The dependency rules put `splitforge-storage` out of reach, which is the hexagonal
//! design doing its job rather than an inconvenience. This crate owns the *shape* of the
//! answer — [`Health`] — and [`splitforge-edge`] owns getting one, by implementing
//! [`HealthSource`]. That keeps the wire contract testable with no database at all, which
//! is why most of the tests below run on every platform rather than only on the target.
//!
//! [`splitforge-edge`]: https://github.com/truppelli/splitforge/tree/main/apps/splitforge-edge

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::{Deserialize, Serialize};

#[cfg(unix)]
mod socket;

#[cfg(unix)]
pub use socket::{SOCKET_MODE, ServeError, serve_on_socket};

/// Where the socket lives when nothing says otherwise.
///
/// Under `/run` because it is a tmpfs: a stale socket cannot survive a reboot, so the only
/// case that needs cleaning up by hand is a crash rather than a power cut.
///
/// Defined outside the `#[cfg(unix)]` module on purpose. It is a string, and the service's
/// argument parser needs a default on every build — including the ones that cannot serve.
pub const DEFAULT_SOCKET_PATH: &str = "/run/splitforge/api.sock";

/// Whether the device is working.
///
/// Two values rather than a spectrum. An operator at a checkpoint can act on "something is
/// wrong"; they cannot act on a severity score, and a monitor that has to interpret one
/// will interpret it wrongly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Nothing detected is wrong.
    Ok,
    /// Something is wrong that this endpoint can see. See [`Health::degraded_by`].
    Degraded,
}

/// What the running service reports about itself.
///
/// Every field here has to be answerable from a counting query, a schema lookup, or a
/// filesystem call. Health gets polled, and it gets polled hardest when the device is
/// already struggling — an endpoint whose cost grows with the length of the event becomes
/// the outage it was added to detect. Nothing in this struct walks the journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    /// Whether anything detected is wrong.
    pub status: Status,
    /// What is wrong, in the operator's terms. Empty exactly when `status` is `ok`.
    pub degraded_by: Vec<String>,
    /// The build serving this.
    pub version: String,
    /// How long this process has been up. Restarts are visible as this resetting.
    pub uptime_seconds: u64,
    /// The database's file name. Never its path — a path names a person on most machines.
    pub database: String,
    /// Schema version, which is the first thing to check against a build.
    pub schema_version: i64,
    /// Rows in `raw_reads`, from `SELECT COUNT(*)`.
    pub raw_reads: u64,
    /// Free space on the volume holding the database.
    pub free_mb: Option<u64>,
    /// The configured floor below which a race will not start.
    pub min_free_mb: u64,
    /// Whether a race would be allowed to start right now.
    pub above_floor: Option<bool>,
    /// Wall-clock discontinuities this device has recorded, from `SELECT COUNT(*)`.
    ///
    /// Cumulative for the life of the database, not for the life of this process: a step
    /// observed before the last restart still moved every timestamp written around it.
    pub clock_steps: u64,
    // Deliberately no reader connection state. No reader protocol exists until Milestone 3,
    // and a `"reader": "connected"` field that is always true because nothing ever connects
    // would be the most dangerous kind of health check: one that reports the absence of
    // monitoring as the absence of a problem. See `docs/hardware-support.md`.
}

impl Health {
    /// A report with nothing wrong.
    #[must_use]
    pub fn ok(
        version: impl Into<String>,
        uptime_seconds: u64,
        database: impl Into<String>,
        schema_version: i64,
        raw_reads: u64,
        min_free_mb: u64,
    ) -> Self {
        Self {
            status: Status::Ok,
            degraded_by: Vec::new(),
            version: version.into(),
            uptime_seconds,
            database: database.into(),
            schema_version,
            raw_reads,
            free_mb: None,
            min_free_mb,
            above_floor: None,
            clock_steps: 0,
        }
    }

    /// Records a reason the device is not healthy, flipping the status.
    ///
    /// Additive rather than a setter, because the interesting case is the second reason: a
    /// device that is both low on disk and unable to read its own journal should say both,
    /// and an API that overwrote the first would report the least alarming one.
    pub fn degrade(&mut self, reason: impl Into<String>) {
        self.status = Status::Degraded;
        self.degraded_by.push(reason.into());
    }

    /// Whether anything is wrong.
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        self.status == Status::Degraded
    }
}

/// Something that can answer "is this device working?".
///
/// Returns a [`Health`] rather than a `Result`, deliberately. An implementation that cannot
/// read the database reports that as a degradation and the endpoint still answers, because
/// the one thing a health check must never do is fail to say anything: a monitor cannot
/// tell "the service is wedged" apart from "the service could not phrase its complaint",
/// and the two need very different responses.
pub trait HealthSource: Send + Sync + 'static {
    /// Takes a reading. Must be cheap enough to call every few seconds, forever.
    fn health(&self) -> Health;
}

impl<F> HealthSource for F
where
    F: Fn() -> Health + Send + Sync + 'static,
{
    fn health(&self) -> Health {
        self()
    }
}

/// The API's routes.
///
/// One endpoint. It will grow at Milestone 3, when reader connection state exists and lives
/// in this process rather than in the database — which is the thing a health endpoint can
/// report and a one-shot CLI command cannot.
pub fn router(source: Arc<dyn HealthSource>) -> Router {
    Router::new()
        .route("/health", get(health))
        .with_state(source)
}

/// `GET /health`.
///
/// Degraded answers carry HTTP 503 so that `curl --fail`, a systemd watchdog, and a
/// monitoring script all agree without parsing the body. The body says the same thing for
/// anyone who is reading it.
async fn health(State(source): State<Arc<dyn HealthSource>>) -> Response {
    let health = source.health();
    let code = if health.is_degraded() {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    (code, axum::Json(health)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Health {
        Health::ok("0.0.0", 42, "event.db", 4, 638, 256)
    }

    #[test]
    fn a_healthy_report_carries_no_reasons() {
        let health = sample();
        assert_eq!(health.status, Status::Ok);
        assert!(health.degraded_by.is_empty());
        assert!(!health.is_degraded());
    }

    #[test]
    fn degrading_accumulates_reasons_rather_than_replacing_them() {
        // The second reason is the point. A device that is both low on disk and unable to
        // read its journal has two problems, and reporting the tamer one is how an operator
        // fixes the wrong thing.
        let mut health = sample();
        health.degrade("12 MB free, below the 256 MB floor");
        health.degrade("the journal could not be counted");

        assert_eq!(health.status, Status::Degraded);
        assert_eq!(health.degraded_by.len(), 2);
        assert!(health.is_degraded());
    }

    #[test]
    fn health_never_claims_a_reader_is_connected() {
        // No reader protocol exists until Milestone 3. A field that is always "connected"
        // because nothing ever connects reports the absence of monitoring as the absence of
        // a problem, which is worse than reporting nothing.
        let json = serde_json::to_value(sample()).expect("serialize");
        assert!(json.get("reader").is_none(), "got: {json}");
        assert!(json.get("readers").is_none(), "got: {json}");
        assert!(json.get("connected").is_none(), "got: {json}");
    }

    #[test]
    fn the_report_names_the_database_file_and_not_its_path() {
        let json = serde_json::to_value(sample()).expect("serialize");
        assert_eq!(json["database"], "event.db");
    }

    #[test]
    fn a_closure_is_a_health_source() {
        // So a test — or a future caller with state of its own — does not have to declare a
        // type to answer one question.
        let source: Arc<dyn HealthSource> = Arc::new(sample);
        assert_eq!(source.health().status, Status::Ok);
    }

    #[test]
    fn the_report_round_trips_through_json() {
        let mut health = sample();
        health.degrade("12 MB free");
        health.free_mb = Some(12);
        health.above_floor = Some(false);

        let text = serde_json::to_string(&health).expect("serialize");
        let back: Health = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(health, back);
    }
}
