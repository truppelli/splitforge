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
//! **It composes one of two providers, or none.** `--simulate` runs a scenario through the
//! read path; `--serial` opens a ThingMagic module on a device path; omitting both serves
//! health and reads nothing, which is what a device waiting for hardware should do. The two
//! flags conflict at the argument parser, so the service never arbitrates between a real
//! module and a scripted one.
//!
//! Neither is a second code path. Both arrive as a `Box<dyn ReaderProvider>` handed to the
//! same loop, which is the claim the port exists to make — composing a module changes which
//! value is boxed and nothing else.
//!
//! **`--serial` decodes reads with a parser anchored on one captured frame** and refuses to
//! guess at layouts it has not seen, so a wrong assumption shows up as no reads and a climbing
//! fault count rather than as evidence about a chip that was never there. Connection edges
//! become confirmed gaps whatever the reports look like — that half needs no decoder.
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
use splitforge_api::{
    ClockSource, Health, HealthSource, OpenGap, ReaderHealth, ReaderKind, ReaderState,
};
use splitforge_cli::{ScriptedReader, Speed};
use splitforge_domain::{
    ClockStep, DeviceClockState, GapDetection, JournalError, RaceId, RawRead, RawReadJournal,
    ReaderId, SAMPLE_INTERVAL_MS, SilenceVerdict, assess_silence,
};
use splitforge_reader::{Disconnection, Ingest, ReaderEvent, ReaderFaults, ReaderProvider};
use splitforge_storage::{ConfigStore, RaceSelection, SqliteJournal};
use splitforge_thingmagic::{
    Region, SerialSettings, StartSequence, StreamDecoder, ThingMagicReader,
};
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

    /// Read from a ThingMagic serial module on this device path. Requires `--region`.
    ///
    /// Every connection tells the module to read: it stops any stream the last connection left
    /// running, selects Gen2 and the region, turns the module's read filter off, and starts a
    /// stream. A module that refuses a step is recorded as a gap that did not start.
    ///
    /// Reads are decoded by a parser anchored on one captured frame from real hardware, and
    /// **it refuses to guess**: a report whose layout it has not seen becomes a counted
    /// decode fault rather than a wrong chip identifier. Connection edges are recorded as
    /// bounded gaps whatever the reports look like — that half needs no decoder.
    ///
    /// **The installed unit cannot use this.** `deploy/splitforge-edge.service` passes no
    /// arguments and sets `PrivateDevices=yes`, which gives the service a private `/dev`
    /// with no serial node in it. This is a bench flag until both change deliberately.
    #[arg(
        long,
        value_name = "PATH",
        conflicts_with = "simulate",
        requires = "region"
    )]
    serial: Option<String>,

    /// The regulatory region the module transmits in, for `--serial`: `na`, `eu`, `eu3`, and
    /// the rest of the names in `splitforge_thingmagic::Region`.
    ///
    /// **There is no default.** The module is one SKU pre-configured for many regions, and
    /// which one is legal depends on where the device is. The service sets it on every
    /// connection rather than trusting whatever the module last held.
    #[arg(long, value_name = "REGION", requires = "serial")]
    region: Option<Region>,

    /// Bits per second for `--serial`. The module's own default is 115200.
    #[arg(long, value_name = "BAUD", default_value_t = 115_200)]
    serial_baud: u32,

    /// Which configured reader `--serial` is reading for.
    ///
    /// Optional when the race configures exactly one reader, which is the only topology this
    /// milestone supports. It names the reader a gap is recorded against, so it has to match
    /// a reader the database knows.
    #[arg(long, value_name = "ID")]
    reader: Option<String>,
}

/// The service's state, and the handles it reads and writes the database through.
///
/// `rusqlite::Connection` is `Send` but not `Sync`, so a shared handle needs a mutex.
///
/// **Two sets of handles on one database**, because of what contends for them. The read path
/// holds [`Self::stores`] for an append that fsyncs the sidecar before it commits, which on an
/// SD card is the longest hold in the process; the clock monitor and the silence watchdog hold
/// it briefly to write their rows. Health only reads, and reads it on
/// [`Self::observed`], a connection of its own. In WAL mode a reader neither waits for a
/// writer nor holds one up, so a monitor polling health adds nothing to the time a read takes
/// to reach the journal, and never waits out an append.
struct Device {
    database: PathBuf,
    started: Instant,
    /// The handles the service writes through: the read path, the clock monitor, and the
    /// silence watchdog.
    stores: Mutex<Stores>,
    /// A second journal and configuration handle on the same database, for health alone.
    ///
    /// Never appended to. It sees what the writers have committed, which is what health
    /// reports, and its lock is contended only by other health requests.
    observed: Mutex<Stores>,
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
    /// Reads the journal refused for good and the audit trail holds instead.
    set_aside: u64,
    /// What the provider could not read, as it last reported.
    faults: ReaderFaults,
    /// Why the read in hand is not stored yet, while the read path is retrying it.
    fault: Option<String>,
    /// Which reader is composed, and which race it is reading for.
    ///
    /// `None` until one is composed, which is what stops the silence watchdog from opening a
    /// gap against a device that simply has no reader yet.
    watching: Option<Watching>,
    /// When this service last saw a message from the reader, on the monotonic clock.
    ///
    /// Set when the reader is composed rather than left empty, so silence is measured from
    /// the moment the service started listening. A `None` here would have to mean either
    /// "no reader" or "a reader that has said nothing since boot", and those call for
    /// opposite reactions.
    last_message: Option<Instant>,
    /// Whether the provider has said it lost the port and has not yet said it is back.
    ///
    /// **This is what stops the two detectors from arguing.** They answer the same question
    /// from different evidence and only one of them is guessing: while the transport has
    /// reported its own failure there is nothing an inference from quiet can add.
    ///
    /// It is a correctness guard rather than an optimization, because
    /// [`assess_silence`] reads *not silent for long enough yet* as
    /// [`SilenceVerdict::Close`] — a verdict that means "reads are arriving". A tick landing
    /// inside the first threshold's worth of a real outage reaches it without any read having
    /// arrived, and would close the confirmed gap the disconnection had just opened, writing
    /// a *"reads resumed"* edge at a moment when nothing resumed.
    ///
    /// Deliberately **not** derived from whether a gap is open in the journal: a gap can be
    /// open because a previous run of this service left one there, which says nothing about
    /// whether *this* process's provider currently holds a port.
    transport_down: bool,
}

/// What the silence watchdog needs to know to do its job.
#[derive(Clone)]
struct Watching {
    reader: ReaderId,
    race: RaceId,
}

impl ReaderStatus {
    /// A service composed with no reader.
    const fn none() -> Self {
        Self {
            kind: ReaderKind::None,
            state: None,
            received: 0,
            persisted: 0,
            set_aside: 0,
            faults: ReaderFaults {
                framing: 0,
                decoding: 0,
            },
            fault: None,
            watching: None,
            last_message: None,
            transport_down: false,
        }
    }

