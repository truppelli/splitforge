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
use splitforge_domain::{DeviceClockState, GapDetection};

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
    /// The reader this process was given, and what it has produced.
    ///
    /// This field spent Milestones 0–2 as a comment refusing to exist, on the grounds that
    /// *"a `\"reader\": \"connected\"` field that is always true because nothing ever
    /// connects would be the most dangerous kind of health check: one that reports the
    /// absence of monitoring as the absence of a problem."*
    ///
    /// That warning is the specification for the shape it finally took. [`ReaderKind::None`]
    /// is a distinct value from any connection state, and [`ReaderHealth::state`] is `None`
    /// alongside it — so "this device was never given a reader" cannot be read as "this
    /// device has a working reader", which is the exact confusion the comment forbade.
    pub reader: ReaderHealth,
    /// What the time daemon last said, or `None` before the first sample.
    ///
    /// Sampled on an interval and cached rather than measured per request: `chronyc` blocks
    /// while it retries an unresponsive daemon, and a watchdog polling every few seconds
    /// must never be the thing that waits for it.
    pub clock_source: Option<ClockSource>,
}

/// Which reader the service was composed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReaderKind {
    /// None was configured. **Not a fault** — it is the whole of what this service did
    /// before Milestone 3a, and a device being prepared has no reader yet either.
    None,
    /// A simulated reader. Its reads are synthetic and they enter the journal as evidence
    /// like any other, which is why this is reported rather than hidden.
    Simulated,
}

/// Whether a configured reader is producing reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReaderState {
    /// Producing, or waiting for the next read.
    Connected,
    /// Composed, with no connection to the reader right now.
    ///
    /// The state a serial reader is in before its port opens and between losing it and
    /// getting it back, and the state every reader starts in — a composition root cannot
    /// honestly report a connection it has not been told exists. Distinct from
    /// [`Self::Stopped`] because the provider is still trying: this one is expected to
    /// change on its own, and that one is not.
    Disconnected,
    /// Finished or given up. A simulated reader reaching the end of its script is the
    /// ordinary case; a read path that stopped because a write failed is not, and that one
    /// also degrades the status with a reason.
    Stopped,
}

/// The reader half of a health report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReaderHealth {
    /// Which reader, if any.
    pub kind: ReaderKind,
    /// Its state, or `None` when [`Self::kind`] is [`ReaderKind::None`] and there is
    /// nothing whose state could be described.
    pub state: Option<ReaderState>,
    /// Messages taken off the reader's channel.
    pub reads_received: u64,
    /// Reads the journal has accepted and made durable.
    ///
    /// Deliberately a second number. `docs/architecture.md` § 4: *"Any metric that says
    /// 'reads received' and any metric that says 'reads persisted' are different numbers,
    /// and the gap between them is a monitored quantity."* A single counter incremented
    /// before the write would report reads that a power cut took.
    pub reads_persisted: u64,
    /// The gap this reader is in right now, if it is in one.
    ///
    /// `None` is the ordinary state and means the reader is producing — or that it never
    /// was, which [`Self::kind`] and [`Self::state`] already distinguish. An open gap
    /// degrades the whole report, so a watchdog sees it on the status line without parsing
    /// prose ([ADR-0025](../../../docs/adr/0025-m3a-proves-durability-above-the-transport.md)).
    ///
    /// Read from the journal on every request rather than held in memory, which is what
    /// makes it survive a restart: the row that opened the gap was on disk before the power
    /// went ([ADR-0026](../../../docs/adr/0026-a-reader-gap-is-two-rows.md)).
    pub open_gap: Option<OpenGap>,
}

impl ReaderHealth {
    /// A device with no reader composed.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            kind: ReaderKind::None,
            state: None,
            reads_received: 0,
            reads_persisted: 0,
            open_gap: None,
        }
    }
}

