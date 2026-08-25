//! # splitforge-edge
//!
//! The service systemd runs: the composition root that wires every adapter.
//!
//! ## Boundaries
//!
//! - **May depend on:** every crate in the workspace
//! - **Must never depend on:** nothing — this is the only crate that knows all concrete implementations
//!
//! These rules come from ADR-0001 and are tabulated in `docs/architecture.md`.
//! They are enforced by `crates/splitforge-testkit/tests/dependency_rules.rs`, not left to
//! review (ADR-0012).
//!
//! ## What it is today
//!
//! A long-running process that opens the event database and serves
//! [`splitforge-api`](splitforge_api) on a Unix socket (ADR-0021). That is deliberately
//! less than the name suggests: there is no reader to compose until Milestone 3, and this
//! binary claims nothing about one.
//!
//! What it adds over `splitforge status` is **liveness**. A one-shot command answers from
//! the database and cannot tell you whether the service is running; `Restart=always`
//! restarts a crashed process but cannot tell you whether the restarted one is working.
//! Health answers both, and at Milestone 3 it becomes the only thing that can report reader
//! connection state — which lives in this process and in no file.
//!
//! ## The API does not write
//!
//! Serving health appends nothing. That is [S10](../../../docs/threat-model.md):
//! *"API failure must never stop journaling. The read path does not traverse the API."*
//! When Milestone 3 gives this process a reader, the ordering in
//! [architecture § 3](../../../docs/architecture.md) still puts the durable write before
//! anything the API can observe.
//!
//! The *service* does write, in exactly one place: the clock monitor below appends to
//! `clock_steps` when the device's wall clock jumps. That is an observation this process
//! makes and nothing else can — a one-shot command cannot notice a discontinuity it was
//! not running across.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Parser;
use splitforge_api::{Health, HealthSource};
use splitforge_domain::{ClockStep, RawReadJournal, SAMPLE_INTERVAL_MS};
use splitforge_storage::{ConfigStore, SqliteJournal};

/// The SplitForge edge service.
#[derive(Debug, Parser)]
#[command(name = "splitforge-edge", version, about, long_about = None)]
struct Args {
    /// Path to the event database.
    #[arg(
        long,
        value_name = "PATH",
        default_value = "/var/lib/splitforge/event.db"
    )]
    database: PathBuf,

    /// Where to put the API socket.
    ///
    /// There is deliberately no option to listen on a port. See ADR-0021.
    #[arg(long, value_name = "PATH", default_value = splitforge_api::DEFAULT_SOCKET_PATH)]
    socket: PathBuf,
}

/// Everything the health endpoint reads, behind one lock.
///
/// `rusqlite::Connection` is `Send` but not `Sync`, so a shared handle needs a mutex. A
/// blocking lock inside an async handler is normally a smell; it is acceptable here for a
/// specific reason — everything behind it is a counting query, a `statvfs`, or a single-row
/// insert, all bounded in time regardless of how long the event has run. Two tasks contend
/// for it: the health handler and the clock monitor, and the monitor holds it for one
/// `INSERT` at most once per [`SAMPLE_INTERVAL_MS`]. If either of those stops being true
/// this needs `spawn_blocking`.
struct Device {
    database: PathBuf,
    started: Instant,
    stores: Mutex<Stores>,
}

struct Stores {
    journal: SqliteJournal,
    config: ConfigStore,
}

