//! The write-ahead sidecar: evidence that outlives the database.
//!
//! A plain text file beside the event database, one line per raw read, appended and
//! fsynced **before** the SQLite transaction that stores the same read opens. That ordering
//! is the entire design: it makes the sidecar a superset of the journal at every instant, so
//! "the database is corrupt" stops being a synonym for "the reads are gone."
//!
//! ## Why a text file
//!
//! Because the failure it defends against is the one where sophisticated tools are
//! unavailable. A corrupt SQLite file needs `sqlite3`, a recovery build, and someone who
//! knows what a torn page is. A corrupt line in this file needs `tail`. The recovery path
//! has to be usable by a volunteer in a car park with a phone, or it is a recovery path in
//! name only.
//!
//! ## Line format
//!
//! ```text
//! SFJ1 <64 hex characters of sha256(json)> <json>\n
//! ```
//!
//! The version tag comes first so a future format can change everything after it. The
//! digest comes before the payload so a line can be rejected without parsing it. The JSON
//! carries every column the `raw_reads` table does, with timestamps as microseconds since
//! the Unix epoch — the same encoding the database uses, so a replayed read is byte-for-byte
//! the read that was stored, not a re-interpretation of it.
//!
//! Every field is written on every line. A recovery format should not have optional
//! syntax: a missing field must be a defect that fails loudly, never an absence that
//! quietly decodes as `None`.
//!
//! ## After a power cut
//!
//! A write the power interrupted leaves the start of a line with no newline after it. The
//! next write is appended straight onto it, so the two share one line: the remains of the
//! interrupted write, then a complete line that was written and synced as it should have
//! been. That line is found by its tag and kept, because its digest proves it; the remains in
//! front of it are counted as a torn write, not as damage. `tail` shows such a line as a
//! long one with a second `SFJ1` in it, and everything from that second tag on is the read.
//!
//! **Nothing here writes a newline to close the gap.** The reads are all in the file already,
//! and every process that opens a journal opens this file — `reads --follow` and `doctor`
//! included, while the service is appending. A reader that "repaired" a tail without a
//! newline could be looking at the service's own line half-written, and would split it.
//!
//! ## What is deliberately absent
//!
//! **The sequence number.** It is assigned by SQLite's `AUTOINCREMENT` when the row is
//! inserted, and this file is written before that happens. Writing a guess would be
//! writing a number that is wrong exactly when it matters. Sequence is a property of a
//! particular database, and a replay into a fresh one assigns it afresh.
//!
//! **Configuration.** The roster, the checkpoints, the policies — none of it is here. That
//! is the pre-race snapshot's job ([`crate::create_backup`]). The two mechanisms divide
//! the problem along the line where it actually splits: the snapshot covers what barely
//! changes, the sidecar covers what changes every second. See
//! `docs/adr/0018-write-ahead-sidecar-journal.md`.

use std::fs::{File, OpenOptions};
use std::io::{BufRead as _, BufReader, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use splitforge_domain::{ChipId, RawRead, RawReadId, ReaderId, TimestampSource};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::StorageError;
use crate::connection::{from_micros, to_micros};
use crate::journal::{
    clock_state_from_str, clock_state_to_str, fallback_from_str, fallback_to_str, hex_lower,
};

/// Format tag. Bump it, do not reinterpret it: a reader that guesses at an unfamiliar
/// version is a reader that silently mis-decodes evidence.
const LINE_TAG: &str = "SFJ1";

/// Where the sidecar for a given database lives.
///
/// Appended to the whole filename rather than replacing the extension, so `event.db`
/// becomes `event.db.reads.jsonl` and the pairing is obvious in a directory listing at a
/// glance. `.gitignore` covers the suffix — this file holds chip reads, which are event
/// data in exactly the sense the repository refuses to accept.
pub(crate) fn path_for(database: &Path) -> PathBuf {
    let mut name = database.as_os_str().to_owned();
    name.push(".reads.jsonl");
    PathBuf::from(name)
}

/// One read as it was recovered from the sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SidecarRead {
    /// The evidence itself.
    pub read: RawRead,
    /// When the writing process recorded it, preserved so a replay does not restamp it.
    pub recorded_at: OffsetDateTime,
}

/// What a pass over the sidecar found, with every verified read kept. Tests only: a pass
/// that recovers anything goes through [`Sidecar::scan_from`], which keeps none of them.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct SidecarScan {
    /// Every line that parsed and verified.
    pub records: Vec<SidecarRead>,
    /// Lines that failed the digest or the decode. Real damage, not a torn write.
    pub corrupt_lines: u64,
    /// Bytes after the last newline: a write that was interrupted before it finished.
    pub torn_tail_bytes: u64,
    /// Interrupted writes that a later write was appended onto, each found in front of the
    /// line it now shares. Counted once per line: two power cuts with nothing finished
    /// between them look like one.
    pub torn_writes: u64,
}

