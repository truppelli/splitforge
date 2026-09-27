//! What `doctor` and the diagnostic bundle need to know about the whole journal, in one pass.
//!
//! Both used to call `read_all` and hold every read at once to answer questions whose answers
//! are a handful of numbers: where the sequence starts and whether it has gaps, which readers
//! and antennas reads came from, which chips were seen, and how many reads carried an
//! untrusted clock. On a million reads that was 430 MB for `doctor` and 537 MB for the bundle,
//! on a Pi 4 with 2 GB (security review 2026-09-13).
//!
//! [`JournalSummary::of`] streams the journal a row at a time. What it keeps grows with the
//! number of distinct readers, antennas and chips, which is bounded by the equipment and the
//! field, not with the number of reads, which grows all day.

use std::collections::{BTreeMap, BTreeSet};

use splitforge_domain::{ReaderId, StoredRawRead};
use splitforge_storage::{SqliteJournal, StorageError};
use time::OffsetDateTime;

/// How many sequence gaps are named individually. The rest are counted.
const GAPS_NAMED: usize = 5;

/// One reader and antenna that reads came from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Source {
    pub(crate) reader: ReaderId,
    pub(crate) antenna: Option<u16>,
}

/// The journal, counted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct JournalSummary {
    /// Reads in the journal.
    pub(crate) reads: usize,
    /// The first read's sequence number.
    pub(crate) first_seq: Option<u64>,
    /// The last read's sequence number.
    pub(crate) last_seq: Option<u64>,
    /// How many times the sequence skips a number.
    pub(crate) gap_count: usize,
    /// The sequence number before each of the first [`GAPS_NAMED`] gaps.
    pub(crate) first_gaps_after: Vec<u64>,
    /// The earliest authoritative timestamp.
    pub(crate) first_read_at: Option<OffsetDateTime>,
    /// The latest authoritative timestamp.
    pub(crate) last_read_at: Option<OffsetDateTime>,
    /// Reads taken while the device clock had no trustworthy source.
    pub(crate) untrusted_clock_reads: usize,
    /// Reads with no reader timestamp, timed by the device instead.
    pub(crate) device_timed_reads: usize,
    /// The clock offset largest in magnitude, sign kept.
    pub(crate) max_clock_offset_ms: Option<i64>,
    /// Every reader and antenna reads came from, in the order each was first seen.
    pub(crate) sources: Vec<Source>,
    /// How many reads came from each of them.
    pub(crate) reads_by_source: BTreeMap<Source, usize>,
    /// Every chip that was read.
    pub(crate) chips: BTreeSet<String>,
}

impl JournalSummary {
    /// Counts `journal`, holding one read at a time.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the journal cannot be read.
    pub(crate) fn of(journal: &SqliteJournal) -> Result<Self, StorageError> {
        let mut summary = Self::default();
        journal.for_each_read(|stored| summary.add(&stored))?;
        Ok(summary)
    }

