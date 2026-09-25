//! Telling the module to start reading.
//!
//! The module never initiates (user guide § 7), so a connection that only listens hears
//! nothing. Streaming is something the host asks for, on every connection, and this is the
//! asking.
//!
//! # The sequence
//!
//! [`StartSequence::gen2`] sends, in order, waiting for each answer before the next:
//!
//! 1. **Stop streaming.** The module cannot tell a broken link from a working one and goes on
//!    streaming into it (§ 8.8.2), so a reconnection can find the last session's stream still
//!    running. Any answer is accepted, and so is none: a module that was not streaming has
//!    nothing to stop.
//! 2. **Version**, which proves an application firmware is answering at this baud rate.
//! 3. **Gen2** as the tag protocol.
//! 4. **The region**, which the operator chose. The module is a global SKU, so it is set rather
//!    than assumed.
//! 5. **The read power**, which the operator also chose (ADR-0038). The read zone, the signal
//!    strength of every read, and the current the module draws all follow from it.
//! 6. **What power the module applied**, and the range it accepts, which it reports once asked.
//!    Not required: a module that accepted the power and cannot describe it still reads.
//! 7. **Read filter off**, so the module does not suppress the repeated reads a burst is made of.
//! 8. **Start streaming.**
//!
//! Every byte comes from [`crate::command`], which cites where each command was checked.
//!
//! **No antenna-port command.** SparkFun sends `91 01 01`. MercuryAPI 2023 sends `91 01` and
//! adds the second byte only for the M6e family, which this module is not. The sources disagree
//! for this module, it has one port, and the start command reads from the configured list.
//!
//! # Answers, and everything else
//!
//! While the sequence runs, frames keep arriving: a stream left running by the last connection
//! is still pouring tag reports in. [`Starting::claims`] says which frames are the answer this
//! sequence is waiting for, by opcode and, for `0x2F`, by the option byte the answer echoes
//! (`01` for start, `02` for stop, per MercuryAPI's `serial_reader.c`). Everything else is not
//! this module's business and goes to the decoder, because a tag report that reached the host
//! is evidence whichever session it belongs to.
//!
//! Pure over its inputs: it writes to whatever it is given and takes the time as an argument,
//! so every branch is tested without a port or a clock.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use crate::command::{Command, OpCode, PowerReport, ReadPower, Region};
use crate::frame::{MAX_FRAME_LEN, Response};

/// How long each command's answer may take before the sequence gives up on it.
///
/// MercuryAPI waits on the order of a second for a command's answer. This is twice that: the
/// stop can arrive behind the tail of a stream, and a sequence that gave up too early would
/// churn connections on a module that was only busy.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(2);

/// One command, and whether its answer has to report success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Step {
    command: Command,
    must_succeed: bool,
}

/// The commands a connection sends before it streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartSequence {
    steps: Vec<Step>,
}

impl StartSequence {
    /// The Gen2 streaming start, in `region`, reading at `power`.
    #[must_use]
    pub fn gen2(region: Region, power: ReadPower) -> Self {
        let step = |command, must_succeed| Step {
            command,
            must_succeed,
        };
        Self {
            steps: vec![
                step(Command::stop_streaming(), false),
                step(Command::version(), true),
                step(Command::gen2_protocol(), true),
                step(Command::region(region), true),
                step(Command::read_power(power), true),
                step(Command::read_power_with_limits(), false),
                step(Command::read_filter_off(), true),
                step(Command::start_streaming(), true),
            ],
        }
    }

    /// The commands, in the order they are sent.
    pub fn commands(&self) -> impl Iterator<Item = &Command> {
        self.steps.iter().map(|step| &step.command)
    }
}

/// Why a connection did not start streaming.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// The module answered with a status other than success.
    #[error("the module answered {command:?} with status {status:#06x}")]
    Status {
        /// The command it refused.
        command: OpCode,
        /// The status word it answered with.
        status: u16,
    },
    /// No answer arrived within [`ANSWER_TIMEOUT`].
    #[error("the module did not answer {command:?} within {} s", ANSWER_TIMEOUT.as_secs())]
    NoAnswer {
        /// The command nothing answered.
        command: OpCode,
    },
}