impl HealthSource for Device {
    fn health(&self) -> Health {
        let name = self.database.file_name().map_or_else(
            || "(unnamed)".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        );

        let mut health = Health::ok(
            env!("CARGO_PKG_VERSION"),
            self.started.elapsed().as_secs(),
            name,
            0,
            0,
            0,
        );

        // A poisoned mutex means a previous call panicked while holding it. Reporting that
        // as a degradation is strictly better than panicking again: the endpoint keeps
        // answering, and what it answers is the truth.
        let Ok(stores) = self.stores.lock() else {
            health.degrade("the device state is unreadable after an earlier failure");
            return health;
        };

        match stores.journal.schema_version() {
            Ok(version) => health.schema_version = version,
            Err(error) => health.degrade(format!("the schema version could not be read: {error}")),
        }
        match stores.journal.count() {
            Ok(reads) => health.raw_reads = reads,
            Err(error) => health.degrade(format!("the journal could not be counted: {error}")),
        }

        let floor = match stores.config.min_free_bytes() {
            Ok(floor) => floor,
            Err(error) => {
                health.degrade(format!("the free-space floor could not be read: {error}"));
                splitforge_storage::DEFAULT_MIN_FREE_BYTES
            }
        };
        health.min_free_mb = floor / (1024 * 1024);

        match stores.journal.clock_step_count() {
            Ok(steps) => {
                health.clock_steps = steps;
                if steps > 0 {
                    // Degraded rather than merely reported. A step is not a transient
                    // condition that clears: every timestamp written on the other side of
                    // it was taken from a clock that moved, and the operator needs to know
                    // before they publish, not after somebody disputes a time.
                    let largest = stores
                        .journal
                        .largest_clock_step_ms()
                        .ok()
                        .flatten()
                        .unwrap_or(0);
                    health.degrade(format!(
                        "the device clock has jumped {steps} time(s), the largest by \
                         {largest} ms; run `splitforge doctor` before publishing"
                    ));
                }
            }
            Err(error) => health.degrade(format!("clock steps could not be counted: {error}")),
        }

        match splitforge_storage::disk_space(&self.database) {
            Ok(space) => {
                health.free_mb = Some(space.available_mb());
                health.above_floor = Some(space.is_above(floor));
                if !space.is_above(floor) {
                    health.degrade(format!(
                        "{} MB free, below the {} MB floor; `splitforge race start` will refuse",
                        space.available_mb(),
                        health.min_free_mb
                    ));
                }
            }
            Err(error) => health.degrade(format!("free space could not be measured: {error}")),
        }

        health
    }
}

/// Watches the wall clock for discontinuities, forever.
///
/// The whole mechanism is two clocks and a subtraction. Wall time and monotonic time are
/// sampled together; between two samples they should advance by the same amount, and when
/// they do not, something moved the wall clock — NTP finding a network, an RTC module, an
/// operator with `date`. The Pi 3 has no battery-backed clock ([ADR-0002]), so it boots
/// believing whatever it believed last and *something* is going to correct it.
///
/// Why this lives in the service rather than in `splitforge doctor`: a step is only visible
/// to something that was running across it. A one-shot command sees the clock as it is now
/// and has nothing to compare it against.
///
/// A failure to record is logged and the loop continues. Losing the ability to note that
/// the clock jumped is bad; stopping the process that also serves health is worse, and the
/// next sample re-baselines against a clock that has already moved.
///
/// [ADR-0002]: ../../../docs/adr/0002-raspberry-pi-target.md
async fn watch_the_clock(device: Arc<Device>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(SAMPLE_INTERVAL_MS));

    // `Delay` rather than the default `Burst`: if the whole process is descheduled — which
    // on a loaded Pi is exactly when the clock is also being corrected — `Burst` fires every
    // missed tick back to back, and each of those would compare two samples taken
    // microseconds apart and call the accumulated wall movement a step.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    interval.tick().await;

    let mut baseline = sample();

    loop {
        interval.tick().await;
        let current = sample();

        let monotonic_ms =
            u64::try_from(current.1.saturating_duration_since(baseline.1).as_millis())
                .unwrap_or(u64::MAX);

        if let Some(step) = ClockStep::detect(baseline.0, current.0, monotonic_ms) {
            eprintln!(
                "splitforge-edge: the wall clock moved {} ms relative to elapsed time \
                 ({} -> {})",
                step.step_ms, step.observed_before, step.observed_after
            );

            match device.stores.lock() {
                Ok(mut stores) => {
                    if let Err(error) = stores.journal.record_clock_step(&step) {
                        eprintln!("splitforge-edge: the clock step could not be recorded: {error}");
                    }
                }
                Err(_) => eprintln!(
                    "splitforge-edge: the clock step could not be recorded; device state is \
                     unreadable after an earlier failure"
                ),
            }
        }

        baseline = current;
    }
}

