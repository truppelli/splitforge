//! A self-contained demo race, printed as it happens.
//!
//! This binary exists so the simulator can be watched without a database, a fixture, or
//! the CLI. It deliberately persists nothing: for the real vertical slice — journal,
//! deduplication, and JSON output — use `splitforge simulate` from `splitforge-cli`.

use splitforge_domain::{ChipId, ReaderId};
use splitforge_reader::{ReaderProvider, ReaderTimestamp};
use splitforge_simulator::{BurstProfile, Scenario, SimulatedReader, Speed};
use time::macros::datetime;
use time::{Duration, OffsetDateTime};

/// Fixed so that two runs of the demo are directly comparable.
const START: OffsetDateTime = datetime!(2026-04-11 08:00:00 UTC);
const SEED: u64 = 0x5F17_F03E;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut scenario = Scenario::new(ReaderId::new("demo-mat"), BurstProfile::default(), SEED);
    for bib in 101_u32..=104 {
        let chip = ChipId::new(format!("E2801160600002{bib:010}"));
        let index = i64::from(bib - 101);
        scenario = scenario
            .crossing(
                chip.clone(),
                Some(1),
                START + Duration::milliseconds(index * 800),
            )
            .crossing(
                chip,
                Some(2),
                START + Duration::milliseconds(1_052_400 + index * 44_100),
            );
    }

    let reader = SimulatedReader::new(&scenario, Speed::Immediate);
    println!(
        "{} crossings expanded into {} reads",
        scenario.len(),
        reader.scripted_reads()
    );

    let mut receiver = Box::new(reader).start();
    let mut count = 0_u32;
    while let Some(message) = receiver.recv().await {
        count += 1;
        let at = match message.timestamp {
            ReaderTimestamp::Utc { at } => at.to_string(),
            ReaderTimestamp::Uptime { micros } => format!("uptime+{micros}us"),
            ReaderTimestamp::Absent => "no timestamp".to_owned(),
        };
        println!(
            "{at}  antenna {:<2} rssi {:>4}  {}",
            message.antenna.unwrap_or(0),
            message.rssi_dbm.unwrap_or(0),
            message.chip
        );
    }

    println!("{count} reads emitted. Nothing was persisted; see `splitforge simulate`.");
}