/// What a pass over the sidecar counted, without the reads themselves.
#[derive(Debug, Default)]
pub(crate) struct ScanTally {
    /// Lines that verified.
    pub records: u64,
    /// Lines that failed the digest or the decode. Real damage, not a torn write.
    pub corrupt_lines: u64,
    /// Bytes after the last newline: a write that was interrupted before it finished.
    pub torn_tail_bytes: u64,
    /// Interrupted writes that a later write was appended onto. See [`Line::AfterTornWrite`].
    pub torn_writes: u64,
    /// The last line that verified: where it ends, and the digest it carries. A checkpoint
    /// can be recorded there, and nowhere later in this pass.
    pub last_verified: Option<Tip>,
}

/// The end of a complete line in the sidecar, and the digest that line carries.
///
/// What a checkpoint records (ADR-0037), and what is checked against the file before one is
/// trusted: a file that no longer has this line ending at this offset is not the file the
/// checkpoint was written about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tip {
    /// The offset just past the line's newline.
    pub end: u64,
    /// The 64 hex characters after the line's tag.
    pub digest: String,
}

/// An append-only text journal beside the database.
#[derive(Debug)]
pub(crate) struct Sidecar {
    path: PathBuf,
    file: File,
    /// The last line this handle wrote, or that recovery verified. `None` until one of those
    /// has happened: a handle that has only been opened knows nothing about the file's end.
    tip: Option<Tip>,
}

