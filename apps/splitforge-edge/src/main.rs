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
//! ## It does not write
//!
//! Nothing here appends to the journal. That is [S10](../../../docs/threat-model.md):
//! *"API failure must never stop journaling. The read path does not traverse the API."*
//! Today that is trivially true because nothing writes at all; when Milestone 3 gives this
//! process a reader, the ordering in [architecture § 3](../../../docs/architecture.md)
//! still puts the durable write before anything the API can observe.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Parser;
use splitforge_api::{Health, HealthSource};
use splitforge_domain::RawReadJournal;
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
/// specific reason — every query behind it is a counting query or a `statvfs`, bounded in
/// time regardless of how long the event has run, and there is exactly one endpoint
/// contending for it. If either of those stops being true this needs `spawn_blocking`.
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
