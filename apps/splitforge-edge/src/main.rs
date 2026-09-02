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
//! A long-running process that opens the event database, takes reads off a
//! [`ReaderProvider`] and makes them durable, and serves
//! [`splitforge-api`](splitforge_api) on a Unix socket (ADR-0021).
//!
//! **The only provider it can compose is a simulated one**, because there is no adapter for
//! a physical reader yet — Milestone 3a is gated on buying the module. That is a real read
//! path with synthetic reads on the front of it, not a placeholder: the loop, the ordering,
//! the failure behavior, and the health reporting are the ones a module will arrive into,
//! and the port is what keeps `--simulate` from being a second code path.
//!
//! What it adds over `splitforge status` is **liveness**. A one-shot command answers from
//! the database and cannot tell you whether the service is running; `Restart=always`
//! restarts a crashed process but cannot tell you whether the restarted one is working.
//! Health answers both, and it is now also the only thing that can report reader state —
//! which lives in this process and in no file, so no command that opens the database could
//! see it.
//!
//! ## The API does not write
//!
//! Serving health appends nothing. That is [S10](../../../docs/threat-model.md):
//! *"API failure must never stop journaling. The read path does not traverse the API."* The
//! ordering in [architecture § 3](../../../docs/architecture.md) puts the durable write
//! before anything the API can observe, which is why `reads_persisted` moves only after
//! `append` returns and never before it.
//!
//! The *service* writes in two places, and both are things only a running process can know.
//! The read path appends evidence. The clock monitor appends to `clock_steps` when the
//! device's wall clock jumps — an observation a one-shot command cannot make, because it was
//! not there for the moment before.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Parser;
use splitforge_api::{ClockSource, Health, HealthSource, ReaderHealth, ReaderKind, ReaderState};
use splitforge_cli::{ScriptedReader, Speed};
use splitforge_domain::{ClockStep, DeviceClockState, RawReadJournal, SAMPLE_INTERVAL_MS};
use splitforge_reader::{Ingest, ReaderMessage, ReaderProvider};
use splitforge_storage::{ConfigStore, RaceSelection, SqliteJournal};
use splitforge_timesource::ClockReading;
use tokio::sync::mpsc::Receiver;

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

    /// Run a scripted scenario through the read path instead of waiting for a reader.
    ///
    /// **Its reads are synthetic and they enter the journal as evidence like any other.**
    /// Nothing marks a row as simulated, because nothing in the read path may treat one
    /// differently — that is what makes this worth running. What does say so is `/health`,
    /// which reports `"kind": "simulated"` for as long as the service is composed this way,
    /// and the log line printed at startup.
    ///
    /// Omitted, the service composes no reader at all and says so, which is what a device
    /// waiting for hardware should report.
    #[arg(long, value_name = "SLUG")]
    simulate: Option<String>,

    /// Seed for the simulated reader. The same seed replays the same race.
    #[arg(long, value_name = "SEED", default_value_t = 0x5F17_F03E)]
    simulate_seed: u64,

    /// `immediate`, or a speed multiplier such as `1` for wall-clock time.
    ///
    /// `immediate` writes as fast as the journal accepts, which is the setting that puts
    /// real back-pressure on the write path. A multiplier is for watching it happen.
    #[arg(long, value_name = "SPEED", default_value = "immediate")]
    simulate_speed: Speed,
}

/// Everything the health endpoint reads, behind one lock.
///
/// `rusqlite::Connection` is `Send` but not `Sync`, so a shared handle needs a mutex.
/// Everything behind it is a counting query, a `statvfs`, or a single-row insert, all
/// bounded in time regardless of how long the event has run.
///
/// **Three things contend for it now**, and the third is why the two fields below sit
/// outside it. The health handler reads; the clock monitor holds it for one `INSERT` at most
/// once per [`SAMPLE_INTERVAL_MS`]; and the read path holds it for an append that fsyncs the
/// sidecar before it commits, which on an SD card is the longest hold in the process. The
/// note this comment used to carry — *"if either of those stops being true this needs
/// `spawn_blocking`"* — is what the read path's own thread answers: it does not run on the
/// reactor, so a health request waits on a mutex rather than on a starved executor.
struct Device {
    database: PathBuf,
    started: Instant,
    stores: Mutex<Stores>,
    /// What the time daemon last said, or `None` before the first sample.
    ///
    /// Deliberately **not** behind [`Self::stores`]. The health handler must be able to
    /// report the clock source while a read is being written, and folding it in behind the
    /// journal's lock would make the cheapest field on the report wait for the most
    /// expensive one. An `RwLock` because every reader here only reads: the sampler swaps a
    /// small value once a minute, and the blocking subprocess that produced it ran outside
    /// the lock entirely.
    clock: RwLock<Option<ClockReading>>,
    /// What the reader is doing, which lives here and in no file.
    ///
    /// Also not behind [`Self::stores`], and for a sharper reason than the clock is: the
    /// thread that updates these counters is the thread that holds the journal lock while it
    /// writes. Sharing one lock would mean `/health` could only report the read path's
    /// progress at the moments the read path was not making any.
    reader: Mutex<ReaderStatus>,
}

