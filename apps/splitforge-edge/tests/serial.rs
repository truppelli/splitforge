//! The service against a module, as far as a machine with no module can go.
//!
//! `apps/splitforge-edge/tests/service.rs` runs the real binary against a real socket. This
//! runs it against a real **device**: a pseudo-terminal, opened by the same `--serial` an
//! operator types, with something on the other end that speaks the protocol. Between them
//! these cover every part of Milestone 3a's read path except the module itself.
//!
//! # What this can show
//!
//! That the service opens a tty at all; that it sends the start sequence, in order, byte for
//! byte; that reports streamed back become rows in the journal; that a connection lost is a
//! confirmed gap; and that a module which refuses a command is a reader that never connected
//! rather than a silent one.
//!
//! # What it cannot
//!
//! **That a real module answers any of this.** The fake here is built from the same sources
//! the adapter was ([ADR-0033](../../../docs/adr/0033-each-connection-starts-the-stream.md)),
//! so the two agree by construction — which is exactly the failure this crate has already
//! shipped once, in `crc.rs`, and is why the M3a boxes stay unticked until hardware answers.
//! What a pty also is not is a USB serial bridge: nothing here says what a real one does when
//! its cable is pulled, only what this code does when a device stops answering.
//!
//! Unix only, because a pty is.

#![cfg(unix)]

use std::io::{Read as _, Write as _};
use std::os::fd::{AsFd as _, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use splitforge_storage::ConfigStore;
use splitforge_thingmagic::crc::{CAPTURED_FRAME, crc16};
use splitforge_thingmagic::{OpCode, Region, StartSequence};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;
use tokio::process::{Child, Command as Process};

/// The service binary, built by Cargo for this test run.
const SERVICE: &str = env!("CARGO_BIN_EXE_splitforge-edge");

/// The fixture, which configures one race and one reader — so `--reader` is not needed.
const FIXTURE: &str = "five-k";

/// A pseudo-terminal: one end for the fake module, the other a path for `--serial`.
struct Pty {
    controller: std::fs::File,
    device: String,
}

fn pty() -> Pty {
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

    let controller: OwnedFd =
        openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).expect("open a pseudo-terminal");

    // Non-blocking, so the fake module's reader can notice it has been unplugged. A blocking
    // read on a controller whose device end is open waits forever, which is a test that hangs
    // in teardown rather than one that fails.
    rustix::io::ioctl_fionbio(controller.as_fd(), true).expect("non-blocking");

    // **Close-on-exec, or the cable cannot be pulled.** `posix_openpt` does not set it, unlike
    // everything `std::fs` opens, so the service this test spawns inherits the controller end
    // and holds the pty open itself. Dropping every copy in this process then closes nothing:
    // the device stays open, the service reads on undisturbed, and the disconnection never
    // happens. Worth knowing beyond this file — a fake module is a subprocess away from the
    // same trap.
    rustix::io::fcntl_setfd(controller.as_fd(), rustix::io::FdFlags::CLOEXEC).expect("cloexec");
    grantpt(controller.as_fd()).expect("grant");
    unlockpt(controller.as_fd()).expect("unlock");
    let device = ptsname(controller.as_fd(), Vec::new())
        .expect("name the device end")
        .into_string()
        .expect("a pty path is ASCII");

    Pty {
        controller: std::fs::File::from(controller),
        device,
    }
}

/// A module-to-host frame: header, length, opcode, status, data, CRC.
fn response(opcode: u8, status: u16, data: &[u8]) -> Vec<u8> {
    let mut frame = vec![
        0xFF,
        u8::try_from(data.len()).expect("small"),
        opcode,
        (status >> 8) as u8,
        (status & 0xFF) as u8,
    ];
    frame.extend_from_slice(data);
    frame.extend_from_slice(&crc16(&frame[1..]).to_be_bytes());
    frame
}