/// Both clocks, read as close together as the machine allows.
fn sample() -> (time::OffsetDateTime, Instant) {
    (time::OffsetDateTime::now_utc(), Instant::now())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Opening the journal reconciles it against the write-ahead sidecar (ADR-0018), which
    // is why this is the writer's entry point even though nothing here writes yet: a
    // service starting after a crash is exactly when reads need replaying.
    let (journal, recovery) = SqliteJournal::open_recovering(&args.database)
        .with_context(|| format!("opening the journal at {}", args.database.display()))?;
    let config = ConfigStore::open(&args.database)
        .with_context(|| format!("opening the configuration at {}", args.database.display()))?;

    // Said out loud rather than swallowed. A service that silently replayed 600 reads on
    // start has just told you something important about the last shutdown, and journald is
    // where somebody will look for it afterwards.
    if recovery.replayed_into_database > 0 || recovery.backfilled_into_sidecar > 0 {
        eprintln!(
            "splitforge-edge: recovered on start — {} read(s) replayed into the database, \
             {} backfilled into the sidecar",
            recovery.replayed_into_database, recovery.backfilled_into_sidecar
        );
    }
    if recovery.found.corrupt_lines > 0 {
        eprintln!(
            "splitforge-edge: {} sidecar line(s) failed their checksum and could not be \
             recovered; run `splitforge doctor`",
            recovery.found.corrupt_lines
        );
    }

    let device = Arc::new(Device {
        database: args.database.clone(),
        started: Instant::now(),
        stores: Mutex::new(Stores { journal, config }),
    });

    eprintln!(
        "splitforge-edge: database {}, socket {}",
        args.database.display(),
        args.socket.display()
    );

    // Detached rather than joined. The clock monitor is a background observation, and a
    // service that refused to serve health because its clock watcher fell over would have
    // traded the smaller problem for the larger one. It ends when the process does.
    tokio::spawn(watch_the_clock(Arc::clone(&device)));

    serve(&args.socket, device).await
}

#[cfg(unix)]
async fn serve(socket: &std::path::Path, device: Arc<Device>) -> Result<()> {
    splitforge_api::serve_on_socket(socket, splitforge_api::router(device), shutdown())
        .await
        .map_err(Into::into)
}

/// Resolves on SIGTERM or SIGINT.
///
/// SIGTERM is what systemd sends on `stop` and on `restart`, so handling it is what makes
/// the difference between a socket removed cleanly and one left on disk for the next start
/// to clear.
#[cfg(unix)]
async fn shutdown() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut term = match signal(SignalKind::terminate()) {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("splitforge-edge: cannot listen for SIGTERM ({error}); running anyway");
            std::future::pending::<()>().await;
            return;
        }
    };

    tokio::select! {
        _ = term.recv() => eprintln!("splitforge-edge: SIGTERM, shutting down"),
        result = tokio::signal::ctrl_c() => match result {
            Ok(()) => eprintln!("splitforge-edge: interrupted, shutting down"),
            Err(error) => eprintln!("splitforge-edge: cannot listen for SIGINT ({error})"),
        },
    }
}

/// The service is Unix-only, and says so rather than pretending.
///
/// [ADR-0002](../../../docs/adr/0002-raspberry-pi-target.md) makes 64-bit Linux the target,
/// and ADR-0021 makes the API a Unix socket. A build on another platform is a development
/// convenience — `cargo test` on a contributor's laptop — and binding a TCP port to make it
/// run would quietly undo the decision on the machine it was made for.
#[cfg(not(unix))]
async fn serve(_socket: &std::path::Path, _device: Arc<Device>) -> Result<()> {
    anyhow::bail!(
        "splitforge-edge serves its API on a Unix socket (ADR-0021) and cannot run on this \
         platform. The target is 64-bit Linux (ADR-0002); use the CLI here instead."
    )
}