/// The reader half of health, as the service tracks it.
struct ReaderStatus {
    kind: ReaderKind,
    state: Option<ReaderState>,
    /// Messages taken off the reader's channel.
    received: u64,
    /// Reads the journal has accepted and made durable.
    persisted: u64,
    /// Why the read path stopped, when it stopped for a reason worth reporting.
    fault: Option<String>,
}

impl ReaderStatus {
    /// A service composed with no reader.
    const fn none() -> Self {
        Self {
            kind: ReaderKind::None,
            state: None,
            received: 0,
            persisted: 0,
            fault: None,
        }
    }
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

        // Read before the journal lock, so the answer survives a poisoned `stores` — the
        // clock source is exactly the sort of thing somebody wants to know about a device
        // that has already had one failure.
        match self.clock.read() {
            Ok(sampled) => {
                // `device_clock_state` rather than `state_for_evidence`: the endpoint keeps
                // the distinction a read cannot. `None` here means nobody has asked yet, and
                // it must not arrive as a measurement saying the clock is bad.
                health.clock_source = sampled.as_ref().map(|reading| ClockSource {
                    measurement: reading.measurement().to_owned(),
                    state: reading.device_clock_state(),
                });
            }
            Err(_) => {
                health.degrade("the device's time source is unreadable after an earlier failure")
            }
        }

        // Also before the journal lock, and for a stronger reason than the clock: these
        // counters are updated by the thread that holds that lock while it writes. Reading
        // them through it would mean reporting the read path's progress only while the read
        // path was making none.
        match self.reader.lock() {
            Ok(reader) => {
                health.reader = ReaderHealth {
                    kind: reader.kind,
                    state: reader.state,
                    reads_received: reader.received,
                    reads_persisted: reader.persisted,
                };
                if let Some(fault) = reader.fault.as_ref() {
                    // A reader that stopped because a write failed is a different fact from
                    // one that reached the end of its script, and only the first degrades.
                    health.degrade(format!("the read path stopped: {fault}"));
                }
            }
            Err(_) => health.degrade("the reader's state is unreadable after an earlier failure"),
        }

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

impl Device {
    /// The clock state to stamp on a read.
    ///
    /// `state_for_evidence` rather than `device_clock_state`, and the difference matters
    /// here rather than in the report: a `raw_reads` row has nowhere to put *"nobody asked
    /// yet"*, so an unmeasured clock is recorded as `Unsynced`. That is the safe direction —
    /// `is_trustworthy` is false for it, so the error is toward warning about a clock that
    /// was fine rather than staying quiet about one that was not.
    fn clock_state_for_evidence(&self) -> DeviceClockState {
        self.clock
            .read()
            .map_or(DeviceClockState::Unsynced, |slot| {
                slot.as_ref()
                    .map_or(DeviceClockState::Unsynced, ClockReading::state_for_evidence)
            })
    }