impl Sidecar {
    /// Opens (creating if absent) the sidecar at `path`.
    ///
    /// **Created `0640` on Unix, whatever the umask.** Every start replays what this file
    /// holds into the append-only journal, and the per-line digest is unkeyed, so it catches
    /// damage and not a line written on purpose. Write access to this file is write access to
    /// evidence. Left to the umask, the service's `UMask=0007` made it `0660` and let anyone
    /// in the `splitforge` group append a read. The mode applies only when the file is
    /// created; an existing sidecar keeps the permissions it has.
    pub(crate) fn open(path: PathBuf) -> Result<Self, StorageError> {
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o640);
        }
        let file = options.open(&path).map_err(|error| {
            StorageError::Sidecar(format!("opening {}: {error}", path.display()))
        })?;
        Ok(Self {
            path,
            file,
            tip: None,
        })
    }

    /// Where this sidecar lives.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Appends `reads`, all stamped `recorded_at`, and fsyncs before returning.
    pub(crate) fn append(
        &mut self,
        reads: &[RawRead],
        recorded_at: OffsetDateTime,
    ) -> Result<(), StorageError> {
        self.append_records(reads.iter().map(|read| (read, recorded_at)))
    }

    /// Appends reads that already carry their own `recorded_at`, for backfill.
    pub(crate) fn append_recorded(&mut self, records: &[SidecarRead]) -> Result<(), StorageError> {
        self.append_records(
            records
                .iter()
                .map(|record| (&record.read, record.recorded_at)),
        )
    }

    fn append_records<'a>(
        &mut self,
        records: impl Iterator<Item = (&'a RawRead, OffsetDateTime)>,
    ) -> Result<(), StorageError> {
        // One buffer, one write, one fsync per batch. A reader report is a batch, so the
        // cost is per report rather than per tag — which is what makes the extra sync
        // affordable on an SD card.
        let mut buffer = String::new();
        let mut count = 0_usize;
        let mut last_digest = String::new();
        for (read, recorded_at) in records {
            let record = SidecarRecord::of(read, recorded_at)?;
            let json = serde_json::to_string(&record).map_err(|error| {
                StorageError::Sidecar(format!("encoding read {}: {error}", read.id))
            })?;
            last_digest = hex_lower(Sha256::digest(json.as_bytes()).as_slice());
            buffer.push_str(LINE_TAG);
            buffer.push(' ');
            buffer.push_str(&last_digest);
            buffer.push(' ');
            buffer.push_str(&json);
            buffer.push('\n');
            count += 1;
        }
        if count == 0 {
            return Ok(());
        }

        self.file.write_all(buffer.as_bytes()).map_err(|error| {
            StorageError::Sidecar(format!("appending to {}: {error}", self.path.display()))
        })?;
        // `sync_all`, not `sync_data`: appending changes the file length, and a length that
        // has not reached the disk is a tail that is not there after a power cut.
        self.file.sync_all().map_err(|error| {
            StorageError::Sidecar(format!("fsync {}: {error}", self.path.display()))
        })?;
        // Where this handle's own line ended, not the file's length: in append mode the write
        // lands at the end and leaves the offset after it, which is this line's end even if
        // another process appends after it.
        let end = self.file.stream_position().map_err(|error| {
            StorageError::Sidecar(format!(
                "locating the end of {}: {error}",
                self.path.display()
            ))
        })?;
        self.tip = Some(Tip {
            end,
            digest: last_digest,
        });
        Ok(())
    }

    /// The last line this handle wrote, or recovery verified.
    pub(crate) const fn tip(&self) -> Option<&Tip> {
        self.tip.as_ref()
    }

    /// Records a line recovery verified, so a checkpoint can be taken there.
    pub(crate) fn set_tip(&mut self, tip: Tip) {
        self.tip = Some(tip);
    }

    /// Reads and verifies every line, keeping every read. Tests only.
    #[cfg(test)]
    pub(crate) fn scan(&self) -> Result<SidecarScan, StorageError> {
        let mut records = Vec::new();
        let tally = self.scan_from(0, |record| {
            records.push(record);
            Ok(())
        })?;
        Ok(SidecarScan {
            records,
            corrupt_lines: tally.corrupt_lines,
            torn_tail_bytes: tally.torn_tail_bytes,
            torn_writes: tally.torn_writes,
        })
    }

    /// Reads and verifies every line from `offset` on, a line at a time, handing each read
    /// that verifies to `each` and keeping none of them.
    ///
    /// `offset` is `0` or the end of a line a checkpoint recorded. A damaged line is counted
    /// and skipped rather than fatal. The rest of the file is still evidence, and refusing to
    /// recover 40,000 good reads because one line rotted would be the wrong answer to the
    /// problem this file exists to solve.
    ///
    /// Memory is one line, whatever the file's length (ADR-0037). The whole file used to be
    /// read in one call and every read held, which on an ordinary restart was about 0.9 KB per
    /// read the event had taken.
    pub(crate) fn scan_from(
        &self,
        offset: u64,
        mut each: impl FnMut(SidecarRead) -> Result<(), StorageError>,
    ) -> Result<ScanTally, StorageError> {
        let reading = |error: std::io::Error| {
            StorageError::Sidecar(format!("reading {}: {error}", self.path.display()))
        };
        let mut file = File::open(&self.path).map_err(reading)?;
        file.seek(SeekFrom::Start(offset)).map_err(reading)?;
        let mut reader = BufReader::new(file);

        let mut tally = ScanTally::default();
        let mut position = offset;
        let mut line = Vec::new();
        loop {
            line.clear();
            let length = reader.read_until(b'\n', &mut line).map_err(reading)?;
            if length == 0 {
                break;
            }
            position += length as u64;
            // Everything after the final newline is a write that did not finish. That is an
            // expected state after a power cut, not damage, so it is reported separately.
            let Some(complete) = line.strip_suffix(b"\n") else {
                tally.torn_tail_bytes = length as u64;
                break;
            };
            if complete.is_empty() {
                continue;
            }
            let (record, at) = match read_line(complete) {
                Line::Record(record) => (record, 0),
                Line::AfterTornWrite(record, at) => {
                    tally.torn_writes += 1;
                    (record, at)
                }
                Line::AfterDamage(record, at) => {
                    tally.corrupt_lines += 1;
                    (record, at)
                }
                Line::Corrupt => {
                    tally.corrupt_lines += 1;
                    continue;
                }
            };
            tally.records += 1;
            if let Some(digest) = digest_at(complete, at) {
                tally.last_verified = Some(Tip {
                    end: position,
                    digest,
                });
            }
            each(record)?;
        }
        Ok(tally)
    }

    /// Whether the complete line ending at `tip.end` carries `tip.digest`.
    ///
    /// The check a checkpoint has to pass before recovery trusts it. `false` for a file that is
    /// shorter, one whose byte before `end` is not a newline, and one where the line there is
    /// some other line, which is what a line inserted or removed before it leaves. Only the
    /// last [`MAX_CHECKED_LINE`] bytes are read, and a longer line is not trusted.
    pub(crate) fn ends_with_line(&self, tip: &Tip) -> Result<bool, StorageError> {
        let reading = |error: std::io::Error| {
            StorageError::Sidecar(format!("reading {}: {error}", self.path.display()))
        };
        let mut file = File::open(&self.path).map_err(reading)?;
        let length = file.metadata().map_err(reading)?.len();
        if tip.end == 0 || tip.end > length {
            return Ok(false);
        }
        let start = tip.end.saturating_sub(MAX_CHECKED_LINE);
        file.seek(SeekFrom::Start(start)).map_err(reading)?;
        let mut window = Vec::new();
        file.take(tip.end - start)
            .read_to_end(&mut window)
            .map_err(reading)?;

        let Some(line) = window.strip_suffix(b"\n") else {
            return Ok(false);
        };
        let line = match line.iter().rposition(|byte| *byte == b'\n') {
            Some(newline) => &line[newline + 1..],
            None if start == 0 => line,
            None => return Ok(false),
        };
        let needle = format!("{LINE_TAG} {} ", tip.digest);
        Ok(line
            .windows(needle.len())
            .any(|candidate| candidate == needle.as_bytes()))
    }
}