/// The gap a reader is currently in.
///
/// Deliberately not the whole [`splitforge_domain::ReaderGap`]: health answers *is this
/// device in trouble right now*, and the sequence numbers that identify the evidence belong
/// to whoever is reading the evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenGap {
    /// How it was noticed. `suspected` means the stream went quiet, which is
    /// indistinguishable from a checkpoint nobody is crossing — so an operator reading this
    /// is being told what was observed, not what is wrong.
    pub detection: GapDetection,
    /// How long it has been open, in milliseconds, on the monotonic clock.
    pub open_for_ms: u64,
}

/// What the system's time daemon last reported.
///
/// Two fields because *"not measured"* and *"measured, and the clock is bad"* are different
/// facts that call for opposite reactions, and a report that collapsed them would tell an
/// operator their clock was broken because nobody asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockSource {
    /// Which kind of answer this is, as a fixed token: `measured`, `no_daemon_tool`,
    /// `daemon_unreachable`, or `unreadable`. A watchdog branches on this and never on
    /// prose.
    pub measurement: String,
    /// The determined state, or `None` when nothing could be measured.
    ///
    /// Note this is **not** the state stamped on reads. A read's column is not nullable, so
    /// an unmeasured clock is recorded there as `Unsynced`; here the distinction survives,
    /// because an operator can act on it.
    pub state: Option<DeviceClockState>,
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
            reader: ReaderHealth::none(),
            clock_source: None,
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
/// One endpoint. It grew at Milestone 3a, when reader connection state started existing in
/// this process and in no file — which is the thing a health endpoint can report and a
/// one-shot CLI command structurally cannot.
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
        // This test used to assert that no `reader` field existed at all, because none could
        // be honest before Milestone 3a: a field that is always "connected" because nothing
        // ever connects reports the absence of monitoring as the absence of a problem.
        //
        // The field exists now, and the claim it guards is unchanged — a device with no
        // reader must never look like a device with a working one. What it asserts is the
        // *shape* that makes that impossible, rather than the absence that used to.
        let json = serde_json::to_value(sample()).expect("serialize");

        assert_eq!(json["reader"]["kind"], "none", "got: {json}");
        assert!(
            json["reader"]["state"].is_null(),
            "a device with no reader has no state to report: {json}"
        );
        // The two counters are the other half. Zero here is a fact about a device that has
        // taken no reads, not a placeholder standing in for an unanswered question.
        assert_eq!(json["reader"]["reads_received"], 0);
        assert_eq!(json["reader"]["reads_persisted"], 0);
    }

    #[test]
    fn a_reader_that_stopped_is_not_a_reader_that_was_never_configured() {
        // The distinction the whole shape exists for. Both of these are "not producing
        // reads", and an operator's response to them is opposite: one device needs a reader
        // attached, the other needs someone to find out why its reader quit.
        let mut never = sample();
        never.reader = ReaderHealth::none();

        let mut stopped = sample();
        stopped.reader = ReaderHealth {
            kind: ReaderKind::Simulated,
            state: Some(ReaderState::Stopped),
            reads_received: 638,
            reads_persisted: 638,
            open_gap: None,
        };

        assert_ne!(never.reader.kind, stopped.reader.kind);
        assert_ne!(never.reader.state, stopped.reader.state);
    }

    #[test]
    fn a_clock_that_was_never_sampled_is_not_a_clock_that_is_broken() {
        // The same rule as the reader field, applied to the time source: `None` means
        // nobody asked, and it must not serialize into anything a watchdog could read as a
        // measurement. `state` inside a reading carries the same distinction one level down.
        let unsampled = serde_json::to_value(sample()).expect("serialize");
        assert!(unsampled["clock_source"].is_null(), "got: {unsampled}");

        let mut asked = sample();
        asked.clock_source = Some(ClockSource {
            measurement: "no_daemon_tool".to_owned(),
            state: None,
        });
        let asked = serde_json::to_value(asked).expect("serialize");

        assert_eq!(asked["clock_source"]["measurement"], "no_daemon_tool");
        assert!(asked["clock_source"]["state"].is_null());
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