    fn add(&mut self, stored: &StoredRawRead) {
        let read = &stored.read;
        self.reads += 1;

        if let Some(last) = self.last_seq
            && stored.seq != last + 1
        {
            self.gap_count += 1;
            if self.first_gaps_after.len() < GAPS_NAMED {
                self.first_gaps_after.push(last);
            }
        }
        self.first_seq.get_or_insert(stored.seq);
        self.last_seq = Some(stored.seq);

        let at = read.authoritative_timestamp();
        self.first_read_at = Some(self.first_read_at.map_or(at, |first| first.min(at)));
        self.last_read_at = Some(self.last_read_at.map_or(at, |last| last.max(at)));

        if !read.device_clock_state.is_trustworthy() {
            self.untrusted_clock_reads += 1;
        }
        if read.reader_timestamp.is_none() {
            self.device_timed_reads += 1;
        }
        if let Some(offset) = read.clock_offset_ms {
            self.max_clock_offset_ms = Some(self.max_clock_offset_ms.map_or(offset, |largest| {
                if offset.abs() > largest.abs() {
                    offset
                } else {
                    largest
                }
            }));
        }

        let source = Source {
            reader: read.source.clone(),
            antenna: read.antenna,
        };
        match self.reads_by_source.get_mut(&source) {
            Some(count) => *count += 1,
            None => {
                self.sources.push(source.clone());
                self.reads_by_source.insert(source, 1);
            }
        }

        if !self.chips.contains(read.chip.as_str()) {
            self.chips.insert(read.chip.as_str().to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use splitforge_domain::{
        ChipId, DeviceClockState, FallbackReason, RawRead, RawReadId, TimestampSource,
    };
    use time::Duration;

    /// A read with sequence `seq`, from `reader` on `antenna`, of `chip`, received `second`s in.
    fn stored(
        seq: u64,
        reader: &str,
        antenna: Option<u16>,
        chip: &str,
        second: i64,
    ) -> StoredRawRead {
        let at = OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_775_000_000 + second);
        StoredRawRead {
            seq,
            recorded_at: at,
            payload_sha256: String::new(),
            read: RawRead {
                id: RawReadId::new(),
                source: ReaderId::new(reader),
                antenna,
                chip: ChipId::new(chip),
                reader_timestamp: Some(at),
                reader_uptime_us: None,
                received_at: at,
                received_at_monotonic_ns: None,
                rssi_dbm: None,
                device_clock_state: DeviceClockState::NtpSynced,
                timestamp_source: TimestampSource::ReaderUtc,
                clock_offset_ms: None,
                raw_payload: Vec::new(),
            },
        }
    }

    fn summarize(reads: &[StoredRawRead]) -> JournalSummary {
        let mut summary = JournalSummary::default();
        for read in reads {
            summary.add(read);
        }
        summary
    }

    #[test]
    fn an_empty_journal_is_all_zeroes_and_nones() {
        assert_eq!(summarize(&[]), JournalSummary::default());
    }

    #[test]
    fn sequence_gaps_are_counted_and_the_first_five_named_by_the_sequence_before_them() {
        // Gaps after 3, 5, 7, 9, 11 and 13: six, of which five are named.
        let seqs = [2, 3, 5, 7, 9, 11, 13, 20];
        let reads: Vec<StoredRawRead> = seqs
            .iter()
            .map(|seq| stored(*seq, "mat", Some(1), "A", 0))
            .collect();
        let summary = summarize(&reads);

        assert_eq!(summary.reads, 8);
        assert_eq!(
            summary.first_seq,
            Some(2),
            "a journal that does not start at 1"
        );
        assert_eq!(summary.last_seq, Some(20));
        assert_eq!(summary.gap_count, 6);
        assert_eq!(summary.first_gaps_after, [3, 5, 7, 9, 11]);
    }

    #[test]
    fn sources_keep_the_order_the_journal_met_them_and_count_each() {
        let reads = [
            stored(1, "mat", Some(2), "A", 0),
            stored(2, "lap-mat", None, "B", 1),
            stored(3, "mat", Some(1), "A", 2),
            stored(4, "mat", Some(2), "C", 3),
        ];
        let summary = summarize(&reads);

        let order: Vec<(&str, Option<u16>)> = summary
            .sources
            .iter()
            .map(|source| (source.reader.as_str(), source.antenna))
            .collect();
        assert_eq!(
            order,
            [("mat", Some(2)), ("lap-mat", None), ("mat", Some(1))]
        );
        assert_eq!(summary.reads_by_source.values().sum::<usize>(), 4);
        assert_eq!(
            summary.reads_by_source[&Source {
                reader: ReaderId::new("mat"),
                antenna: Some(2)
            }],
            2
        );
        assert_eq!(
            summary.chips.iter().map(String::as_str).collect::<Vec<_>>(),
            ["A", "B", "C"]
        );
    }

    #[test]
    fn times_clocks_and_offsets_are_taken_across_every_read() {
        let mut late = stored(1, "mat", Some(1), "A", 30);
        late.read.device_clock_state = DeviceClockState::Unsynced;
        late.read.clock_offset_ms = Some(40);
        let mut early = stored(2, "mat", Some(1), "A", 10);
        early.read.reader_timestamp = None;
        early.read.timestamp_source = TimestampSource::DeviceReceipt {
            reason: FallbackReason::NoReaderTimestamp,
        };
        early.read.clock_offset_ms = Some(-75);
        let middle = stored(3, "mat", Some(1), "A", 20);
        let summary = summarize(&[late.clone(), early.clone(), middle]);

        assert_eq!(
            summary.first_read_at,
            Some(early.read.authoritative_timestamp())
        );
        assert_eq!(
            summary.last_read_at,
            Some(late.read.authoritative_timestamp())
        );
        assert_eq!(summary.untrusted_clock_reads, 1);
        assert_eq!(summary.device_timed_reads, 1);
        assert_eq!(
            summary.max_clock_offset_ms,
            Some(-75),
            "the largest in magnitude, with its sign"
        );
    }
}