/// What a step of the sequence produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// A command was sent and its answer is awaited.
    Waiting,
    /// The module accepted the start command. The stream is running.
    Started,
    /// The module refused, or did not answer, a command that had to succeed.
    Refused(Refusal),
}

/// A start sequence in progress on one connection.
#[derive(Debug)]
pub struct Starting {
    steps: Vec<Step>,
    next: usize,
    deadline: Option<Instant>,
    /// What the module said about its read power, until [`Self::take_power_report`].
    power: Option<PowerReport>,
}

impl Starting {
    /// A sequence ready to send its first command.
    #[must_use]
    pub fn new(sequence: &StartSequence) -> Self {
        Self {
            steps: sequence.steps.clone(),
            next: 0,
            deadline: None,
            power: None,
        }
    }

    /// Sends the first command.
    ///
    /// # Errors
    ///
    /// Whatever writing to the port returned. A port that cannot be written is a connection
    /// that has ended.
    pub fn begin(&mut self, port: &mut dyn Write, now: Instant) -> io::Result<Progress> {
        self.send(port, now)
    }

    /// Whether `response` is the answer this sequence is waiting for.
    #[must_use]
    pub fn claims(&self, response: &Response<'_>) -> bool {
        let Some(step) = self.awaited() else {
            return false;
        };
        let command = step.command;
        if response.opcode != command.opcode().to_byte() {
            return false;
        }
        // `0x2F` answers stop and start alike, and echoes the option byte first. Without this,
        // a stop's answer arriving late would be taken for the start's.
        if command.opcode() == OpCode::MultiProtocolTagOp {
            return response.data.first() == command.body().get(2);
        }
        true
    }

    /// Takes the answer [`Self::claims`] accepted, and sends the next command if there is one.
    ///
    /// # Errors
    ///
    /// Whatever writing the next command returned.
    pub fn answer(
        &mut self,
        response: &Response<'_>,
        port: &mut dyn Write,
        now: Instant,
    ) -> io::Result<Progress> {
        let Some(step) = self.awaited() else {
            return Ok(Progress::Waiting);
        };
        if step.must_succeed && !response.is_ok() {
            return Ok(Progress::Refused(Refusal::Status {
                command: step.command.opcode(),
                status: response.status,
            }));
        }
        if step.command == Command::read_power_with_limits() && response.is_ok() {
            self.power = PowerReport::from_answer(response.data);
        }
        self.next += 1;
        self.send(port, now)
    }

    /// What the module reported about its read power on this connection, once.
    ///
    /// `None` before it answers, after it has been taken, and when it refused or did not answer.
    pub const fn take_power_report(&mut self) -> Option<PowerReport> {
        self.power.take()
    }

    /// Checks the time. Past the deadline, a command that had to succeed is a refusal, and one
    /// that did not is passed over.
    ///
    /// # Errors
    ///
    /// Whatever writing the next command returned.
    pub fn tick(&mut self, port: &mut dyn Write, now: Instant) -> io::Result<Progress> {
        let (Some(step), Some(deadline)) = (self.awaited(), self.deadline) else {
            return Ok(self.settled());
        };
        if now < deadline {
            return Ok(Progress::Waiting);
        }
        if step.must_succeed {
            return Ok(Progress::Refused(Refusal::NoAnswer {
                command: step.command.opcode(),
            }));
        }
        self.next += 1;
        self.send(port, now)
    }

    /// Whether the command awaited now is the start itself, so the caller can re-anchor the
    /// session's timestamps at the moment it was sent.
    #[must_use]
    pub fn is_starting(&self) -> bool {
        self.awaited()
            .is_some_and(|step| step.command == Command::start_streaming())
    }

    fn awaited(&self) -> Option<&Step> {
        self.deadline.and(self.steps.get(self.next))
    }

    fn settled(&self) -> Progress {
        if self.next >= self.steps.len() {
            Progress::Started
        } else {
            Progress::Waiting
        }
    }

