//! `port::open` against a real tty device.
//!
//! Every other test of this crate's connection lifecycle supplies a fake that implements
//! `Read`, which is the right way to test the lifecycle and says nothing about the one
//! function that opens a device: `serialport::new(path, baud).timeout(t).open()`. That call
//! has never opened anything. It sets termios attributes, and what it does to a device that
//! is not a UART is a question no fake can answer.
//!
//! A pseudo-terminal is the closest device a machine with no module can offer. It is a real
//! tty: the same `open`, the same `tcsetattr`, the same `read` returning what a timeout
//! returns. What it is not is a USB serial bridge — nothing here says how a real one behaves
//! when its cable is pulled, which is the module's half of Milestone 3a.
//!
//! Unix only, because a pty is.

#![cfg(unix)]

use std::io::{ErrorKind, Read as _, Write as _};
use std::os::fd::{AsFd as _, OwnedFd};
use std::time::{Duration, Instant};

use splitforge_thingmagic::port::{PortFactory as _, SerialSettings, serial};

/// One end of a pseudo-terminal, and the path of the other.
struct Pty {
    controller: std::fs::File,
    device: String,
}

/// Opens a pseudo-terminal pair.
///
/// `rustix` rather than `libc` so the `unsafe` this needs is inside a crate that audits it,
/// per CONTRIBUTING's workspace-wide ban.
fn pty() -> Pty {
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

    let controller: OwnedFd =
        openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).expect("open a pseudo-terminal");
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

fn settings(device: &str, timeout: Duration) -> SerialSettings {
    SerialSettings {
        path: device.to_owned(),
        baud: 115_200,
        read_timeout: timeout,
    }
}

#[test]
fn the_adapter_opens_a_real_tty_and_reads_what_is_written_to_it() {
    let mut pty = pty();
    let mut factory = serial(settings(&pty.device, Duration::from_millis(250)));

    let mut port = factory.open().expect("a tty is a port");

    pty.controller.write_all(b"hello").expect("write");
    pty.controller.flush().expect("flush");

    let mut buffer = [0_u8; 16];
    let count = port.read(&mut buffer).expect("read what was written");
    assert_eq!(&buffer[..count], b"hello");
}

#[test]
fn the_adapter_writes_to_a_real_tty() {
    // The half that had no implementation at all until the start sequence: `Port` was
    // read-only, so nothing had ever written a byte to a device.
    let mut pty = pty();
    let mut factory = serial(settings(&pty.device, Duration::from_millis(250)));
    let mut port = factory.open().expect("a tty is a port");

    port.write_all(b"\xFF\x00\x03\x1D\x0C").expect("write");
    port.flush().expect("flush");

    let mut buffer = [0_u8; 16];
    let count = pty.controller.read(&mut buffer).expect("read the command");
    assert_eq!(&buffer[..count], b"\xFF\x00\x03\x1D\x0C");
}

#[test]
fn an_idle_port_times_out_rather_than_reporting_the_connection_gone() {
    // **The branch M3a names as unverified.** `pump` treats `TimedOut` as a quiet module and
    // anything else as a dead connection, so what an idle device returns decides whether the
    // port is reopened between every pair of runners. On a tty it is `TimedOut`, and it takes
    // the timeout to arrive.
    let pty = pty();
    let timeout = Duration::from_millis(250);
    let mut factory = serial(settings(&pty.device, timeout));
    let mut port = factory.open().expect("a tty is a port");

    let mut buffer = [0_u8; 16];
    let began = Instant::now();
    let error = port.read(&mut buffer).expect_err("nothing was written");
    let waited = began.elapsed();

    assert_eq!(
        error.kind(),
        ErrorKind::TimedOut,
        "an idle port must not look like a disconnection"
    );
    assert!(
        waited >= timeout / 2,
        "returned in {waited:?}, so it did not wait for the timeout"
    );
}

#[test]
fn a_device_that_is_not_there_is_an_error_rather_than_a_panic() {
    let mut factory = serial(settings(
        "/dev/splitforge-no-such-device",
        Duration::from_secs(1),
    ));
    // Matched rather than `expect_err`, because a `Port` is a boxed trait object with no
    // `Debug` to print.
    let Err(error) = factory.open() else {
        panic!("a device that does not exist must not open");
    };
    assert_eq!(
        error.kind(),
        ErrorKind::NotFound,
        "the kind has to survive, or nothing downstream can tell a missing device from one \
         this account may not open: {error:?}"
    );
    let text = error.to_string();
    assert!(
        text.contains("/dev/splitforge-no-such-device"),
        "an operator reading this in journald needs the path: {text:?}"
    );
}

#[test]
fn a_path_that_is_not_a_tty_is_refused_and_names_itself() {
    // The operator error this actually catches: `--serial` pointed at something that is not a
    // device. The file opens and `tcgetattr` refuses it, which serialport reports as
    // `Unknown` — so the kind stays `Other` here, honestly, and the path is what tells an
    // operator what they typed.
    let file = tempfile::NamedTempFile::new().expect("a temporary file");
    let path = file.path().display().to_string();
    let mut factory = serial(settings(&path, Duration::from_secs(1)));

    let Err(error) = factory.open() else {
        panic!("a regular file is not a serial port");
    };
    assert!(
        error.to_string().contains(&path),
        "an operator who mistyped a path has to see which one: {error:?}"
    );
}