/// The longest line [`Sidecar::ends_with_line`] will look back over. A line is a few hundred
/// bytes; this is room for a payload far longer than any reader sends, and a bound on what a
/// checkpoint check can cost.
const MAX_CHECKED_LINE: u64 = 64 * 1024;

/// The digest carried by the verified line that starts `at` bytes into `line`.
fn digest_at(line: &[u8], at: usize) -> Option<String> {
    let digest = line.get(at + LINE_TAG.len() + 1..at + LINE_TAG.len() + 1 + 64)?;
    std::str::from_utf8(digest).ok().map(str::to_owned)
}

/// One complete line, and what it turned out to hold.
///
/// Where a verified line was found behind something else, the offset it starts at comes with
/// it, so the digest it carries can be recorded.
enum Line {
    /// A line that verified.
    Record(SidecarRead),
    /// The remains of an interrupted write, then a line that verified at this offset.
    AfterTornWrite(SidecarRead, usize),
    /// Bytes that are not the start of any line, then a line that verified at this offset.
    AfterDamage(SidecarRead, usize),
    /// Nothing in it verified.
    Corrupt,
}

/// Reads one line, and looks behind whatever is in front of a line for the line itself.
///
/// **Before this, the first read written after a power cut was lost from this file.** The
/// interrupted write and the next one became one line, the line failed its digest, and the
/// read — written, synced, and then committed to the database — had one copy until a restart
/// backfilled it. `doctor` meanwhile reported the line as damage that had left the database
/// the only copy, after every power cut, for as long as the file existed.
///
/// Every place the tag starts after the first byte is tried in order, and the first whose
/// remainder verifies is the line. The digest is what makes this safe rather than generous:
/// a remainder that verifies is a line this writer wrote, whatever is in front of it.
fn read_line(line: &[u8]) -> Line {
    if let Ok(record) = parse_line(line) {
        return Line::Record(record);
    }
    let start = format!("{LINE_TAG} ");
    let start = start.as_bytes();
    let found = (1..line.len())
        .filter(|&at| line[at..].starts_with(start))
        .find_map(|at| parse_line(&line[at..]).ok().map(|record| (at, record)));
    match found {
        Some((at, record)) if is_torn_write(&line[..at], start) => Line::AfterTornWrite(record, at),
        Some((at, record)) => Line::AfterDamage(record, at),
        None => Line::Corrupt,
    }
}

/// Whether `fragment` is what an interrupted write leaves behind.
///
/// That is the start of a line cut off anywhere, the tag included, or bytes the filesystem
/// allocated before the power went and never filled, which read back as zeros. Anything else
/// in front of a good line is damage and is counted as such: the line behind it is still
/// kept, but calling rot a power cut would turn an error into a warning.
fn is_torn_write(fragment: &[u8], start: &[u8]) -> bool {
    let unfilled = fragment.iter().take_while(|byte| **byte == 0).count();
    let fragment = &fragment[unfilled..];
    fragment.is_empty() || fragment.starts_with(start) || start.starts_with(fragment)
}

fn parse_line(line: &[u8]) -> Result<SidecarRead, StorageError> {
    let text = std::str::from_utf8(line)
        .map_err(|error| StorageError::Sidecar(format!("line is not utf-8: {error}")))?;
    let rest = text
        .strip_prefix(LINE_TAG)
        .and_then(|rest| rest.strip_prefix(' '))
        .ok_or_else(|| StorageError::Sidecar(format!("line is not tagged {LINE_TAG}")))?;
    let (digest, json) = rest
        .split_once(' ')
        .ok_or_else(|| StorageError::Sidecar("line has no digest separator".to_owned()))?;

    let actual = hex_lower(Sha256::digest(json.as_bytes()).as_slice());
    if actual != digest {
        return Err(StorageError::Sidecar(format!(
            "digest mismatch: line claims {digest}, content hashes to {actual}"
        )));
    }

    let record: SidecarRecord = serde_json::from_str(json)
        .map_err(|error| StorageError::Sidecar(format!("decoding line: {error}")))?;
    record.into_read()
}