    /// Applies one change to the reader's reported state.
    ///
    /// A poisoned lock is logged and dropped rather than propagated. Losing a counter update
    /// makes `/health` understate progress; panicking in the read path would stop the timer,
    /// and of the two only one of them loses a race.
    fn update_reader(&self, change: impl FnOnce(&mut ReaderStatus)) {
        match self.reader.lock() {
            Ok(mut status) => change(&mut status),
            Err(_) => eprintln!(
                "splitforge-edge: the reader's state could not be updated; it is unreadable \
                 after an earlier failure"
            ),
        }
    }
}

/// Drains a reader into the journal until the reader stops or a write fails.
///
/// **The ordering is the whole of this function**, and it is the one
/// [architecture § 3](../../../docs/architecture.md) fixes: the durable write completes
/// before anything is told the read exists. `SqliteJournal::append` appends to the
/// write-ahead sidecar and fsyncs it *before* the database commit
/// ([ADR-0018](../../../docs/adr/0018-write-ahead-sidecar-journal.md)), and only when it has
/// returned `Ok` does `persisted` move. A counter incremented before the write would report
/// reads that a power cut took, which is why `received` and `persisted` are two numbers and
/// not one — `docs/architecture.md` § 4 requires exactly that gap to be observable.
///
/// **Runs on its own thread**, not on the reactor and not in `spawn_blocking`. Every append
/// fsyncs, and on an SD card that is milliseconds during which nothing else on that thread
/// runs; the health endpoint exists to report trouble and must not be queued behind the
/// trouble. A detached `std` thread rather than a blocking task because the process must be
/// able to exit on SIGTERM without waiting for a reader that may never send again — a read
/// caught in flight by that exit is covered by the sidecar, which is what the sidecar is for.
///
/// A failed write stops the loop rather than skipping the read. A timer that cannot persist
/// evidence must say so and stop, not keep counting into a journal that is missing rows —
/// and stopping is visible, both as `ReaderState::Stopped` and as a degraded `/health`.
fn read_into_journal(device: &Device, mut receiver: Receiver<ReaderMessage>, ingest: Ingest) {
    while let Some(message) = receiver.blocking_recv() {
        device.update_reader(|status| status.received += 1);

        // The device's own clocks, read as close to arrival as this thread can manage. The
        // monotonic value is nanoseconds since this process started: an origin that is the
        // session, which is the only span the value is comparable within anyway.
        let received_at = time::OffsetDateTime::now_utc();
        let monotonic_ns = u64::try_from(device.started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let read = ingest.normalize(
            message,
            received_at,
            Some(monotonic_ns),
            device.clock_state_for_evidence(),
        );

        let outcome =
            match device.stores.lock() {
                Ok(mut stores) => stores.journal.append(&read).map(|_| ()).map_err(|error| {
                    format!("read {} could not be made durable: {error}", read.id)
                }),
                Err(_) => Err("the device state is unreadable after an earlier failure".to_owned()),
            };

        match outcome {
            // Only now. Everything above this line can still fail; nothing below it can
            // un-write the read.
            Ok(()) => device.update_reader(|status| status.persisted += 1),
            Err(fault) => {
                eprintln!("splitforge-edge: the read path stopped — {fault}");
                device.update_reader(|status| {
                    status.state = Some(ReaderState::Stopped);
                    status.fault = Some(fault);
                });
                return;
            }
        }
    }

    // The channel closed: the reader has nothing more to send. For a scripted scenario that
    // is the ordinary end of a run, which is why it carries no fault beside it.
    device.update_reader(|status| status.state = Some(ReaderState::Stopped));
}

/// How often the service asks the time daemon what it believes.
///
/// A minute, because what it measures moves on the order of minutes: chrony reaching a
/// source, losing one, or a PPS refclock coming up. Sampling faster would spend a subprocess
/// to re-learn the same answer, and sampling per request would put `chronyc`'s retry timeout
/// in the path of a watchdog whose whole job is to notice quickly.
const CLOCK_SOURCE_INTERVAL_SECS: u64 = 60;

/// Asks the time daemon what it believes, forever, and caches the answer.
///
/// Every read this service writes carries a `DeviceClockState`, and that is permanent
/// evidence — so it has to come from a measurement rather than an assumption. The same
/// sample answers `/health`, which is why it is taken here once rather than by each caller.
///
/// **The subprocess runs on a blocking thread.** `read_tracking` shells out to `chronyc`,
/// which blocks for as long as it takes to give up on a daemon that is not answering; run on
/// the reactor it would stall every task in the process, including the health endpoint that
/// exists to report trouble.
///
/// A failed sample leaves the previous answer in place rather than clearing it. Losing one
/// reading is a gap in freshness; replacing a good reading with nothing would tell the
/// operator their clock is unmeasured when it was measured a minute ago.
async fn watch_the_time_source(device: Arc<Device>) {
    let mut interval =
        tokio::time::interval(std::time::Duration::from_secs(CLOCK_SOURCE_INTERVAL_SECS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // The first tick completes immediately, and `main` has already taken that sample before
    // starting the read path. Consuming it here keeps startup from asking twice.
    interval.tick().await;

    loop {
        interval.tick().await;
        sample_the_time_source(&device).await;
    }
}

/// Asks the daemon once and stores the answer.
///
/// **Called once before the read path starts, and on an interval after that.** The ordering
/// is not incidental: `state_for_evidence` answers `Unsynced` while nothing has been
/// sampled, so a service that started reading before its first sample would stamp `Unsynced`
/// on the opening reads of an event — on a device whose clock was fine, for no reason but
/// the order two startup tasks happened to run in. `is_trustworthy` is false for `Unsynced`
/// and gates publication, and the reads this would mislabel are the ones around the gun.
///
/// The subprocess runs on a blocking thread: `read_tracking` shells out to `chronyc`, which
/// blocks for as long as it takes to give up on a daemon that is not answering.
async fn sample_the_time_source(device: &Device) {
    match tokio::task::spawn_blocking(splitforge_timesource::read_tracking).await {
        Ok(reading) => match device.clock.write() {
            // A failed sample leaves the previous answer in place rather than clearing it.
            Ok(mut slot) => *slot = Some(reading),
            Err(_) => eprintln!(
                "splitforge-edge: the time source could not be recorded; it is unreadable \
                 after an earlier failure"
            ),
        },
        Err(error) => {
            eprintln!("splitforge-edge: the time source could not be sampled: {error}");
        }
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
        clock: RwLock::new(None),
        reader: Mutex::new(ReaderStatus::none()),
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

    // Before the read path, deliberately. Every read carries a `DeviceClockState`, and one
    // stamped `Unsynced` because nothing had asked yet is a worse answer than waiting the
    // one subprocess call it takes to have a real one.
    sample_the_time_source(&device).await;
    tokio::spawn(watch_the_time_source(Arc::clone(&device)));

    if let Some(scenario) = args.simulate.as_deref() {
        compose_simulated_reader(
            &device,
            scenario,
            args.simulate_seed,
            args.simulate_speed,
            &args.database,
        )?;
    }

    serve(&args.socket, device).await
}

/// Puts a scripted reader on the front of the read path.
///
/// The composition root's actual job, for the first time: the loop it hands the reader to is
/// written against [`ReaderProvider`] and cannot tell what is behind it, so the ThingMagic
/// adapter arrives here as a different value passed to the same function rather than as a
/// second read path.
///
/// Fails the service rather than starting without a reader. A device told to run a scenario
/// and silently running none is a device that reports a healthy, idle read path all day.
fn compose_simulated_reader(
    device: &Arc<Device>,
    scenario: &str,
    seed: u64,
    speed: Speed,
    database: &std::path::Path,
) -> Result<()> {
    // The race's own configuration, so the reader and its trust setting are the operator's
    // rather than the binary's — the same values `splitforge simulate` runs under.
    let store = ConfigStore::open(database)
        .with_context(|| format!("opening the configuration at {}", database.display()))?;
    let race = match store.resolve_race(None).context("resolving the race")? {
        RaceSelection::One(race) => race,
        RaceSelection::None => anyhow::bail!(
            "--simulate {scenario} needs a configured race, and this database has none. \
             Run `splitforge fixture load --fixture {scenario}` against it first."
        ),
        RaceSelection::Ambiguous(races) => anyhow::bail!(
            "--simulate {scenario} needs exactly one race and this database has {}. \
             The service takes no race selector; configure one race, or run the scenario \
             with `splitforge simulate` instead.",
            races.len()
        ),
    };
    let config = store
        .load(race.id)
        .context("loading the race configuration")?;

    let ScriptedReader {
        provider,
        ingest,
        reader,
        planned_crossings,
    } = splitforge_cli::scripted_reader(&config, scenario, seed, speed)?;

    eprintln!(
        "splitforge-edge: SIMULATED reader {:?} running scenario {scenario:?} \
         ({planned_crossings} planned crossing(s), seed {seed:#x}) — its reads enter the \
         journal as evidence",
        reader.as_str()
    );

    // Boxed as the port rather than as the concrete type, which is the line that keeps the
    // read path honest: nothing below here can branch on the reader being simulated.
    let provider: Box<dyn ReaderProvider> = Box::new(provider);
    let receiver = provider.start();

    device.update_reader(|status| {
        status.kind = ReaderKind::Simulated;
        status.state = Some(ReaderState::Connected);
    });

    let device = Arc::clone(device);
    std::thread::spawn(move || read_into_journal(&device, receiver, ingest));

    Ok(())
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