    fn send(&mut self, port: &mut dyn Write, now: Instant) -> io::Result<Progress> {
        let Some(step) = self.steps.get(self.next) else {
            self.deadline = None;
            return Ok(Progress::Started);
        };
        let mut out = [0_u8; MAX_FRAME_LEN];
        let frame = step
            .command
            .encode(&mut out)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        port.write_all(frame)?;
        port.flush()?;
        self.deadline = Some(now + ANSWER_TIMEOUT);
        Ok(Progress::Waiting)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{Decoded, decode};

    /// The power every sequence here is built with.
    fn power() -> ReadPower {
        "22.5".parse().expect("a power")
    }

    /// A response frame, as the module would send it.
    fn answer_frame(opcode: u8, status: u16, data: &[u8]) -> Vec<u8> {
        let mut frame = vec![
            crate::frame::SOH,
            u8::try_from(data.len()).expect("small"),
            opcode,
            (status >> 8) as u8,
            (status & 0xFF) as u8,
        ];
        frame.extend_from_slice(data);
        let crc = crate::crc::crc16(&frame[1..]);
        frame.extend_from_slice(&crc.to_be_bytes());
        frame
    }

    fn response(frame: &[u8]) -> Response<'_> {
        let Ok(Decoded::Frame { response, .. }) = decode(frame) else {
            panic!("the fixture is a whole frame");
        };
        response
    }

    /// The opcodes written so far, in order.
    fn sent(written: &[u8]) -> Vec<u8> {
        let mut opcodes = Vec::new();
        let mut rest = written;
        while !rest.is_empty() {
            let length = usize::from(rest[1]);
            opcodes.push(rest[2]);
            rest = &rest[3 + length + 2..];
        }
        opcodes
    }

    #[test]
    fn a_module_that_accepts_every_command_starts_streaming() {
        let sequence = StartSequence::gen2(Region::Na, power());
        let mut starting = Starting::new(&sequence);
        let mut port = Vec::new();
        let now = Instant::now();

        assert_eq!(
            starting.begin(&mut port, now).expect("write"),
            Progress::Waiting
        );

        let answers = [
            answer_frame(0x2F, 0x0000, &[0x02]),
            answer_frame(0x03, 0x0000, &[0x01, 0x02, 0x03]),
            answer_frame(0x93, 0x0000, &[]),
            answer_frame(0x97, 0x0000, &[]),
            answer_frame(0x92, 0x0000, &[]),
            answer_frame(0x62, 0x0000, &[0x01, 0x08, 0xCA, 0x0A, 0x8C, 0x00, 0x00]),
            answer_frame(0x9A, 0x0000, &[]),
        ];
        for frame in &answers {
            let answer = response(frame);
            assert!(starting.claims(&answer), "{frame:02X?}");
            assert_eq!(
                starting.answer(&answer, &mut port, now).expect("write"),
                Progress::Waiting
            );
        }

        assert!(starting.is_starting(), "the start is the last command out");
        let started = answer_frame(0x2F, 0x0000, &[0x01]);
        assert!(starting.claims(&response(&started)));
        assert_eq!(
            starting
                .answer(&response(&started), &mut port, now)
                .expect("write"),
            Progress::Started
        );

        assert_eq!(
            sent(&port),
            vec![0x2F, 0x03, 0x93, 0x97, 0x92, 0x62, 0x9A, 0x2F]
        );
        let mut expected = Vec::new();
        for command in sequence.commands() {
            let mut out = [0_u8; MAX_FRAME_LEN];
            expected.extend_from_slice(command.encode(&mut out).expect("encode"));
        }
        assert_eq!(
            port, expected,
            "byte for byte what the command builders produce"
        );
    }