/// The on-disk shape of one line.
///
/// Deliberately its own type rather than a `Serialize` on [`RawRead`]. This is a recovery
/// format: its field names are a contract with every future build that has to read a file
/// written today, and they must not change because somebody renamed a struct member. The
/// golden-line test below is what enforces that.
#[derive(Debug, Serialize, Deserialize)]
struct SidecarRecord {
    id: String,
    source: String,
    antenna: Option<u16>,
    chip: String,
    reader_timestamp_us: Option<i64>,
    reader_uptime_us: Option<u64>,
    received_at_us: i64,
    received_at_monotonic_ns: Option<u64>,
    rssi_dbm: Option<i16>,
    device_clock_state: String,
    timestamp_source: String,
    timestamp_fallback_reason: Option<String>,
    clock_offset_ms: Option<i64>,
    raw_payload_hex: String,
    recorded_at_us: i64,
}

impl SidecarRecord {
    fn of(read: &RawRead, recorded_at: OffsetDateTime) -> Result<Self, StorageError> {
        let (source_text, fallback_text) = match read.timestamp_source {
            TimestampSource::ReaderUtc => ("reader_utc", None),
            TimestampSource::DeviceReceipt { reason } => {
                ("device_receipt", Some(fallback_to_str(reason).to_owned()))
            }
        };
        Ok(Self {
            id: read.id.to_string(),
            source: read.source.as_str().to_owned(),
            antenna: read.antenna,
            chip: read.chip.as_str().to_owned(),
            reader_timestamp_us: read.reader_timestamp.map(to_micros).transpose()?,
            reader_uptime_us: read.reader_uptime_us,
            received_at_us: to_micros(read.received_at)?,
            received_at_monotonic_ns: read.received_at_monotonic_ns,
            rssi_dbm: read.rssi_dbm,
            device_clock_state: clock_state_to_str(read.device_clock_state).to_owned(),
            timestamp_source: source_text.to_owned(),
            timestamp_fallback_reason: fallback_text,
            clock_offset_ms: read.clock_offset_ms,
            raw_payload_hex: hex_lower(&read.raw_payload),
            recorded_at_us: to_micros(recorded_at)?,
        })
    }

    fn into_read(self) -> Result<SidecarRead, StorageError> {
        let uuid = Uuid::parse_str(&self.id).map_err(|error| {
            StorageError::Sidecar(format!("raw read id {:?}: {error}", self.id))
        })?;

        let timestamp_source = match self.timestamp_source.as_str() {
            "reader_utc" => TimestampSource::ReaderUtc,
            "device_receipt" => {
                let reason = self.timestamp_fallback_reason.ok_or_else(|| {
                    StorageError::Sidecar("device_receipt line has no fallback reason".to_owned())
                })?;
                TimestampSource::DeviceReceipt {
                    reason: fallback_from_str(&reason)?,
                }
            }
            other => {
                return Err(StorageError::Sidecar(format!(
                    "unknown timestamp_source {other:?}"
                )));
            }
        };

        let recovered = SidecarRead {
            recorded_at: from_micros(self.recorded_at_us)?,
            read: RawRead {
                id: RawReadId::from_uuid(uuid),
                source: ReaderId::new(self.source),
                antenna: self.antenna,
                chip: ChipId::new(self.chip),
                reader_timestamp: self.reader_timestamp_us.map(from_micros).transpose()?,
                reader_uptime_us: self.reader_uptime_us,
                received_at: from_micros(self.received_at_us)?,
                received_at_monotonic_ns: self.received_at_monotonic_ns,
                rssi_dbm: self.rssi_dbm,
                device_clock_state: clock_state_from_str(&self.device_clock_state)?,
                timestamp_source,
                clock_offset_ms: self.clock_offset_ms,
                raw_payload: hex_decode(&self.raw_payload_hex)?,
            },
        };

        // A line the database cannot hold is damage like any other, counted and skipped. A
        // recovery that stopped on it would stop recovering every read around it, on every
        // start. Nothing this build writes can fail here, because the same check runs before
        // an append, so a line that does came from an older build or from a hand.
        crate::journal::storable(&recovered.read)?;
        Ok(recovered)
    }
}

