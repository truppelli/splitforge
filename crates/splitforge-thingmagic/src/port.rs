//! Opening the thing the bytes come from.
//!
//! This is the only module in the crate that mentions `serialport`, and that containment is
//! deliberate. Everything above it takes a [`PortFactory`], so the connection lifecycle —
//! reconnect, backoff, resynchronization after a mid-frame disconnect — is exercised against
//! ports that fail exactly when a test wants them to, rather than against hardware nobody
//! has yet.

use std::io::{self, Read};
use std::time::Duration;

/// An open port, read as a plain byte stream.
///
/// Boxed rather than generic because a factory returns a *new* one on every reconnect, and
/// the concrete type is of no interest to anything above this module.
pub type Port = Box<dyn Read + Send>;

/// Opens a port, once per connection attempt.
///
/// Implemented for any suitable closure, so a test supplies one in a line and the real
/// adapter supplies [`serial`].
pub trait PortFactory: Send {
    /// Opens the port, or explains why it could not be opened.
    ///
    /// # Errors
    ///
    /// Whatever the underlying device reports. A failure here is expected rather than
    /// exceptional — an unplugged reader is the ordinary morning-of state — and the caller
    /// retries with backoff rather than giving up.
    fn open(&mut self) -> io::Result<Port>;
}

impl<F> PortFactory for F
where
    F: FnMut() -> io::Result<Port> + Send,
{
    fn open(&mut self) -> io::Result<Port> {
        self()
    }
}

/// How to reach a physical module.
#[derive(Debug, Clone)]
pub struct SerialSettings {
    /// The device path.
    ///
    /// Prefer the stable name a udev rule provides — `/dev/splitforge-reader` — over
    /// `/dev/ttyUSB0`, which renumbers on re-enumeration and, because the M7e-Pico carrier
    /// board has no USB of its own, actually names the USB-to-UART bridge rather than the
    /// module ([the reader notes](../../../docs/readers/thingmagic-m7e-pico.md)).
    pub path: String,
    /// Bits per second. The module accepts 9.6 k to 921.6 k; 115 200 is its default.
    pub baud: u32,
    /// How long a read may block before returning nothing.
    ///
    /// A timeout is not a failure. It is how the read loop stays responsive to shutdown
    /// while a race has gaps in it, and it must never be treated as a disconnection.
    pub read_timeout: Duration,
}

impl Default for SerialSettings {
    fn default() -> Self {
        Self {
            path: "/dev/splitforge-reader".to_owned(),
            baud: 115_200,
            read_timeout: Duration::from_millis(250),
        }
    }
}

/// A factory that opens a real serial port.
#[must_use]
pub fn serial(settings: SerialSettings) -> impl PortFactory {
    move || -> io::Result<Port> {
        let port = serialport::new(&settings.path, settings.baud)
            .timeout(settings.read_timeout)
            .open()
            .map_err(io::Error::other)?;
        Ok(Box::new(port) as Port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closure_is_a_port_factory() {
        let mut factory = || -> io::Result<Port> { Ok(Box::new(io::empty()) as Port) };
        let mut port = factory.open().expect("the fake port opens");

        let mut sink = Vec::new();
        port.read_to_end(&mut sink).expect("reading an empty port");
        assert!(sink.is_empty());
    }

    #[test]
    fn a_factory_may_refuse_to_open() {
        let mut factory = || -> io::Result<Port> { Err(io::Error::from(io::ErrorKind::NotFound)) };
        assert!(factory.open().is_err());
    }

    #[test]
    fn the_default_path_is_the_stable_one_not_the_renumbering_one() {
        // /dev/ttyUSB0 belongs to whichever bridge enumerated first. Defaulting to it would
        // work on the bench and pick the wrong device the first time two are plugged in.
        let settings = SerialSettings::default();
        assert_eq!(settings.path, "/dev/splitforge-reader");
        assert_eq!(settings.baud, 115_200);
    }
}