    #[test]
    fn a_refused_command_stops_the_sequence_and_says_which_and_why() {
        let mut starting = Starting::new(&StartSequence::gen2(Region::Eu3, power()));
        let mut port = Vec::new();
        let now = Instant::now();
        starting.begin(&mut port, now).expect("write");

        for frame in [
            answer_frame(0x2F, 0x0000, &[0x02]),
            answer_frame(0x03, 0x0000, &[]),
            answer_frame(0x93, 0x0000, &[]),
        ] {
            starting
                .answer(&response(&frame), &mut port, now)
                .expect("write");
        }

        let refused = answer_frame(0x97, 0x0105, &[]);
        assert!(starting.claims(&response(&refused)));
        assert_eq!(
            starting
                .answer(&response(&refused), &mut port, now)
                .expect("write"),
            Progress::Refused(Refusal::Status {
                command: OpCode::SetRegion,
                status: 0x0105
            })
        );
        assert_eq!(
            sent(&port),
            vec![0x2F, 0x03, 0x93, 0x97],
            "nothing after the refusal"
        );
    }

    #[test]
    fn a_stop_with_nothing_to_stop_is_passed_over() {
        // Whatever the module says to a stop, and whether it says anything: a module that was
        // not streaming has nothing to stop, and the sequence goes on either way.
        let now = Instant::now();

        let mut refused_stop = Starting::new(&StartSequence::gen2(Region::Na, power()));
        let mut port = Vec::new();
        refused_stop.begin(&mut port, now).expect("write");
        let stop = answer_frame(0x2F, 0x0101, &[0x02]);
        assert_eq!(
            refused_stop
                .answer(&response(&stop), &mut port, now)
                .expect("write"),
            Progress::Waiting
        );
        assert_eq!(sent(&port), vec![0x2F, 0x03]);

        let mut silent_stop = Starting::new(&StartSequence::gen2(Region::Na, power()));
        let mut port = Vec::new();
        silent_stop.begin(&mut port, now).expect("write");
        assert_eq!(
            silent_stop.tick(&mut port, now).expect("write"),
            Progress::Waiting
        );
        assert_eq!(
            silent_stop
                .tick(&mut port, now + ANSWER_TIMEOUT)
                .expect("write"),
            Progress::Waiting,
            "the stop's deadline passes, and the version goes out"
        );
        assert_eq!(sent(&port), vec![0x2F, 0x03]);
    }

    #[test]
    fn a_command_nobody_answers_is_a_refusal_at_the_deadline() {
        let mut starting = Starting::new(&StartSequence::gen2(Region::Na, power()));
        let mut port = Vec::new();
        let now = Instant::now();
        starting.begin(&mut port, now).expect("write");
        starting
            .answer(&response(&answer_frame(0x2F, 0, &[0x02])), &mut port, now)
            .expect("write");

        assert_eq!(
            starting
                .tick(&mut port, now + ANSWER_TIMEOUT - Duration::from_millis(1))
                .expect("write"),
            Progress::Waiting
        );
        assert_eq!(
            starting
                .tick(&mut port, now + ANSWER_TIMEOUT)
                .expect("write"),
            Progress::Refused(Refusal::NoAnswer {
                command: OpCode::Version
            })
        );
    }

    #[test]
    fn a_late_stop_answer_is_not_taken_for_the_start() {
        // Both answer with 0x2F. The option byte each echoes is what tells them apart.
        let mut starting = Starting::new(&StartSequence::gen2(Region::Na, power()));
        let mut port = Vec::new();
        let now = Instant::now();
        starting.begin(&mut port, now).expect("write");
        for frame in [
            answer_frame(0x2F, 0, &[0x02]),
            answer_frame(0x03, 0, &[]),
            answer_frame(0x93, 0, &[]),
            answer_frame(0x97, 0, &[]),
            answer_frame(0x92, 0, &[]),
            answer_frame(0x62, 0, &[0x01, 0x08, 0xCA, 0x0A, 0x8C, 0x00, 0x00]),
            answer_frame(0x9A, 0, &[]),
        ] {
            starting
                .answer(&response(&frame), &mut port, now)
                .expect("write");
        }

        let late_stop = answer_frame(0x2F, 0, &[0x02]);
        assert!(
            !starting.claims(&response(&late_stop)),
            "a stop's answer must not start the stream"
        );
        assert!(starting.claims(&response(&answer_frame(0x2F, 0, &[0x01]))));
    }