fn hex_decode(text: &str) -> Result<Vec<u8>, StorageError> {
    if !text.len().is_multiple_of(2) {
        return Err(StorageError::Sidecar(
            "payload hex has an odd number of characters".to_owned(),
        ));
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(text.len() / 2);
    for pair in bytes.as_chunks::<2>().0 {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        out.push(high << 4 | low);
    }
    Ok(out)
}

fn hex_value(byte: u8) -> Result<u8, StorageError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        other => Err(StorageError::Sidecar(format!(
            "payload hex contains {:?}",
            char::from(other)
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use splitforge_domain::{DeviceClockState, FallbackReason};
    use tempfile::tempdir;
    use time::Duration;

    fn sample_read(chip: &str) -> RawRead {
        let received = OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_700_000_000);
        RawRead {
            id: RawReadId::new(),
            source: ReaderId::new("mat"),
            antenna: Some(2),
            chip: ChipId::new(chip),
            reader_timestamp: Some(received - Duration::milliseconds(35)),
            reader_uptime_us: None,
            received_at: received,
            received_at_monotonic_ns: Some(9_876_543_210),
            rssi_dbm: Some(-58),
            device_clock_state: DeviceClockState::GpsLocked,
            timestamp_source: TimestampSource::ReaderUtc,
            clock_offset_ms: Some(35),
            raw_payload: vec![0xDE, 0xAD, 0xBE, 0xEF],
        }
    }

    fn open_temp() -> (tempfile::TempDir, Sidecar) {
        let dir = tempdir().expect("tempdir");
        let sidecar = Sidecar::open(path_for(&dir.path().join("event.db"))).expect("open");
        (dir, sidecar)
    }

    #[test]
    fn a_read_survives_a_round_trip_through_the_file_with_every_field_intact() {
        let (_dir, mut sidecar) = open_temp();
        let read = sample_read("E280A");
        let recorded_at = OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_700_000_001);
        sidecar
            .append(std::slice::from_ref(&read), recorded_at)
            .expect("append");

        let scan = sidecar.scan().expect("scan");
        assert_eq!(scan.records.len(), 1);
        assert_eq!(scan.corrupt_lines, 0);
        assert_eq!(scan.torn_tail_bytes, 0);
        assert_eq!(
            scan.records[0].read, read,
            "the sidecar is evidence; it must round-trip byte for byte"
        );
        assert_eq!(scan.records[0].recorded_at, recorded_at);
    }

    #[test]
    fn a_read_with_no_reader_timestamp_round_trips_its_fallback_reason() {
        let (_dir, mut sidecar) = open_temp();
        let mut read = sample_read("E280B");
        read.reader_timestamp = None;
        read.clock_offset_ms = None;
        read.reader_uptime_us = Some(42_000_000);
        read.timestamp_source = TimestampSource::DeviceReceipt {
            reason: FallbackReason::ReaderUptimeOnly,
        };
        read.device_clock_state = DeviceClockState::Unsynced;

        sidecar
            .append(std::slice::from_ref(&read), OffsetDateTime::UNIX_EPOCH)
            .expect("append");
        let scan = sidecar.scan().expect("scan");
        assert_eq!(scan.records[0].read, read);
    }

    #[test]
    fn an_empty_payload_round_trips_rather_than_becoming_a_decode_error() {
        let (_dir, mut sidecar) = open_temp();
        let mut read = sample_read("E280C");
        read.raw_payload = Vec::new();
        read.antenna = None;
        read.rssi_dbm = None;
        read.received_at_monotonic_ns = None;

        sidecar
            .append(std::slice::from_ref(&read), OffsetDateTime::UNIX_EPOCH)
            .expect("append");
        assert_eq!(sidecar.scan().expect("scan").records[0].read, read);
    }

    #[test]
    fn a_torn_final_line_is_reported_and_the_reads_before_it_survive() {
        // The power-cut case: the last write did not finish. Everything before it did.
        let dir = tempdir().expect("tempdir");
        let path = path_for(&dir.path().join("event.db"));
        {
            let mut sidecar = Sidecar::open(path.clone()).expect("open");
            sidecar
                .append(
                    &[sample_read("A"), sample_read("B")],
                    OffsetDateTime::UNIX_EPOCH,
                )
                .expect("append");
        }
        // Simulate an interrupted append: a partial line with no newline.
        {
            let mut file = OpenOptions::new().append(true).open(&path).expect("reopen");
            file.write_all(b"SFJ1 deadbeef {\"id\":\"half-writ")
                .expect("partial");
        }

        let sidecar = Sidecar::open(path).expect("open");
        let scan = sidecar.scan().expect("scan");
        assert_eq!(scan.records.len(), 2, "the completed reads are still there");
        assert_eq!(scan.corrupt_lines, 0, "a torn tail is not corruption");
        assert_eq!(scan.torn_tail_bytes, 30);
    }

    /// Appends `bytes` with no newline, as a write the power interrupted leaves them.
    fn interrupt_a_write(path: &Path, bytes: &[u8]) {
        let mut file = OpenOptions::new().append(true).open(path).expect("reopen");
        file.write_all(bytes)
            .expect("the part that reached the disk");
    }

    /// A sidecar holding `A`, then `fragment` with no newline, then `B` appended after it.
    fn a_power_cut_between(fragment: &[u8], after: &RawRead) -> SidecarScan {
        let dir = tempdir().expect("tempdir");
        let path = path_for(&dir.path().join("event.db"));
        Sidecar::open(path.clone())
            .expect("open")
            .append(&[sample_read("A")], OffsetDateTime::UNIX_EPOCH)
            .expect("append");
        interrupt_a_write(&path, fragment);
        Sidecar::open(path.clone())
            .expect("the next start")
            .append(std::slice::from_ref(after), OffsetDateTime::UNIX_EPOCH)
            .expect("the first read after the cut");
        Sidecar::open(path).expect("open").scan().expect("scan")
    }

    #[test]
    fn a_read_appended_onto_an_interrupted_write_is_recovered_from_behind_it() {
        // The 2026-09-13 review's reproduction, at the file: the next append was glued onto
        // the half-line, and the pair failed as one corrupt line.
        let after = sample_read("B");
        let scan = a_power_cut_between(b"SFJ1 deadbeef {\"id\":\"half-writ", &after);

        assert_eq!(scan.records.len(), 2, "A, and the read after the cut");
        assert_eq!(scan.records[1].read, after, "recovered byte for byte");
        assert_eq!(scan.torn_writes, 1, "the cut is still reported, as a cut");
        assert_eq!(scan.corrupt_lines, 0, "a power cut is not damage");
        assert_eq!(scan.torn_tail_bytes, 0);
    }

    #[test]
    fn a_write_cut_off_inside_the_tag_is_still_a_torn_write() {
        let scan = a_power_cut_between(b"SF", &sample_read("B"));
        assert_eq!((scan.records.len(), scan.torn_writes), (2, 1));
        assert_eq!(scan.corrupt_lines, 0);
    }

    #[test]
    fn bytes_the_filesystem_never_filled_are_a_torn_write() {
        // A file extended before the power went and never written reads back as zeros. The
        // two can come together: the unfilled bytes, then the start of the line.
        for fragment in [&[0_u8; 96][..], b"\0\0\0\0SFJ1 12ab {\"id\":"] {
            let scan = a_power_cut_between(fragment, &sample_read("B"));
            assert_eq!(
                (scan.records.len(), scan.torn_writes),
                (2, 1),
                "{fragment:?}"
            );
            assert_eq!(scan.corrupt_lines, 0, "{fragment:?}");
        }
    }

    #[test]
    fn two_power_cuts_with_nothing_finished_between_them_look_like_one() {
        let scan = a_power_cut_between(
            b"SFJ1 aaaa {\"id\":\"first-halfSFJ1 bbbb {\"id\":\"second-ha",
            &sample_read("B"),
        );
        assert_eq!((scan.records.len(), scan.torn_writes), (2, 1));
        assert_eq!(scan.corrupt_lines, 0);
    }

    #[test]
    fn damage_in_front_of_a_good_line_is_still_damage_and_the_line_is_still_kept() {
        // Something other than a line's start glued onto the next one. The line behind it
        // verifies, so it is evidence and is kept; calling the rest a power cut would turn
        // an error into a warning.
        let after = sample_read("B");
        let scan = a_power_cut_between(b"\xff\x13 not how any line starts", &after);
        assert_eq!(scan.records.len(), 2);
        assert_eq!(scan.records[1].read, after);
        assert_eq!(scan.corrupt_lines, 1);
        assert_eq!(scan.torn_writes, 0);
    }

    #[test]
    fn a_tag_inside_a_read_is_not_mistaken_for_the_start_of_one() {
        // Every candidate is tried in order and only a verified one is kept, so a chip id
        // that happens to contain the tag changes nothing.
        let after = sample_read("SFJ1 INSIDE");
        let scan = a_power_cut_between(b"SFJ1 00 {\"id\":\"cut", &after);
        assert_eq!(scan.records[1].read, after);
        assert_eq!((scan.torn_writes, scan.corrupt_lines), (1, 0));

        let (_dir, mut intact) = open_temp();
        intact
            .append(std::slice::from_ref(&after), OffsetDateTime::UNIX_EPOCH)
            .expect("append");
        let scan = intact.scan().expect("scan");
        assert_eq!(scan.records[0].read, after);
        assert_eq!((scan.torn_writes, scan.corrupt_lines), (0, 0));
    }

    #[test]
    fn a_line_whose_content_was_altered_fails_its_digest_and_is_counted() {
        let dir = tempdir().expect("tempdir");
        let path = path_for(&dir.path().join("event.db"));
        {
            let mut sidecar = Sidecar::open(path.clone()).expect("open");
            sidecar
                .append(
                    &[sample_read("KEEP"), sample_read("TAMPER")],
                    OffsetDateTime::UNIX_EPOCH,
                )
                .expect("append");
        }

        let text = std::fs::read_to_string(&path).expect("read");
        // Change a chip id without recomputing the digest, which is what bit rot looks
        // like and also what a careless edit looks like.
        std::fs::write(&path, text.replace("TAMPER", "TAMPEQ")).expect("write");

        let scan = Sidecar::open(path).expect("open").scan().expect("scan");
        assert_eq!(scan.records.len(), 1, "the untouched line still verifies");
        assert_eq!(scan.records[0].read.chip.as_str(), "KEEP");
        assert_eq!(
            scan.corrupt_lines, 1,
            "an altered line must be rejected, not accepted with the wrong chip"
        );
    }

    #[test]
    fn appending_nothing_writes_nothing() {
        let (_dir, mut sidecar) = open_temp();
        sidecar
            .append(&[], OffsetDateTime::UNIX_EPOCH)
            .expect("append");
        assert_eq!(
            std::fs::metadata(sidecar.path()).expect("stat").len(),
            0,
            "an empty batch must not produce a line"
        );
    }

    #[test]
    fn the_line_format_is_pinned_so_a_field_rename_cannot_silently_break_recovery() {
        // A golden line, written by hand. If a future change to `RawRead` alters what this
        // file contains, this test fails — which is the entire point. A sidecar that a
        // later build cannot read is not a recovery mechanism.
        let json = concat!(
            r#"{"id":"11111111-1111-4111-8111-111111111111","source":"mat","antenna":2,"#,
            r#""chip":"E280A","reader_timestamp_us":1699999999965000,"reader_uptime_us":null,"#,
            r#""received_at_us":1700000000000000,"received_at_monotonic_ns":9876543210,"#,
            r#""rssi_dbm":-58,"device_clock_state":"gps_locked","timestamp_source":"reader_utc","#,
            r#""timestamp_fallback_reason":null,"clock_offset_ms":35,"#,
            r#""raw_payload_hex":"deadbeef","recorded_at_us":1700000001000000}"#
        );
        let digest = hex_lower(Sha256::digest(json.as_bytes()).as_slice());
        let line = format!("{LINE_TAG} {digest} {json}");

        let record = parse_line(line.as_bytes()).expect("the pinned format must still parse");
        assert_eq!(record.read.chip.as_str(), "E280A");
        assert_eq!(record.read.raw_payload, vec![0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(record.read.antenna, Some(2));
        assert_eq!(record.read.rssi_dbm, Some(-58));
        assert_eq!(record.read.timestamp_source, TimestampSource::ReaderUtc);
        assert_eq!(record.read.device_clock_state, DeviceClockState::GpsLocked);

        // And the encoder still produces exactly it.
        let round_tripped = SidecarRecord::of(&record.read, record.recorded_at).expect("encode");
        assert_eq!(
            serde_json::to_string(&round_tripped).expect("encode"),
            json,
            "the written form must match the pinned form field for field and order for order"
        );
    }

    #[test]
    fn an_untagged_line_is_rejected_rather_than_guessed_at() {
        let error = parse_line(br#"{"id":"whatever"}"#).expect_err("must refuse");
        assert!(error.to_string().contains("not tagged"), "got: {error}");
    }

    #[test]
    fn hex_decoding_refuses_malformed_input() {
        assert!(hex_decode("abc").is_err(), "odd length");
        assert!(hex_decode("zz").is_err(), "not hex");
        assert_eq!(hex_decode("").expect("empty"), Vec::<u8>::new());
    }

    #[test]
    fn the_sidecar_path_sits_beside_the_database_it_belongs_to() {
        assert_eq!(
            path_for(Path::new("/var/lib/splitforge/event.db")),
            PathBuf::from("/var/lib/splitforge/event.db.reads.jsonl")
        );
    }
}