/// A tag report for `epc`, built from the captured frame so its layout is one a module
/// actually produced.
fn tag_report(epc: [u8; 12]) -> Vec<u8> {
    // The payload is everything between the five-byte header and the two-byte CRC; the EPC
    // sits after the PC word, 26 bytes in.
    let mut payload = CAPTURED_FRAME[5..CAPTURED_FRAME.len() - 2].to_vec();
    payload[26..38].copy_from_slice(&epc);
    response(OpCode::ReadTagIdMultiple.to_byte(), 0x0000, &payload)
}

/// What the fake module was asked to do when it is asked for the region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answers {
    /// Accept every command, then stream.
    Everything,
    /// Refuse the region, as a module configured for another one would.
    ButNotTheRegion,
}

/// A module on the other end of a pty: answers commands, then streams tag reports.
///
/// **Two threads, and the reason is the device.** A pty controller has no read timeout, so a
/// thread that both answered commands and streamed reports would sit blocked in `read` for as
/// long as the service had nothing to say — which, once the stream is running, is forever. And
/// a controller whose device end has closed returns `EIO` rather than end-of-file, on every
/// read, so the reader has to treat a closed port as "wait for it to reopen" rather than as the
/// end. That is what a reconnection looks like from the module's side, and getting it wrong
/// made this fake stop answering after the service's first retry.
struct FakeModule {
    /// Every command frame the service sent, in order, as raw bytes.
    heard: Arc<Mutex<Vec<u8>>>,
    /// Set once the start command has been accepted.
    streaming: Arc<AtomicBool>,
    /// Cleared to stop both threads: this test's cable pull.
    plugged_in: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl FakeModule {
    /// Starts a module on `pty`'s controller end.
    fn answering(pty: Pty, answers: Answers, tags: Vec<[u8; 12]>) -> Self {
        let heard = Arc::new(Mutex::new(Vec::new()));
        let streaming = Arc::new(AtomicBool::new(false));
        let plugged_in = Arc::new(AtomicBool::new(true));
        let (send_frame, to_write) = std::sync::mpsc::channel::<Vec<u8>>();

        let mut reading = pty.controller.try_clone().expect("clone the controller");
        let mut writing = pty.controller;

        // Reads commands, answers them through the writer.
        let reader = std::thread::spawn({
            let heard = Arc::clone(&heard);
            let streaming = Arc::clone(&streaming);
            let plugged_in = Arc::clone(&plugged_in);
            move || {
                let mut pending = Vec::new();
                // The read power it was last set to, in hundredths of a dBm, big-endian.
                let mut power = [0x00_u8, 0x00];
                let mut chunk = [0_u8; 512];

                while plugged_in.load(Ordering::SeqCst) {
                    match reading.read(&mut chunk) {
                        Ok(0) => {
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Ok(count) => pending.extend_from_slice(&chunk[..count]),
                        // Nothing to read yet. Whatever is half-read stays half-read.
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        // No device end open: the service is between connections. On a pty
                        // controller that is `EIO` rather than end-of-file, so this waits for
                        // the port to be reopened rather than treating a retry as the end —
                        // and drops a partial frame, which belonged to the old connection.
                        Err(_) => {
                            pending.clear();
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                    }

                    // A command frame is header, length, opcode, data, CRC — no status word.
                    while pending.len() >= 5 {
                        let frame_len = 5 + usize::from(pending[1]);
                        if pending.len() < frame_len {
                            break;
                        }
                        let frame: Vec<u8> = pending.drain(..frame_len).collect();
                        assert_eq!(frame[0], 0xFF, "every command starts with the header byte");
                        assert_eq!(
                            crc16(&frame[1..frame_len - 2]),
                            u16::from_be_bytes([frame[frame_len - 2], frame[frame_len - 1]]),
                            "the service sent a frame whose CRC does not match its bytes"
                        );

                        heard.lock().expect("heard").extend_from_slice(&frame);

                        let opcode = frame[2];
                        let body = &frame[3..frame_len - 2];
                        let refuse = answers == Answers::ButNotTheRegion
                            && opcode == OpCode::SetRegion.to_byte();
                        let status = if refuse { 0x0105 } else { 0x0000 };
                        // A multi-protocol answer echoes the option byte it was given. The read
                        // power it reports is the one it was last set to, with the Hecto's range
                        // (ADR-0038), laid out as MercuryAPI's GetReadTxPowerWithLimits reads it.
                        if opcode == OpCode::SetReadTxPower.to_byte() {
                            power = [body[0], body[1]];
                        }
                        let echo: Vec<u8> = if opcode == OpCode::MultiProtocolTagOp.to_byte() {
                            vec![body[2]]
                        } else if opcode == OpCode::GetReadTxPower.to_byte() {
                            vec![0x01, power[0], power[1], 0x0A, 0x8C, 0x00, 0x00]
                        } else {
                            Vec::new()
                        };
                        if send_frame.send(response(opcode, status, &echo)).is_err() {
                            return;
                        }

                        let started = opcode == OpCode::MultiProtocolTagOp.to_byte()
                            && body[2] == 0x01
                            && !refuse;
                        if started {
                            streaming.store(true, Ordering::SeqCst);
                        } else if opcode == OpCode::MultiProtocolTagOp.to_byte() && body[2] == 0x02
                        {
                            // The stop the next connection opens with.
                            streaming.store(false, Ordering::SeqCst);
                        }
                    }
                }
            }
        });

        // Owns every write, so an answer and a tag report can never interleave mid-frame.
        let writer = std::thread::spawn({
            let streaming = Arc::clone(&streaming);
            let plugged_in = Arc::clone(&plugged_in);
            move || {
                let mut streamed = 0_usize;
                while plugged_in.load(Ordering::SeqCst) {
                    if let Ok(frame) = to_write.recv_timeout(Duration::from_millis(20))
                        && writing.write_all(&frame).is_err()
                    {
                        return;
                    }
                    if streaming.load(Ordering::SeqCst) && streamed < tags.len() {
                        if writing.write_all(&tag_report(tags[streamed])).is_err() {
                            return;
                        }
                        streamed += 1;
                    }
                }
            }
        });

        Self {
            heard,
            streaming,
            plugged_in,
            threads: vec![reader, writer],
        }
    }

    /// The command frames the service has sent so far.
    fn heard(&self) -> Vec<u8> {
        self.heard.lock().expect("heard").clone()
    }

    /// Waits until `ready` accepts what has been heard, for up to five seconds.
    fn poll_until(&self, ready: impl Fn(&[u8]) -> bool) -> Option<Vec<u8>> {
        for _ in 0..250 {
            let heard = self.heard();
            if ready(&heard) {
                return Some(heard);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }

    fn is_streaming(&self) -> bool {
        self.streaming.load(Ordering::SeqCst)
    }

    /// Closes the module's end of the port: the cable, pulled.
    fn unplug(&mut self) {
        self.plugged_in.store(false, Ordering::SeqCst);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// The service, started against a pty and a fixture database.
struct Service {
    directory: tempfile::TempDir,
    socket: PathBuf,
    child: Child,
}

impl Service {
    async fn start(device: &str) -> Self {
        let directory = tempfile::tempdir().expect("tempdir");
        let database = directory.path().join("event.db");
        let socket = directory.path().join("api.sock");

        let mut store = ConfigStore::open(&database).expect("open the configuration");
        splitforge_cli::load_fixture(&mut store, "test", FIXTURE).expect("load the fixture");
        drop(store);

        let child = Process::new(SERVICE)
            .args(["--database", database.to_str().expect("utf-8")])
            .args(["--socket", socket.to_str().expect("utf-8")])
            .args(["--serial", device])
            .args(["--region", "na"])
            .args(["--read-power", "22.5"])
            .spawn()
            .expect("start splitforge-edge");

        for _ in 0..400 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(socket.exists(), "the service never created its socket");

        Self {
            directory,
            socket,
            child,
        }
    }

    async fn health(&self) -> serde_json::Value {
        let mut stream = UnixStream::connect(&self.socket).await.expect("connect");
        stream
            .write_all(
                b"GET /health HTTP/1.1\r\nHost: splitforge\r\nConnection: close\r\n\r\n".as_slice(),
            )
            .await
            .expect("write");
        let mut text = String::new();
        stream.read_to_string(&mut text).await.expect("read");
        let (_head, body) = text.split_once("\r\n\r\n").expect("a body");
        serde_json::from_str(body).expect("JSON")
    }

    /// Polls `/health` until `ready` accepts it, or gives up after ten seconds.
    async fn poll_until(
        &self,
        ready: impl Fn(&serde_json::Value) -> bool,
    ) -> Option<serde_json::Value> {
        for _ in 0..200 {
            let health = self.health().await;
            if ready(&health) {
                return Some(health);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        None
    }

    fn database(&self) -> PathBuf {
        self.directory.path().join("event.db")
    }

    /// Stops the service and hands back its directory, still alive.
    ///
    /// A `TempDir` deletes its contents when it drops, so a test that wants to look at the
    /// database *after* the service has exited has to keep the handle — the same trap
    /// `service.rs` records.
    async fn stop(mut self) -> tempfile::TempDir {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
        self.directory
    }
}

/// Rows in `raw_reads`, read from the file rather than from the service.
fn journalled(database: &Path) -> u64 {
    use splitforge_domain::RawReadJournal as _;
    let journal = splitforge_storage::SqliteJournal::open(database).expect("open the journal");
    journal.count().expect("count")
}

#[tokio::test]
async fn the_service_opens_a_device_starts_a_stream_and_journals_what_arrives() {
    let pty = pty();
    let device = pty.device.clone();
    let tags = vec![
        [0xE2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x15, 0x01],
        [0xE2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x15, 0x02],
    ];
    let mut module = FakeModule::answering(pty, Answers::Everything, tags);
    let service = Service::start(&device).await;

    // The whole point: reports the module streamed are rows in the journal.
    let health = service
        .poll_until(|health| health["reader"]["reads_persisted"] == 2)
        .await
        .unwrap_or_else(|| panic!("two reads never arrived"));

    assert_eq!(health["reader"]["kind"], "serial");
    assert_eq!(health["reader"]["state"], "connected");
    assert_eq!(health["reader"]["decode_faults"], 0);
    assert_eq!(health["reader"]["open_gap"], serde_json::Value::Null);
    assert_eq!(health["status"], "ok", "{:?}", health["degraded_by"]);

    // And the service sent the start sequence, byte for byte.
    let mut expected = Vec::new();
    let power = "22.5".parse().expect("a power");
    for command in StartSequence::gen2(Region::Na, power).commands() {
        let mut out = [0_u8; 255];
        expected.extend_from_slice(command.encode(&mut out).expect("encode"));
    }
    assert_eq!(
        module.heard(),
        expected,
        "stop, version, Gen2, region, read power, its read-back, read filter off, start"
    );
    assert_eq!(
        health["reader"]["read_power"],
        serde_json::json!({"centi_dbm": 2250, "min_centi_dbm": 0, "max_centi_dbm": 2700}),
        "the power the module reported applying, from --read-power by way of the module"
    );

    // And what the reads are to be read against is on the audit trail, once.
    let trail = ConfigStore::open(service.database())
        .expect("open the configuration")
        .audit_trail(20)
        .expect("read the audit trail");
    let configured: Vec<_> = trail
        .iter()
        .filter(|entry| entry.action == "reader.configured")
        .collect();
    assert_eq!(configured.len(), 1, "{trail:?}");
    let detail = configured[0].detail.as_deref().expect("detail");
    assert!(detail.contains("\"read_power_centi_dbm\":2250"), "{detail}");
    assert!(detail.contains("\"region\":\"na\""), "{detail}");
    assert!(module.is_streaming());

    module.unplug();
    let _ = service.stop().await;
}

#[tokio::test]
async fn a_connection_that_ends_is_a_confirmed_gap_and_the_reads_before_it_survive() {
    let pty = pty();
    let device = pty.device.clone();
    let tags = vec![[0xE2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x15, 0x03]];
    let mut module = FakeModule::answering(pty, Answers::Everything, tags);
    let service = Service::start(&device).await;

    service
        .poll_until(|health| health["reader"]["reads_persisted"] == 1)
        .await
        .expect("the read arrived");

    module.unplug();

    let health = service
        .poll_until(|health| health["reader"]["open_gap"] != serde_json::Value::Null)
        .await
        .unwrap_or_else(|| panic!("the service never noticed the module was gone"));

    assert_eq!(
        health["reader"]["open_gap"]["detection"], "confirmed",
        "a device that stopped answering is not a guess"
    );
    assert_eq!(health["status"], "degraded");
    assert_eq!(
        health["reader"]["reads_persisted"], 1,
        "a disconnection does not disturb what was already stored"
    );

    let database = service.database();
    let _directory = service.stop().await;
    assert_eq!(journalled(&database), 1, "the row outlives the service");
}

#[tokio::test]
async fn a_module_that_refuses_a_command_never_reports_itself_connected() {
    // The failure an operator meets with the wrong region, and the reason the start sequence
    // reports its own refusal: silence and a refusal look identical on a port.
    let pty = pty();
    let device = pty.device.clone();
    let mut module = FakeModule::answering(pty, Answers::ButNotTheRegion, Vec::new());
    let service = Service::start(&device).await;

    let health = service
        .poll_until(|health| health["reader"]["open_gap"] != serde_json::Value::Null)
        .await
        .unwrap_or_else(|| panic!("a module that refuses the region must open a gap"));

    assert_eq!(health["reader"]["state"], "disconnected");
    assert_eq!(health["reader"]["reads_persisted"], 0);
    assert_eq!(health["reader"]["open_gap"]["detection"], "confirmed");

    // It retried, and stopped at the region every time. Polled rather than assumed: the gap
    // opens on the first refusal, and the second attempt is a backoff delay behind it.
    let opcodes = module
        .poll_until(|heard| command_opcodes(heard).len() >= 8)
        .map(|heard| command_opcodes(&heard))
        .unwrap_or_else(|| {
            panic!(
                "expected a second attempt, got {:02X?}",
                command_opcodes(&module.heard())
            )
        });
    assert!(
        !opcodes.contains(&OpCode::SetReaderOptionalParams.to_byte()),
        "nothing after the refused region should ever be sent: {opcodes:02X?}"
    );
    assert!(
        !module.is_streaming(),
        "a module that refused a command must not be streaming"
    );

    module.unplug();
    let _ = service.stop().await;
}

/// The opcode of every command frame in `bytes`.
fn command_opcodes(bytes: &[u8]) -> Vec<u8> {
    let mut opcodes = Vec::new();
    let mut rest = bytes;
    while rest.len() >= 5 {
        let frame_len = 5 + usize::from(rest[1]);
        if rest.len() < frame_len {
            break;
        }
        opcodes.push(rest[2]);
        rest = &rest[frame_len..];
    }
    opcodes
}

#[tokio::test]
async fn a_device_that_is_not_there_is_a_gap_rather_than_a_service_that_will_not_start() {
    // A Pi booting with nothing plugged in, which is the ordinary morning-of state.
    let service = Service::start("/dev/splitforge-no-such-device").await;

    let health = service
        .poll_until(|health| health["reader"]["open_gap"] != serde_json::Value::Null)
        .await
        .unwrap_or_else(|| panic!("a missing device must open a gap"));

    assert_eq!(health["reader"]["kind"], "serial");
    assert_eq!(health["reader"]["state"], "disconnected");
    assert_eq!(health["status"], "degraded");

    let _ = service.stop().await;
}