    #[test]
    fn a_tag_report_is_never_an_answer() {
        // A stream from the last connection is still running while the sequence waits. Its
        // reports go to the decoder, not here.
        let mut starting = Starting::new(&StartSequence::gen2(Region::Na, power()));
        let mut port = Vec::new();
        starting.begin(&mut port, Instant::now()).expect("write");

        assert!(!starting.claims(&response(&crate::crc::CAPTURED_FRAME)));
    }

    /// A sequence that has had every answer up to and including the region's.
    fn past_the_region(port: &mut Vec<u8>, now: Instant) -> Starting {
        let mut starting = Starting::new(&StartSequence::gen2(Region::Na, power()));
        starting.begin(port, now).expect("write");
        for frame in [
            answer_frame(0x2F, 0, &[0x02]),
            answer_frame(0x03, 0, &[]),
            answer_frame(0x93, 0, &[]),
            answer_frame(0x97, 0, &[]),
        ] {
            starting
                .answer(&response(&frame), port, now)
                .expect("write");
        }
        starting
    }

    #[test]
    fn a_power_the_module_refuses_ends_the_sequence_and_says_so() {
        // ADR-0038. The range is the module's to judge: FAULT_MSG_POWER_TOO_HIGH.
        let mut port = Vec::new();
        let now = Instant::now();
        let mut starting = past_the_region(&mut port, now);

        let refused = answer_frame(0x92, 0x0103, &[]);
        assert!(starting.claims(&response(&refused)));
        assert_eq!(
            starting
                .answer(&response(&refused), &mut port, now)
                .expect("write"),
            Progress::Refused(Refusal::Status {
                command: OpCode::SetReadTxPower,
                status: 0x0103
            })
        );
        assert_eq!(sent(&port), vec![0x2F, 0x03, 0x93, 0x97, 0x92]);
    }

    #[test]
    fn a_module_that_cannot_describe_its_power_still_starts() {
        // The read-back is for the record, not a condition of reading. Refused or unanswered,
        // the sequence goes on to the read filter.
        let now = Instant::now();

        let mut port = Vec::new();
        let mut refused = past_the_region(&mut port, now);
        refused
            .answer(&response(&answer_frame(0x92, 0, &[])), &mut port, now)
            .expect("write");
        assert_eq!(
            refused
                .answer(&response(&answer_frame(0x62, 0x0109, &[])), &mut port, now)
                .expect("write"),
            Progress::Waiting
        );
        assert_eq!(sent(&port).last(), Some(&0x9A));
        assert_eq!(refused.take_power_report(), None);

        let mut port = Vec::new();
        let mut silent = past_the_region(&mut port, now);
        silent
            .answer(&response(&answer_frame(0x92, 0, &[])), &mut port, now)
            .expect("write");
        assert_eq!(
            silent.tick(&mut port, now + ANSWER_TIMEOUT).expect("write"),
            Progress::Waiting
        );
        assert_eq!(sent(&port).last(), Some(&0x9A));
    }

    #[test]
    fn what_the_module_reports_about_its_power_is_kept_once() {
        let mut port = Vec::new();
        let now = Instant::now();
        let mut starting = past_the_region(&mut port, now);
        for frame in [
            answer_frame(0x92, 0, &[]),
            answer_frame(0x62, 0, &[0x01, 0x08, 0xC0, 0x0A, 0x8C, 0x00, 0x00]),
        ] {
            starting
                .answer(&response(&frame), &mut port, now)
                .expect("write");
        }
        assert_eq!(
            starting.take_power_report(),
            Some(PowerReport {
                centi_dbm: 2240,
                max_centi_dbm: 2700,
                min_centi_dbm: 0,
            }),
            "what the module applied, which is not necessarily what was sent"
        );
        assert_eq!(starting.take_power_report(), None);
    }

    #[test]
    fn nothing_is_claimed_before_the_first_command_goes_out() {
        let starting = Starting::new(&StartSequence::gen2(Region::Na, power()));
        assert!(!starting.claims(&response(&answer_frame(0x2F, 0, &[0x02]))));
    }
}