    /// Why this reader is talking and recording nothing, if it is.
    ///
    /// Frames arrived intact and none decoded, ever. That is a layout this service does not
    /// understand, and it looks exactly like a quiet field unless something says so. Once one
    /// read has decoded, later refusals are counted and do not degrade: a frame type nobody has
    /// captured yet, arriving between runners, would otherwise flip health on and off all day
    /// and teach an operator to ignore it.
    fn undecodable(&self) -> Option<String> {
        if self.faults.decoding == 0 || self.received > 0 {
            return None;
        }
        let reader = self
            .watching
            .as_ref()
            .map_or("the reader", |watching| watching.reader.as_str());
        Some(format!(
            "reader {reader} has sent {} frame(s) that arrived intact and could not be \
             decoded, and none that could; nothing it reports is being recorded",
            self.faults.decoding
        ))
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
                    reads_set_aside: reader.set_aside,
                    framing_faults: reader.faults.framing,
                    decode_faults: reader.faults.decoding,
                    // Filled in below, from the journal rather than from this struct. An
                    // open gap has to survive the restart that a power cut causes, so the
                    // rows are the authority and this process's memory is not.
                    open_gap: None,
                };
                if let Some(fault) = reader.fault.as_ref() {
                    // Present only while a write is failing, and cleared by the one that
                    // lands. Reads are queuing behind it and the reader will soon stop being
                    // read, so this is the most urgent line the endpoint can carry.
                    health.degrade(format!("reads are not being stored: {fault}; retrying"));
                }
                if reader.set_aside > 0 {
                    // Not cleared, because nothing undoes it: each is a read that reached
                    // the device and is not in the journal. The audit trail has them.
                    health.degrade(format!(
                        "{} read(s) held a value the journal cannot store and are not in it; \
                         each is on the audit trail as journal.unstorable",
                        reader.set_aside
                    ));
                }
                if let Some(reason) = reader.undecodable() {
                    health.degrade(reason);
                }
            }
            Err(_) => health.degrade("the reader's state is unreadable after an earlier failure"),
        }

        // A poisoned mutex means a previous call panicked while holding it. Reporting that
        // as a degradation is strictly better than panicking again: the endpoint keeps
        // answering, and what it answers is the truth.
        //
        // `observed`, not `stores`: this is the only thing that reads through it, so the
        // read path never waits on these queries and they never wait on its appends.
        let Ok(stores) = self.observed.lock() else {
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

        // Read from the rows, not from this process's memory. A gap that opened before the
        // last restart is exactly the one worth reporting, and it is the one an in-memory
        // flag would have forgotten (ADR-0026).
        match stores.journal.open_reader_gaps() {
            Ok(gaps) => {
                // The reader field describes *this* device's reader, so the gap reported
                // beside it is the longest-standing one; the rest are in the journal, which
                // is where a device with several readers goes to see them all.
                if let Some(gap) = gaps.first() {
                    // Wall clock, because the gap may have been opened by a previous run of
                    // this service and monotonic readings do not survive that.
                    let elapsed =
                        (time::OffsetDateTime::now_utc() - gap.started_at).whole_milliseconds();
                    health.reader.open_gap = Some(OpenGap {
                        detection: gap.detection,
                        open_for_ms: u64::try_from(elapsed).unwrap_or(0),
                    });
                }
                for gap in &gaps {
                    // Named separately per reader, because "which one" is the first thing an
                    // operator needs and the status line cannot carry it.
                    health.degrade(format!(
                        "reader {} has been {} gone since {}; \
                         reads from it are not being recorded",
                        gap.reader_id.as_str(),
                        match gap.detection {
                            GapDetection::Confirmed => "confirmed",
                            GapDetection::Suspected => "possibly",
                        },
                        gap.started_at,
                    ));
                }
            }
            Err(error) => health.degrade(format!("reader gaps could not be read: {error}")),
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
/// operator with `date`. The Pi 4 has no battery-backed clock ([ADR-0036]), so it boots
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
/// [ADR-0036]: ../../../docs/adr/0036-raspberry-pi-4-is-the-edge-target.md
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
/// **A failed write is retried, and the loop never moves past a read it could have stored**
/// ([ADR-0031](../../../docs/adr/0031-a-failed-write-is-retried-and-an-unstorable-read-is-set-aside.md)).
/// It used to stop instead, so the process stayed up while every later read was dropped, and
/// `Restart=always` never fired. See [`store`] for the one kind of read it does move past.
fn read_into_journal(device: &Device, mut receiver: Receiver<ReaderEvent>, ingest: Ingest) {
    while let Some(event) = receiver.blocking_recv() {
        let message = match event {
            ReaderEvent::Read(message) => message,
            // The lifecycle half, which is why this loop takes events rather than reads.
            // Neither of these is a read, so neither touches a counter that claims to
            // count reads.
            ReaderEvent::Connected => {
                record_connection(device, None);
                continue;
            }
            ReaderEvent::Disconnected { cause } => {
                record_connection(device, Some(cause));
                continue;
            }
            // Not a read and not proof of life: `last_message` stays where it is, so a reader
            // that sends only what cannot be decoded is still silent to the watchdog.
            ReaderEvent::Faults(faults) => {
                device.update_reader(|status| status.faults = faults);
                continue;
            }
        };

        device.update_reader(|status| {
            status.received += 1;
            // Before the write, deliberately. This says the reader is alive, which it has
            // just proved by sending; whether the journal can keep up is a different fault,
            // reported by `persisted` falling behind rather than by a gap in the evidence.
            status.last_message = Some(Instant::now());
        });

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

        match store(device, &read) {
            // Only now. Everything above this line can still fail; nothing below it can
            // un-write the read.
            Stored::Journaled => device.update_reader(|status| status.persisted += 1),
            Stored::SetAside => device.update_reader(|status| status.set_aside += 1),
        }
    }

    // The channel closed: the reader has nothing more to send. For a scripted scenario that
    // is the ordinary end of a run, which is why it carries no fault beside it.
    device.update_reader(|status| status.state = Some(ReaderState::Stopped));
}

/// The delay before the first retry of a failed append.
///
/// Short, because the commonest cause worth retrying, a database lock held by another
/// connection, has usually cleared by then. A `SQLITE_BUSY` has already waited out the
/// five-second busy timeout before it reaches here, so this adds little to that case.
const RETRY_FIRST: std::time::Duration = std::time::Duration::from_millis(100);

/// The ceiling the retry delay doubles to.
///
/// A disk that has been freed, or a lock that has been released, is written to within this
/// long. Nothing is lost by waiting less: while the read path retries, the channel fills and
/// the provider stops taking bytes off the port, which is the back-pressure architecture § 4
/// describes whatever the delay is.
const RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(5);

/// Who the service is, on the audit trail.
const SERVICE_ACTOR: &str = "splitforge-edge";

/// How a read left the read path.
enum Stored {
    /// In the journal and the sidecar.
    Journaled,
    /// Refused for good, and recorded on the audit trail instead.
    SetAside,
}

/// Appends `read`, retrying until it is stored or refused for good.
///
/// **A failure is retried with the same read**, from `RETRY_FIRST` doubling to `RETRY_MAX`,
/// and for as long as it takes. A full disk, a failing card, and a lock held past the busy
/// timeout are all conditions that can end, and the journal is never the thing that gives up
/// on a read (architecture § 4). Health is degraded while it retries and recovers when a write
/// lands. If only the database half of an append failed, the retry writes the sidecar line a
/// second time, and replay takes each id once.
///
/// **A read the journal cannot represent is set aside**, because retrying it would stop the
/// read path exactly as giving up did. It is recorded on the audit trail with every field it
/// carried, counted, and the loop moves on.
///
/// **A poisoned lock ends the process.** Something panicked while holding it, nothing in this
/// process can make it usable again, and exiting is what lets `Restart=always` start one whose
/// recovery replays the sidecar. Retrying it would be the silent stop this replaced.
fn store(device: &Device, read: &RawRead) -> Stored {
    let mut delay = RETRY_FIRST;
    let mut failures = 0_u32;

    loop {
        let Ok(mut stores) = device.stores.lock() else {
            eprintln!(
                "splitforge-edge: the journal is unreadable after an earlier failure; exiting \
                 so the service restarts and recovers from the sidecar"
            );
            std::process::exit(1);
        };

        let error = match stores.journal.append(read) {
            Ok(_) => {
                drop(stores);
                if failures > 0 {
                    eprintln!(
                        "splitforge-edge: read {} was stored after {failures} failed attempt(s)",
                        read.id
                    );
                    device.update_reader(|status| status.fault = None);
                }
                return Stored::Journaled;
            }
            Err(JournalError::Unstorable(reason)) => {
                let recorded = stores
                    .journal
                    .record_unstorable(read, &reason, SERVICE_ACTOR);
                drop(stores);
                // Every one is logged. They cannot arrive often, and each is a read that is
                // not in the journal.
                match recorded {
                    Ok(()) => eprintln!(
                        "splitforge-edge: {reason}; it was set aside on the audit trail as \
                         journal.unstorable and recording continues"
                    ),
                    Err(error) => eprintln!(
                        "splitforge-edge: {reason}; it could not be set aside on the audit \
                         trail either ({error}), and recording continues"
                    ),
                }
                return Stored::SetAside;
            }
            Err(error) => error,
        };
        drop(stores);

        failures = failures.saturating_add(1);
        let fault = format!("read {} could not be made durable: {error}", read.id);
        // Said once per outage rather than per attempt. At the ceiling that is one line every
        // five seconds for as long as the disk stays full, and the health endpoint already
        // carries the current reason.
        if failures == 1 {
            eprintln!("splitforge-edge: {fault}; retrying until it is stored");
        }
        device.update_reader(|status| status.fault = Some(fault));

        std::thread::sleep(delay);
        delay = delay.saturating_mul(2).min(RETRY_MAX);
    }
}

/// Records a change in whether the reader is connected, as evidence rather than as a flag.
///
/// `cause` is `None` for a connection and `Some` for the loss of one. This is the *confirmed*
/// half of [ADR-0025](../../../docs/adr/0025-m3a-proves-durability-above-the-transport.md)'s
/// third clause — the half `check_for_silence` below deliberately cannot supply, because a
/// stream that has gone quiet is indistinguishable from a checkpoint nobody is crossing. A
/// transport that says the port died is not ambiguous, so what it opens is
/// [`GapDetection::Confirmed`].
///
/// **This one is not gated on a race running, and the silence watchdog is.** The asymmetry is
/// the ambiguity, not an oversight: silence during a race means something and silence on a
/// bench overnight does not, whereas a reader that is *not there* is a true statement at any
/// hour. It is also the statement an operator most wants before the gun, when the reason the
/// port will not open is usually that nothing has been plugged into it yet.
///
/// Writing the same gap twice costs nothing — `open_reader_gap` returns the gap already open
/// rather than opening a second ([ADR-0026](../../../docs/adr/0026-a-reader-gap-is-two-rows.md))
/// — which is what lets the provider report every failed reconnection without this function
/// having to remember what it was last told.
fn record_connection(device: &Device, cause: Option<Disconnection>) {
    // The reader's lock first, and released before the journal work below, which can block on
    // an fsync the read path is in the middle of. Same ordering as `check_for_silence`, for
    // the same reason.
    let watching = {
        let Ok(mut status) = device.reader.lock() else {
            eprintln!(
                "splitforge-edge: the reader's state could not be updated; it is \
                 unreadable after an earlier failure"
            );
            return;
        };
        status.state = Some(if cause.is_some() {
            ReaderState::Disconnected
        } else {
            ReaderState::Connected
        });
        // Set even when the row below cannot be written. This is what this process believes
        // about its own provider, and the watchdog has to stand down either way: a failed
        // write leaves the evidence incomplete, and letting an inference from quiet file a
        // *suspected* gap over the top would make it wrong as well.
        status.transport_down = cause.is_some();
        if cause.is_none() {
            // A connection is evidence the reader is alive, exactly as a read is — and it is
            // the only such evidence a reader in an empty field will produce. Without this,
            // silence would still be measured from before the outage, and the watchdog would
            // open a *suspected* gap seconds after a real reconnection closed a confirmed
            // one.
            status.last_message = Some(Instant::now());
        }
        status.watching.clone()
    };

    // No reader composed means there is nothing to record a gap against. The provider cannot
    // reach this state today; the guard is here because `watching` is what names the row.
    let Some(watching) = watching else {
        return;
    };

    let Ok(mut stores) = device.stores.lock() else {
        return;
    };

    let now = time::OffsetDateTime::now_utc();
    let monotonic_ms = u64::try_from(device.started.elapsed().as_millis()).unwrap_or(u64::MAX);

    let outcome = match cause {
        Some(cause) => stores
            .journal
            .open_reader_gap(
                &watching.reader,
                GapDetection::Confirmed,
                now,
                monotonic_ms,
                Some(cause.detail()),
            )
            .map(|_| ()),
        // Closes whatever is open, including a gap that was merely suspected: a reader that
        // is connected is a reader that came back, whichever way its absence was noticed.
        None => stores
            .journal
            .close_reader_gap(
                &watching.reader,
                now,
                monotonic_ms,
                Some("the reader connected"),
            )
            .map(|_| ()),
    };

    if let Err(error) = outcome {
        eprintln!("splitforge-edge: a reader connection change could not be recorded — {error}");
    }
}

/// How often the silence watchdog looks at the clock.
///
/// Ten seconds, and deliberately unrelated to the threshold it is comparing against. The
/// interval bounds how *late* a gap's recorded start can be, not how long silence must last —
/// so it stays small while the threshold stays a race-day policy that
/// [Q14](../../../docs/open-questions.md#q14-reader-silence-threshold) has not answered.
const SILENCE_TICK_SECS: u64 = 10;

/// Opens and closes *suspected* gaps, forever.
///
/// The module cannot announce its own failure — user guide § 8.8.2 says it cannot *"detect a
/// broken communications interface connection and stop streaming the tag results"* — so a
/// reader that has died looks exactly like a reader with nothing to report. This is the half
/// of [ADR-0025](../../../docs/adr/0025-m3a-proves-durability-above-the-transport.md)'s third
/// clause that does not need the transport to say anything.
///
/// **Everything it opens is `suspected`, never `confirmed`.** A quiet checkpoint and a dead
/// module are indistinguishable from here, and the evidence says so rather than resolving the
/// ambiguity with a guess. `confirmed` belongs to whatever actually held the port.
///
/// **It only runs while a race is running.** A device on a bench overnight is silent for
/// twelve hours and that is not a fault; recording it as one would fill the table with gaps
/// nobody can act on and teach an operator to ignore the whole signal.
async fn watch_for_silence(device: Arc<Device>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(SILENCE_TICK_SECS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        interval.tick().await;
        check_for_silence(&device);
    }
}

/// One pass of the watchdog.
///
/// Split out from the loop so a test can run exactly one tick rather than waiting on a timer.
fn check_for_silence(device: &Device) {
    // Read the reader's state first and let go of that lock, because the journal work below
    // can block on an fsync the read path is in the middle of.
    let (watching, last_message, transport_down) = match device.reader.lock() {
        Ok(reader) => (
            reader.watching.clone(),
            reader.last_message,
            reader.transport_down,
        ),
        Err(_) => return,
    };

    // **The transport has already spoken, so quiet adds nothing.** An inference is only worth
    // making where something is left to infer, and a provider that reported its own failure
    // has left nothing ambiguous — the gap is open, and it is already `confirmed`.
    //
    // Without this the next tick would *close* that gap: `assess_silence` reads "not silent
    // for long enough yet" as `Close`, which means "reads are arriving", and inside the first
    // threshold's worth of an outage it is reached without a single read having arrived.
    // `Connected` is what closes this one, and it comes from the transport.
    if transport_down {
        return;
    }

    // No reader composed is not silence. There is nothing whose quiet could mean anything.
    let (Some(watching), Some(last_message)) = (watching, last_message) else {
        return;
    };

    let Ok(mut stores) = device.stores.lock() else {
        return;
    };

    let threshold_ms = match stores.config.reader_silence_threshold_ms() {
        Ok(ms) => ms,
        Err(error) => {
            eprintln!("splitforge-edge: the silence threshold could not be read — {error}");
            return;
        }
    };

    let running = match stores.config.race_is_running(watching.race) {
        Ok(running) => running,
        Err(error) => {
            eprintln!("splitforge-edge: the race's state could not be read — {error}");
            return;
        }
    };

    let silent_for_ms = u64::try_from(last_message.elapsed().as_millis()).unwrap_or(u64::MAX);
    let now = time::OffsetDateTime::now_utc();
    let monotonic_ms = u64::try_from(device.started.elapsed().as_millis()).unwrap_or(u64::MAX);

    // The policy is a pure function in the domain, so the boundary cases are tested without
    // a database or a timer. This function is only the I/O that carries the verdict out.
    let verdict = assess_silence(threshold_ms, running, silent_for_ms);

    if verdict == SilenceVerdict::Open {
        // `open_reader_gap` returns the gap already open rather than opening a second, so a
        // watchdog ticking every ten seconds through a twenty-minute outage writes one row.
        // The recorded start is when the silence *began*, not when this tick noticed it.
        let began = now - time::Duration::milliseconds(i64::try_from(silent_for_ms).unwrap_or(0));
        if let Err(error) = stores.journal.open_reader_gap(
            &watching.reader,
            GapDetection::Suspected,
            began,
            monotonic_ms,
            Some("no reads for longer than the configured silence threshold"),
        ) {
            eprintln!("splitforge-edge: a suspected reader gap could not be recorded — {error}");
        }
    } else if verdict == SilenceVerdict::Close {
        // Reads are arriving again, so whatever was open has ended — including a confirmed
        // gap, because a reader that is delivering is a reader that came back.
        if let Err(error) = stores.journal.close_reader_gap(
            &watching.reader,
            now,
            monotonic_ms,
            Some("reads resumed"),
        ) {
            eprintln!("splitforge-edge: a reader gap could not be closed — {error}");
        }
    }
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
    let (journal, recovery) = SqliteJournal::open_recovering(&args.database, SERVICE_ACTOR)
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

    let observed = observer(&args.database).with_context(|| {
        format!(
            "opening a second handle on {} for health",
            args.database.display()
        )
    })?;

    let device = Arc::new(Device {
        database: args.database.clone(),
        started: Instant::now(),
        stores: Mutex::new(Stores { journal, config }),
        observed: Mutex::new(observed),
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
    tokio::spawn(watch_for_silence(Arc::clone(&device)));

    // At most one reader, and clap enforces the "at most" — `--serial` declares
    // `conflicts_with = "simulate"`, so the two arms below cannot both be taken and the
    // service never has to arbitrate between a real module and a scripted one.
    //
    // Composing none is the third case and it is not a fault: it is what a device waiting
    // for hardware should do, and it is what every deployment does today, because the unit
    // passes no arguments.
    match (args.serial.as_deref(), args.simulate.as_deref()) {
        (Some(path), _) => {
            // `requires` makes this unreachable from the command line; the check keeps it
            // unreachable without an `unwrap`.
            let Some(region) = args.region else {
                anyhow::bail!("--serial {path} needs --region");
            };
            compose_serial_reader(
                &device,
                path,
                args.serial_baud,
                region,
                args.reader.as_deref(),
                &args.database,
            )?;
        }
        (None, Some(scenario)) => {
            compose_simulated_reader(
                &device,
                scenario,
                args.simulate_seed,
                args.simulate_speed,
                &args.database,
            )?;
        }
        (None, None) => eprintln!(
            "splitforge-edge: no reader composed; serving health only. Pass --serial to \
             read a module, or --simulate to run a scenario through the read path."
        ),
    }

    let served = serve(&args.socket, Arc::clone(&device)).await;
    checkpoint_before_exit(&device);
    served
}

/// Records a recovery checkpoint as the service stops, so the next start reads nothing it has
/// already verified (ADR-0037).
///
/// Only on a stop the service is told about. A power cut is covered by the checkpoint an
/// append recorded within the last [`CHECKPOINT_INTERVAL`]. Nothing here can make stopping
/// fail: a checkpoint that cannot be written costs the next start a longer read, and is said.
///
/// [`CHECKPOINT_INTERVAL`]: splitforge_storage::CHECKPOINT_INTERVAL
fn checkpoint_before_exit(device: &Device) {
    let Ok(mut stores) = device.stores.lock() else {
        return;
    };
    if let Err(error) = stores.journal.checkpoint() {
        eprintln!(
            "splitforge-edge: no recovery checkpoint was recorded on the way out ({error}); \
             the next start reads more of the sidecar"
        );
    }
}

/// The handles health reads through, on the database the writers use.
///
/// Opened after the writer's journal, so any replay from the sidecar has already committed.
/// `SqliteJournal::open` does not reconcile, and this handle never appends: it is an observer,
/// in the way `splitforge reads --follow` is.
fn observer(database: &std::path::Path) -> Result<Stores, splitforge_storage::StorageError> {
    Ok(Stores {
        journal: SqliteJournal::open(database)?,
        config: ConfigStore::open(database)?,
    })
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
        // Not `Connected`. The composition root has started a provider, not established a
        // connection, and only the provider can say which it has — a distinction that costs
        // a simulated reader microseconds and stops a serial reader whose port never opened
        // from reporting a healthy read path all day.
        status.state = Some(ReaderState::Disconnected);
        status.watching = Some(Watching {
            reader: reader.clone(),
            race: race.id,
        });
        // Silence is measured from the moment the service started listening, not from the
        // first read — otherwise a reader that never says anything at all is the one case
        // the watchdog cannot see.
        status.last_message = Some(Instant::now());
    });

    let device = Arc::clone(device);
    std::thread::spawn(move || read_into_journal(&device, receiver, ingest));

    Ok(())
}

/// Puts a real serial module on the front of the read path.
///
/// The same function as [`compose_simulated_reader`] with a different provider in the middle,
/// which is the claim [ADR-0004](../../../docs/adr/0004-llrp-first-reader-adapter.md) makes
/// for the port: composing a module changes which value is boxed and nothing else. The loop it
/// hands the reader to is the one the simulator has been exercising since Milestone 1.
///
/// **It composes [`StreamDecoder`]**, which turns a `0x22` streaming response into a read and
/// refuses to guess at anything a captured frame does not anchor. A layout it cannot walk
/// becomes a counted decode fault rather than a plausible, wrong chip identifier — so the
/// failure mode of a wrong assumption here is no reads and a climbing error count, not
/// evidence about a chip that was never there.
///
/// Two halves of Milestone 3a run through this function and they fail independently. The
/// **connection** half — a port that opens, a cable pulled out, a reconnection, each recorded
/// as a bounded gap — needs no decoder at all and works whatever the reports turn out to look
/// like. The **read** half is the decoder's, and is the one a real module could still
/// disagree with.
fn compose_serial_reader(
    device: &Arc<Device>,
    path: &str,
    baud: u32,
    region: Region,
    reader: Option<&str>,
    database: &std::path::Path,
) -> Result<()> {
    let store = ConfigStore::open(database)
        .with_context(|| format!("opening the configuration at {}", database.display()))?;
    let race = match store.resolve_race(None).context("resolving the race")? {
        RaceSelection::One(race) => race,
        RaceSelection::None => anyhow::bail!(
            "--serial {path} needs a configured race, and this database has none. A gap is \
             recorded against a reader, and a reader belongs to a race."
        ),
        RaceSelection::Ambiguous(races) => anyhow::bail!(
            "--serial {path} needs exactly one race and this database has {}. The service \
             takes no race selector; configure one race.",
            races.len()
        ),
    };
    let config = store
        .load(race.id)
        .context("loading the race configuration")?;

    // The reader has to be one the database knows, because its id is what names the gap rows
    // this composition exists to produce. A gap against a reader nobody configured is a row
    // an operator cannot act on.
    let configured = match reader {
        Some(wanted) => config
            .readers
            .iter()
            .find(|candidate| candidate.id.as_str() == wanted)
            .with_context(|| {
                format!(
                    "race {:?} has no reader {wanted:?}. Configured readers: {}",
                    config.race.name,
                    reader_ids(&config)
                )
            })?,
        None => match config.readers.as_slice() {
            [only] => only,
            [] => anyhow::bail!(
                "race {:?} configures no reader. Run `splitforge reader add` before \
                 starting the service with --serial.",
                config.race.name
            ),
            many => anyhow::bail!(
                "race {:?} configures {} readers, so --serial cannot tell which one it is \
                 reading for. Name it with --reader. Configured readers: {}",
                config.race.name,
                many.len(),
                reader_ids(&config)
            ),
        },
    };

    let reader_id = configured.id.clone();
    let ingest = Ingest {
        trust: configured.timestamp_trust,
        ..Ingest::default()
    };

    let settings = SerialSettings {
        path: path.to_owned(),
        baud,
        ..SerialSettings::default()
    };

    eprintln!(
        "splitforge-edge: SERIAL reader {:?} on {path} at {baud} baud, region {}. Each \
         connection starts a Gen2 stream, and reports are decoded by a parser that counts what \
         it cannot read instead of guessing",
        reader_id.as_str(),
        region.name()
    );

    let provider: Box<dyn ReaderProvider> = Box::new(
        ThingMagicReader::new(
            reader_id.clone(),
            splitforge_thingmagic::serial(settings),
            StreamDecoder::new(reader_id.clone()),
        )
        .with_start(StartSequence::gen2(region)),
    );
    let receiver = provider.start();

    device.update_reader(|status| {
        status.kind = ReaderKind::Serial;
        // Disconnected until the provider says otherwise, which for a serial port is a
        // materially different claim than it is for a simulator: this one may never open.
        status.state = Some(ReaderState::Disconnected);
        status.watching = Some(Watching {
            reader: reader_id,
            race: race.id,
        });
        status.last_message = Some(Instant::now());
    });

    let device = Arc::clone(device);
    std::thread::spawn(move || read_into_journal(&device, receiver, ingest));

    Ok(())
}

/// The configured readers, for an error message that tells the operator what to type.
fn reader_ids(config: &splitforge_domain::RaceConfig) -> String {
    config
        .readers
        .iter()
        .map(|reader| reader.id.as_str())
        .collect::<Vec<_>>()
        .join(", ")
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

#[cfg(test)]
mod tests {
    use super::*;
    use splitforge_reader::ReaderTimestamp;

    /// A device on a temporary 5K, with a reader composed and nothing behind it.
    ///
    /// The directory comes back with it: dropping it deletes the database out from under an
    /// open journal, which on Windows fails the next write rather than the assertion that
    /// wanted it.
    fn a_device_watching_a_reader() -> (Device, tempfile::TempDir) {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let database = directory.path().join("event.db");

        let mut config = ConfigStore::open(&database).expect("open the configuration");
        splitforge_cli::load_fixture(&mut config, "test", "five-k").expect("load the fixture");
        let race = match config.resolve_race(None).expect("resolve the race") {
            RaceSelection::One(race) => race,
            other => panic!("expected exactly one race, got {other:?}"),
        };
        let loaded = config.load(race.id).expect("load the race configuration");
        let reader = loaded
            .readers
            .first()
            .expect("the fixture configures a reader")
            .id
            .clone();

        let (journal, _recovery) =
            SqliteJournal::open_recovering(&database, "test").expect("open the journal");

        let observed = observer(&database).expect("open the observer");
        let device = Device {
            database,
            started: Instant::now(),
            stores: Mutex::new(Stores { journal, config }),
            observed: Mutex::new(observed),
            clock: RwLock::new(None),
            reader: Mutex::new(ReaderStatus {
                // The kind is irrelevant here and there is no `Serial` yet: composing the
                // adapter is the next bullet, and a variant nothing produces would be a
                // claim about hardware this milestone has not earned.
                kind: ReaderKind::Simulated,
                state: Some(ReaderState::Disconnected),
                received: 0,
                persisted: 0,
                set_aside: 0,
                faults: ReaderFaults::default(),
                fault: None,
                watching: Some(Watching {
                    reader,
                    race: race.id,
                }),
                last_message: Some(Instant::now()),
                // Matches `state` above: nothing has reported a connection yet. The guard
                // this feeds is lifted by the first `Connected`, so a test that never
                // records one is a test of a device whose provider has said nothing.
                transport_down: false,
            }),
        };

        (device, directory)
    }

    fn open_gap(device: &Device) -> Option<splitforge_domain::ReaderGap> {
        let watching = device
            .reader
            .lock()
            .expect("the reader's state")
            .watching
            .clone()
            .expect("a reader is composed");
        device
            .stores
            .lock()
            .expect("the stores")
            .journal
            .open_reader_gap_for(&watching.reader)
            .expect("read the open gap")
    }

    #[test]
    fn a_transport_that_died_opens_a_confirmed_gap() {
        let (device, _directory) = a_device_watching_a_reader();
        assert!(open_gap(&device).is_none(), "nothing is wrong yet");

        record_connection(&device, Some(Disconnection::Ended));

        let gap = open_gap(&device).expect("the disconnection opened a gap");
        // The milestone. Until the provider could say the port died, every gap this service
        // could open was `suspected` — because silence was the only evidence it had, and a
        // quiet checkpoint produces exactly the same silence.
        assert_eq!(gap.detection, GapDetection::Confirmed);
        assert_eq!(
            device.reader.lock().expect("the reader's state").state,
            Some(ReaderState::Disconnected)
        );
    }

    #[test]
    fn a_reconnection_closes_the_gap_the_disconnection_opened() {
        let (device, _directory) = a_device_watching_a_reader();

        record_connection(&device, Some(Disconnection::NotOpened));
        assert!(open_gap(&device).is_some());

        record_connection(&device, None);

        assert!(
            open_gap(&device).is_none(),
            "a gap that ended must stop degrading health, or the device is permanently sick"
        );
        assert_eq!(
            device.reader.lock().expect("the reader's state").state,
            Some(ReaderState::Connected)
        );
    }

    #[test]
    fn a_reconnection_restarts_the_silence_clock() {
        let (device, _directory) = a_device_watching_a_reader();

        // Backdated well past any threshold: without the reset below, the watchdog would
        // open a *suspected* gap moments after a real reconnection closed a confirmed one.
        let stale = Instant::now() - std::time::Duration::from_secs(3_600);
        device.update_reader(|status| status.last_message = Some(stale));

        record_connection(&device, None);

        let last = device
            .reader
            .lock()
            .expect("the reader's state")
            .last_message
            .expect("a reader is composed");
        assert!(
            last > stale,
            "a connection is evidence the reader is alive, and is the only such evidence a \
             reader in an empty field will produce"
        );
    }

    #[test]
    fn repeated_reports_of_the_same_outage_write_one_gap() {
        let (device, _directory) = a_device_watching_a_reader();

        // The provider reports every failed reconnection rather than tracking which it has
        // already mentioned. That is only safe because opening is idempotent here.
        for _ in 0..5 {
            record_connection(&device, Some(Disconnection::NotOpened));
        }

        let gap = open_gap(&device).expect("a gap");
        let all = device
            .stores
            .lock()
            .expect("the stores")
            .journal
            .recent_reader_gaps(16)
            .expect("read the gaps");
        assert_eq!(all.len(), 1, "five reports, one gap: {all:?}");
        assert_eq!(gap.detection, GapDetection::Confirmed);
    }

    /// Runs events through the read path exactly as a provider's channel would deliver them.
    fn deliver(device: &Device, events: Vec<ReaderEvent>) {
        let (sender, receiver) = tokio::sync::mpsc::channel(events.len().max(1));
        for event in events {
            sender
                .try_send(event)
                .expect("the channel has room for every event");
        }
        drop(sender);
        read_into_journal(device, receiver, Ingest::default());
    }

    fn undecodable_reasons(device: &Device) -> Vec<String> {
        device
            .health()
            .degraded_by
            .into_iter()
            .filter(|reason| reason.contains("could not be decoded"))
            .collect()
    }

    #[test]
    fn frames_that_arrive_intact_and_never_decode_degrade_health() {
        // The review found these counts were read only in tests, so M3a's promise that a wrong
        // assumption shows up as "no reads and a climbing error count" was a promise about a
        // number nobody could see.
        let (device, _directory) = a_device_watching_a_reader();

        deliver(
            &device,
            vec![ReaderEvent::Faults(ReaderFaults {
                framing: 4,
                decoding: 0,
            })],
        );
        let health = device.health();
        assert_eq!(health.reader.framing_faults, 4);
        assert!(
            undecodable_reasons(&device).is_empty(),
            "line noise alone is reported, not degraded on"
        );

        deliver(
            &device,
            vec![ReaderEvent::Faults(ReaderFaults {
                framing: 4,
                decoding: 12,
            })],
        );
        let health = device.health();
        assert_eq!(health.reader.decode_faults, 12);
        assert!(health.is_degraded());
        let reasons = undecodable_reasons(&device);
        assert_eq!(reasons.len(), 1, "{:?}", health.degraded_by);
        assert!(
            reasons[0].contains("mat") && reasons[0].contains("12"),
            "names the reader and the count: {:?}",
            reasons[0]
        );
        assert!(
            !reasons[0].contains("  "),
            "collapsed line continuation: {:?}",
            reasons[0]
        );
    }

    /// A read from the fixture's reader, with whatever the reader said about time.
    fn a_read(device: &Device, chip: &str, timestamp: ReaderTimestamp) -> ReaderEvent {
        let source = device
            .reader
            .lock()
            .expect("the reader's state")
            .watching
            .clone()
            .expect("a reader is composed")
            .reader;
        ReaderEvent::Read(splitforge_reader::ReaderMessage {
            source,
            antenna: Some(1),
            chip: splitforge_domain::ChipId::new(chip),
            timestamp,
            rssi_dbm: None,
            raw_payload: vec![0x01],
        })
    }

    fn journaled(device: &Device) -> u64 {
        device
            .stores
            .lock()
            .expect("the stores")
            .journal
            .count()
            .expect("count the journal")
    }

    #[test]
    fn a_clean_stop_records_where_the_next_start_can_begin() {
        // ADR-0037. Appends record a checkpoint at most once a minute, so without this a
        // restart read up to a minute of the sidecar it had just written.
        let (device, _directory) = a_device_watching_a_reader();
        let before = device
            .stores
            .lock()
            .expect("the stores")
            .journal
            .latest_checkpoint()
            .expect("read the checkpoint");
        deliver(
            &device,
            vec![
                a_read(&device, "E200", ReaderTimestamp::Absent),
                a_read(&device, "E201", ReaderTimestamp::Absent),
            ],
        );

        checkpoint_before_exit(&device);

        let stores = device.stores.lock().expect("the stores");
        let after = stores
            .journal
            .latest_checkpoint()
            .expect("read the checkpoint")
            .expect("a checkpoint was recorded");
        assert_ne!(Some(&after), before.as_ref(), "a new one, not the start's");
        assert_eq!(after.through_seq, stores.journal.count().expect("count"));
    }

    #[test]
    fn health_answers_while_the_read_path_holds_the_journal() {
        // The 2026-09-13 review, from code: every health request ran its counting queries
        // under the lock the read path appends through. On an SD card an append fsyncs twice
        // while it holds that lock, so a monitor polling health added its queries to the read
        // path's latency, and waited out every append in turn.
        let (device, _directory) = a_device_watching_a_reader();
        deliver(
            &device,
            vec![a_read(&device, "E200", ReaderTimestamp::Absent)],
        );
        let device = &device;

        let held = device.stores.lock().expect("hold the read path's lock");
        std::thread::scope(|scope| {
            let (answered, answer) = std::sync::mpsc::channel();
            scope.spawn(move || {
                let _ = answered.send(device.health());
            });
            let health = answer.recv_timeout(std::time::Duration::from_secs(5));
            drop(held);

            let health = health.expect("health waited on the read path's lock");
            assert_eq!(
                health.raw_reads, 1,
                "health reads what the read path committed, on a connection of its own"
            );
        });
    }

    #[test]
    fn a_write_that_fails_is_retried_rather_than_ending_the_read_path() {
        // The 2026-09-13 review, from code: `read_into_journal` returned on the first failed
        // append, and the process stayed up, so `Restart=always` never fired and every read
        // after it was dropped. The trigger here is the one it named first that needs no
        // hardware: SQLITE_BUSY lasting longer than the 5 s busy timeout.
        let (device, _directory) = a_device_watching_a_reader();

        let (locked, release) = std::sync::mpsc::channel::<()>();
        let database = device.database.clone();
        let blocker = std::thread::spawn(move || {
            let connection = rusqlite::Connection::open(database).expect("a second connection");
            connection
                .execute_batch("BEGIN IMMEDIATE")
                .expect("take the write lock");
            locked.send(()).expect("say the lock is held");
            std::thread::sleep(std::time::Duration::from_secs(6));
            connection.execute_batch("ROLLBACK").expect("release it");
        });
        release.recv().expect("the lock is held");

        let events = vec![a_read(&device, "E200", ReaderTimestamp::Absent)];
        std::thread::scope(|scope| {
            let reading = scope.spawn(|| deliver(&device, events));

            // While it retries, health says so. The read path holds no lock between attempts,
            // which is what lets the endpoint answer at all.
            let deadline = Instant::now() + std::time::Duration::from_secs(20);
            let reason = loop {
                let degraded = device.health().degraded_by;
                if let Some(reason) = degraded
                    .into_iter()
                    .find(|reason| reason.contains("not being stored"))
                {
                    break reason;
                }
                assert!(
                    Instant::now() < deadline,
                    "health never said reads were not being stored"
                );
                std::thread::sleep(std::time::Duration::from_millis(50));
            };
            assert!(!reason.contains("  "), "collapsed continuation: {reason:?}");

            reading.join().expect("the read path finished");
        });
        blocker.join().expect("the blocker finished");

        let status = device.reader.lock().expect("the reader's state");
        assert_eq!(
            status.persisted, 1,
            "the read was stored once the lock let go"
        );
        assert!(
            status.fault.is_none(),
            "a write that lands clears the fault"
        );
        drop(status);
        assert_eq!(journaled(&device), 1);
    }

    #[test]
    fn a_read_the_journal_cannot_store_does_not_stop_the_reads_behind_it() {
        // The other trigger the review named: one value the schema cannot hold. An uptime at
        // or above 2^63 microseconds will never fit a signed SQLite integer, so retrying it
        // forever would stop the read path as surely as giving up did.
        let (device, _directory) = a_device_watching_a_reader();

        let events = vec![
            a_read(
                &device,
                "E200",
                ReaderTimestamp::Uptime { micros: u64::MAX },
            ),
            a_read(&device, "E201", ReaderTimestamp::Absent),
        ];
        deliver(&device, events);

        let status = device.reader.lock().expect("the reader's state");
        assert_eq!(status.persisted, 1, "the read behind it was stored");
        assert_eq!(status.set_aside, 1);
        drop(status);
        assert_eq!(journaled(&device), 1);

        // Not silent, and not dependent on a log line surviving.
        let trail = device
            .stores
            .lock()
            .expect("the stores")
            .config
            .audit_trail(10)
            .expect("read the audit trail");
        let set_aside: Vec<_> = trail
            .iter()
            .filter(|entry| entry.action == "journal.unstorable")
            .collect();
        assert_eq!(set_aside.len(), 1, "{trail:?}");
        assert_eq!(set_aside[0].actor, SERVICE_ACTOR);
        let detail = set_aside[0].detail.as_deref().expect("detail");
        assert!(
            detail.contains("E200"),
            "the read is identifiable: {detail}"
        );
        assert!(
            detail.contains(&u64::MAX.to_string()),
            "the value that could not be stored is kept exactly: {detail}"
        );

        let health = device.health();
        assert_eq!(health.reader.reads_set_aside, 1);
        let reasons: Vec<_> = health
            .degraded_by
            .iter()
            .filter(|reason| reason.contains("journal.unstorable"))
            .collect();
        assert_eq!(reasons.len(), 1, "{:?}", health.degraded_by);
        assert!(
            !reasons[0].contains("  "),
            "collapsed continuation: {reasons:?}"
        );
    }

    #[test]
    fn one_decoded_read_proves_the_layout_and_later_refusals_stop_degrading() {
        let (device, _directory) = a_device_watching_a_reader();
        let reader = device
            .reader
            .lock()
            .expect("the reader's state")
            .watching
            .clone()
            .expect("a reader is composed")
            .reader;

        deliver(
            &device,
            vec![
                ReaderEvent::Faults(ReaderFaults {
                    framing: 0,
                    decoding: 3,
                }),
                ReaderEvent::Read(splitforge_reader::ReaderMessage {
                    source: reader,
                    antenna: Some(1),
                    chip: splitforge_domain::ChipId::new("E200"),
                    timestamp: splitforge_reader::ReaderTimestamp::Absent,
                    rssi_dbm: None,
                    raw_payload: vec![0x01],
                }),
                ReaderEvent::Faults(ReaderFaults {
                    framing: 0,
                    decoding: 9,
                }),
            ],
        );

        let health = device.health();
        assert_eq!(health.reader.decode_faults, 9, "still reported");
        assert_eq!(health.reader.reads_persisted, 1);
        assert!(
            undecodable_reasons(&device).is_empty(),
            "a frame type nobody has captured, arriving between runners, must not flip health \
             on and off all day: {:?}",
            health.degraded_by
        );
    }

    #[test]
    fn a_fault_report_is_not_proof_the_reader_is_alive() {
        // The watchdog's clock. A reader sending only what cannot be decoded is recording
        // nothing, and the suspected gap that silence opens is the truth about that.
        let (device, _directory) = a_device_watching_a_reader();
        let stale = Instant::now() - std::time::Duration::from_secs(3_600);
        device.update_reader(|status| status.last_message = Some(stale));

        deliver(
            &device,
            vec![ReaderEvent::Faults(ReaderFaults {
                framing: 1,
                decoding: 1,
            })],
        );

        let status = device.reader.lock().expect("the reader's state");
        assert_eq!(status.last_message, Some(stale));
        assert_eq!(status.received, 0);
    }

    #[test]
    fn the_detail_beside_a_gap_names_which_way_it_failed() {
        // Two different diagnoses. One says look for a cable, the other says something that
        // was working stopped — and an operator reads the second very differently.
        let causes = [
            Disconnection::NotOpened,
            Disconnection::Ended,
            Disconnection::NotStarted,
        ];
        let mut details: Vec<&str> = causes.iter().map(|cause| cause.detail()).collect();
        details.sort_unstable();
        details.dedup();
        assert_eq!(
            details.len(),
            causes.len(),
            "three diagnoses, three sentences"
        );

        for cause in causes {
            let detail = cause.detail();
            // The defect that has shipped four times in this repository: a message written
            // across two source lines whose trailing backslash was dropped still compiles,
            // still passes every test that checks the fields around it, and prints the
            // source file's indentation into the middle of the sentence.
            assert!(
                !detail.contains("  "),
                "collapsed line continuation: {detail:?}"
            );
            assert!(!detail.is_empty());
        }
    }

    /// Starts the race the device is watching, so the silence watchdog does something.
    ///
    /// `load_fixture` configures a race and does not start one, and `assess_silence` is
    /// [`SilenceVerdict::Idle`] while none is running — so a watchdog test that skipped this
    /// would pass without the watchdog ever having made a decision.
    fn start_the_race(device: &Device) {
        let race = device
            .reader
            .lock()
            .expect("the reader's state")
            .watching
            .as_ref()
            .expect("a reader is composed")
            .race;

        device
            .stores
            .lock()
            .expect("the stores")
            .config
            .record_session(
                race,
                splitforge_domain::SessionAction::Start,
                time::OffsetDateTime::now_utc(),
                "test",
                None,
            )
            .expect("start the race");
    }

    #[test]
    fn the_watchdog_leaves_a_gap_the_transport_opened_alone() {
        let (device, _directory) = a_device_watching_a_reader();
        start_the_race(&device);

        record_connection(&device, Some(Disconnection::Ended));
        assert!(open_gap(&device).is_some(), "the cable came out");

        // The tick that lands inside the first threshold's worth of a real outage. This
        // device last heard a read milliseconds ago and the threshold is two minutes, so
        // `assess_silence` reads *not silent long enough yet* as `Close` — a verdict that
        // means "reads are arriving" and is being reached here because none have stopped
        // arriving for long enough to notice, not because any arrived.
        check_for_silence(&device);

        assert!(
            open_gap(&device).is_some(),
            "the transport said the port died and nothing has said otherwise; closing the \
             gap here records that reads resumed at a moment when nothing resumed"
        );
    }

    #[test]
    fn the_watchdog_still_closes_a_gap_when_reads_actually_resume() {
        // The other side of the guard, so it cannot be satisfied by disabling the watchdog:
        // a *suspected* gap must still close on its own evidence.
        let (device, _directory) = a_device_watching_a_reader();
        start_the_race(&device);

        let stale = Instant::now() - std::time::Duration::from_secs(3_600);
        device.update_reader(|status| status.last_message = Some(stale));
        check_for_silence(&device);
        let gap = open_gap(&device).expect("an hour of silence during a race");
        assert_eq!(gap.detection, GapDetection::Suspected);

        // A read arrives, exactly as the read path records one.
        device.update_reader(|status| status.last_message = Some(Instant::now()));
        check_for_silence(&device);

        assert!(
            open_gap(&device).is_none(),
            "reads resumed, so the gap the watchdog opened must close"
        );
    }

    /// A configured database with no service around it, for the argument-checking paths.
    ///
    /// `compose_serial_reader` opens its own `ConfigStore`, so these need a path rather than
    /// a `Device` — and every assertion below is reached *before* any port is opened, which
    /// is what keeps them from spawning a reconnect loop that outlives the test.
    fn a_configured_database() -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let database = directory.path().join("event.db");
        let mut config = ConfigStore::open(&database).expect("open the configuration");
        splitforge_cli::load_fixture(&mut config, "test", "five-k").expect("load the fixture");
        (directory, database)
    }

    /// A `Device` for a database the caller already configured.
    fn a_device_on(database: &std::path::Path) -> Arc<Device> {
        let (journal, _recovery) =
            SqliteJournal::open_recovering(database, "test").expect("open the journal");
        let config = ConfigStore::open(database).expect("reopen the configuration");
        Arc::new(Device {
            database: database.to_path_buf(),
            started: Instant::now(),
            stores: Mutex::new(Stores { journal, config }),
            observed: Mutex::new(observer(database).expect("open the observer")),
            clock: RwLock::new(None),
            reader: Mutex::new(ReaderStatus::none()),
        })
    }

    #[test]
    fn naming_a_reader_the_database_does_not_have_lists_the_ones_it_does() {
        let (_directory, database) = a_configured_database();
        let device = a_device_on(&database);

        let error = compose_serial_reader(
            &device,
            "/dev/null",
            115_200,
            Region::Na,
            Some("nope"),
            &database,
        )
        .expect_err("an unknown reader cannot be composed");
        let message = format!("{error}");

        assert!(message.contains("nope"), "{message}");
        // The fixture's reader, so the operator is told what to type rather than only that
        // they were wrong.
        assert!(message.contains("mat"), "{message}");
        // The defect that has shipped four times in this repository: a message written
        // across two source lines whose trailing backslash was dropped still compiles.
        assert!(
            !message.contains("  "),
            "collapsed line continuation: {message}"
        );
    }

    #[test]
    fn two_readers_and_no_selector_is_an_error_that_says_which_flag_to_pass() {
        let (_directory, database) = a_configured_database();
        {
            let mut config = ConfigStore::open(&database).expect("open the configuration");
            config
                .save_reader(&splitforge_domain::Reader {
                    id: ReaderId::new("second-mat"),
                    label: "a second reader".to_owned(),
                    endpoint: None,
                    timestamp_trust: splitforge_domain::TimestampTrust::default(),
                })
                .expect("save a second reader");
        }
        let device = a_device_on(&database);

        let error =
            compose_serial_reader(&device, "/dev/null", 115_200, Region::Na, None, &database)
                .expect_err("two readers and no --reader cannot be resolved");
        let message = format!("{error}");

        // A gap is recorded *against a reader*, so guessing which one would put evidence on
        // the wrong row rather than merely picking a default.
        assert!(message.contains("--reader"), "{message}");
        assert!(
            !message.contains("  "),
            "collapsed line continuation: {message}"
        );
    }

    #[test]
    fn the_reader_composed_is_the_one_named_and_it_is_reported_as_serial() {
        let (_directory, database) = a_configured_database();
        let device = a_device_on(&database);

        // `/dev/null` opens and returns end-of-stream rather than refusing, so the provider
        // starts, reports a lifecycle, and reconnects — which is all this asserts. What it
        // must not do is guess a reader or claim a connection the composition root has not
        // been told about.
        compose_serial_reader(
            &device,
            "/dev/null",
            115_200,
            Region::Na,
            Some("mat"),
            &database,
        )
        .expect("the fixture's reader composes");

        let status = device.reader.lock().expect("the reader's state");
        assert_eq!(status.kind, ReaderKind::Serial);
        assert_eq!(
            status.state,
            Some(ReaderState::Disconnected),
            "a composition root cannot report a connection the provider has not announced"
        );
        assert_eq!(
            status
                .watching
                .as_ref()
                .expect("a reader is watched")
                .reader
                .as_str(),
            "mat"
        );
    }

    #[test]
    fn a_module_and_a_scenario_cannot_be_composed_at_once() {
        use clap::Parser as _;

        // Enforced by clap rather than by an `if` in `main`, so the service never has to
        // arbitrate between a real module and a scripted one at runtime.
        Args::try_parse_from([
            "splitforge-edge",
            "--serial",
            "/dev/splitforge-reader",
            "--simulate",
            "five-k",
        ])
        .expect_err("--serial and --simulate are mutually exclusive");

        Args::try_parse_from(["splitforge-edge", "--serial", "/dev/ttyUSB0"])
            .expect_err("a module transmits, so --serial needs a region and there is no default");
        Args::try_parse_from(["splitforge-edge", "--region", "na"])
            .expect_err("a region with nothing to set it on is a mistake worth refusing");
        Args::try_parse_from([
            "splitforge-edge",
            "--serial",
            "/dev/ttyUSB0",
            "--region",
            "open",
        ])
        .expect_err("OPEN is not a region the service offers");

        let serial = Args::try_parse_from([
            "splitforge-edge",
            "--serial",
            "/dev/ttyUSB0",
            "--region",
            "na",
        ])
        .expect("--serial with a region parses");
        assert_eq!(serial.serial.as_deref(), Some("/dev/ttyUSB0"));
        assert_eq!(serial.region, Some(Region::Na));
        assert_eq!(
            serial.serial_baud, 115_200,
            "the module's own default, so an operator who omits it gets what the guide says"
        );
    }
}
