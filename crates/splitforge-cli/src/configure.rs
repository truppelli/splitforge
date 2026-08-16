//! Setting an event up: events, races, checkpoints, roster, chips, readers, policy.
//!
//! Every mutation here writes an [`audit_log`](splitforge_storage::AuditEntry) row before
//! it returns. That is not bookkeeping for its own sake — `docs/timing-model.md` § 8 sets
//! the bar as "given a finished event and a disputed result, can you reconstruct from the
//! database alone why that participant received that time, and what changed" — and a config
//! change nobody recorded is exactly the gap that makes the answer no.

use anyhow::{Context, Result, anyhow, bail};
use splitforge_domain::{
    Bib, Checkpoint, CheckpointId, CheckpointPolicy, ChipAssignment, ChipId, Event, EventId,
    Participant, ParticipantId, Race, RaceId, Reader, ReaderId, SelectionRule, TimestampTrust,
    TimingPolicy,
};
use splitforge_storage::{ConfigStore, ImportSummary, RaceSelection, parse_checkpoint_kind};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Resolves an optional `--race` selector, or explains why it could not.
pub(crate) fn resolve_race(store: &ConfigStore, selector: Option<&str>) -> Result<Race> {
    match store.resolve_race(selector)? {
        RaceSelection::One(race) => Ok(*race),
        RaceSelection::None => match selector {
            Some(name) => {
                let known = store.races()?;
                if known.is_empty() {
                    bail!("no races are configured; run `splitforge race create --name ...` first")
                }
                let names: Vec<&str> = known.iter().map(|race| race.name.as_str()).collect();
                bail!(
                    "no race matches {name:?}; configured races: {}",
                    names.join(", ")
                )
            }
            None => bail!("no races are configured; run `splitforge race create --name ...` first"),
        },
        RaceSelection::Ambiguous(races) => {
            let names: Vec<&str> = races.iter().map(|race| race.name.as_str()).collect();
            bail!(
                "several races are configured; name one with --race. Candidates: {}",
                names.join(", ")
            )
        }
    }
}

/// Resolves an optional event selector the same way.
pub(crate) fn resolve_event(store: &ConfigStore, selector: Option<&str>) -> Result<Event> {
    let events = store.events()?;
    let matched: Vec<Event> = match selector {
        Some(selector) => events
            .into_iter()
            .filter(|event| {
                event.name.eq_ignore_ascii_case(selector) || event.id.to_string() == selector
            })
            .collect(),
        None => events,
    };

    match matched.len() {
        1 => matched
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("unreachable: one event that is not there")),
        0 => bail!("no event matches; run `splitforge event create --name ...` first"),
        _ => {
            let names: Vec<&str> = matched.iter().map(|event| event.name.as_str()).collect();
            bail!(
                "several events exist; name one with --event. Candidates: {}",
                names.join(", ")
            )
        }
    }
}

/// Parses an RFC 3339 instant, or returns now.
///
/// UTC only, like everything else that gets stored (ADR-0010). An operator who types a
/// local time with an offset gets it converted; one who types no offset is told, rather
/// than having a timezone guessed for them.
pub(crate) fn parse_instant(value: Option<&str>) -> Result<OffsetDateTime> {
    match value {
        None => Ok(OffsetDateTime::now_utc()),
        Some(text) => OffsetDateTime::parse(text.trim(), &Rfc3339)
            .map(|parsed| parsed.to_offset(time::UtcOffset::UTC))
            .with_context(|| {
                format!("{text:?} is not an RFC 3339 instant, such as 2026-04-11T08:00:00Z")
            }),
    }
}

/// Creates an event.
pub(crate) fn create_event(store: &mut ConfigStore, actor: &str, name: &str) -> Result<Event> {
    let event = Event {
        id: EventId::new(),
        name: name.to_owned(),
    };
    store
        .save_event(&event)
        .with_context(|| format!("creating event {name:?}"))?;
    store.record_audit(actor, "event.create", Some(name), None)?;
    Ok(event)
}

/// Creates a race within an event.
pub(crate) fn create_race(
    store: &mut ConfigStore,
    actor: &str,
    event: &Event,
    name: &str,
    start: Option<&str>,
    laps: u16,
) -> Result<Race> {
    if laps == 0 {
        bail!("a race with zero expected laps has no finish; use --laps 1 or more");
    }
    let race = Race {
        id: RaceId::new(),
        event: event.id,
        name: name.to_owned(),
        scheduled_start: start.map(|value| parse_instant(Some(value))).transpose()?,
        expected_laps: laps,
    };
    store
        .save_race(&race)
        .with_context(|| format!("creating race {name:?}"))?;
    store.record_audit(actor, "race.create", Some(name), None)?;
    Ok(race)
}

/// Adds a checkpoint to a race.
pub(crate) fn add_checkpoint(
    store: &mut ConfigStore,
    actor: &str,
    race: &Race,
    name: &str,
    kind: &str,
    sequence: Option<u16>,
) -> Result<Checkpoint> {
    let kind = parse_checkpoint_kind(kind)
        .map_err(|expected| anyhow!("--kind {kind:?} is not valid: {expected}"))?;

    let existing = store.checkpoints(race.id)?;
    if existing.iter().any(|c| c.name == name) {
        bail!(
            "race {:?} already has a checkpoint named {name:?}",
            race.name
        );
    }
    // Default to the next free slot rather than 0, so adding checkpoints in the order they
    // occur on the course does the obvious thing without any counting.
    let sequence = sequence.unwrap_or_else(|| {
        existing
            .iter()
            .map(|c| c.sequence)
            .max()
            .map_or(0, |max| max.saturating_add(1))
    });

    let checkpoint = Checkpoint {
        id: CheckpointId::new(),
        race: race.id,
        name: name.to_owned(),
        kind,
        sequence,
    };
    store.save_checkpoint(&checkpoint)?;
    store.record_audit(actor, "checkpoint.add", Some(name), None)?;
    Ok(checkpoint)
}

/// One row of a roster CSV.
#[derive(Debug, serde::Deserialize)]
struct RosterRow {
    bib: String,
    name: String,
}

/// Imports a roster from CSV.
pub(crate) fn import_roster(
    store: &mut ConfigStore,
    actor: &str,
    race: &Race,
    file: &std::path::Path,
) -> Result<ImportSummary> {
    let mut reader = csv::Reader::from_path(file)
        .with_context(|| format!("opening roster {}", file.display()))?;

    let existing = store.participants(race.id)?;
    let mut participants = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    for (index, row) in reader.deserialize::<RosterRow>().enumerate() {
        // +2: CSV rows are 1-indexed and the header is row 1, so this is the line number an
        // operator sees in their spreadsheet.
        let line = index + 2;
        let row = row.with_context(|| format!("{}: line {line}", file.display()))?;
        let bib = row.bib.trim();
        let name = row.name.trim();

        if bib.is_empty() {
            bail!("{}: line {line} has no bib", file.display());
        }
        if !seen.insert(bib.to_owned()) {
            bail!(
                "{}: bib {bib:?} appears twice; the import would silently keep only one",
                file.display()
            );
        }

        // Reuse the existing id when the bib is already entered, so re-import is an update
        // rather than a replacement. `ConfigStore` matches on `(race, bib)` regardless, but
        // passing the real id keeps the two layers telling the same story.
        let id = existing
            .iter()
            .find(|participant| participant.bib.as_str() == bib)
            .map_or_else(ParticipantId::new, |participant| participant.id);

        participants.push(Participant {
            id,
            race: race.id,
            bib: Bib::new(bib),
            name: name.to_owned(),
        });
    }

    if participants.is_empty() {
        bail!(
            "{} has no rows; expected a `bib,name` header and at least one entrant",
            file.display()
        );
    }

    let summary = store.import_participants(&participants)?;
    store.record_audit(
        actor,
        "roster.import",
        Some(&race.name),
        Some(&summary_json(file, summary)),
    )?;
    Ok(summary)
}

/// One row of a chip assignment CSV.
#[derive(Debug, serde::Deserialize)]
struct ChipRow {
    bib: String,
    chip: String,
    #[serde(default)]
    valid_from: Option<String>,
    #[serde(default)]
    valid_until: Option<String>,
}

/// Imports chip assignments from CSV.
pub(crate) fn import_chips(
    store: &mut ConfigStore,
    actor: &str,
    race: &Race,
    file: &std::path::Path,
) -> Result<ImportSummary> {
    let mut reader = csv::Reader::from_path(file)
        .with_context(|| format!("opening chip assignments {}", file.display()))?;

    let roster = store.participants(race.id)?;
    if roster.is_empty() {
        bail!(
            "race {:?} has no roster; import participants before chips",
            race.name
        );
    }

    // Chips are handed out before the gun and stay valid for the day. A window that opened
    // at the scheduled start would fail to cover a start-line read taken a second early.
    let default_from = race
        .scheduled_start
        .map_or_else(OffsetDateTime::now_utc, |start| {
            start - time::Duration::hours(24)
        });

    let mut assignments = Vec::new();
    for (index, row) in reader.deserialize::<ChipRow>().enumerate() {
        let line = index + 2;
        let row = row.with_context(|| format!("{}: line {line}", file.display()))?;
        let bib = row.bib.trim();
        let chip = row.chip.trim();

        if chip.is_empty() {
            bail!("{}: line {line} has no chip", file.display());
        }

        let participant = roster
            .iter()
            .find(|participant| participant.bib.as_str() == bib)
            .ok_or_else(|| {
                anyhow!(
                    "{}: line {line} assigns chip {chip} to bib {bib:?}, who is not on the \
                     roster for race {:?}. Import the roster first, or fix the bib — assigning \
                     a chip to nobody would produce a crossing credited to nobody.",
                    file.display(),
                    race.name
                )
            })?;

        let valid_from = match row.valid_from.as_deref().map(str::trim) {
            None | Some("") => default_from,
            Some(value) => parse_instant(Some(value))
                .with_context(|| format!("{}: line {line} valid_from", file.display()))?,
        };
        let valid_until = match row.valid_until.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(value) => Some(
                parse_instant(Some(value))
                    .with_context(|| format!("{}: line {line} valid_until", file.display()))?,
            ),
        };

        assignments.push(ChipAssignment {
            chip: ChipId::new(chip.to_uppercase()),
            participant: participant.id,
            race: race.id,
            valid_from,
            valid_until,
        });
    }

    if assignments.is_empty() {
        bail!(
            "{} has no rows; expected a `bib,chip` header and at least one assignment",
            file.display()
        );
    }

    // Reject overlap here, before writing, so the operator is told which chip is ambiguous
    // rather than discovering it at derivation time when the crossing is already gone.
    let mut merged = store.assignments(race.id)?;
    merged.extend(assignments.iter().cloned());
    splitforge_domain::ChipRegistry::from_assignments(merged).with_context(|| {
        format!(
            "{} would leave a chip assigned to two people at the same instant",
            file.display()
        )
    })?;

    let summary = store.import_assignments(&assignments)?;
    store.record_audit(
        actor,
        "chips.import",
        Some(&race.name),
        Some(&summary_json(file, summary)),
    )?;
    Ok(summary)
}

/// Declares a reader.
pub(crate) fn add_reader(
    store: &mut ConfigStore,
    actor: &str,
    id: &str,
    label: Option<&str>,
    endpoint: Option<&str>,
    trust: &str,
) -> Result<Reader> {
    let id = id.trim();
    if id.is_empty() {
        bail!("--id must not be empty: it is recorded on every read this reader produces");
    }
    let timestamp_trust = TimestampTrust::parse(trust)
        .map_err(|expected| anyhow!("--trust {trust:?} is not valid: {expected}"))?;

    let reader = Reader {
        id: ReaderId::new(id),
        label: label.unwrap_or(id).to_owned(),
        endpoint: endpoint.map(ToOwned::to_owned),
        timestamp_trust,
    };
    store.save_reader(&reader)?;
    store.record_audit(actor, "reader.add", Some(id), None)?;
    Ok(reader)
}

/// Maps a reader, or one antenna on it, to a checkpoint.
pub(crate) fn map_reader(
    store: &mut ConfigStore,
    actor: &str,
    reader_id: &str,
    antenna: Option<u16>,
    checkpoint_name: &str,
    race: &Race,
) -> Result<CheckpointId> {
    let reader = ReaderId::new(reader_id.trim());
    let known = store.readers()?;
    if !known.iter().any(|candidate| candidate.id == reader) {
        let names: Vec<&str> = known.iter().map(|r| r.id.as_str()).collect();
        bail!(
            "no reader {reader_id:?} is configured; run `splitforge reader add --id {reader_id}` \
             first. Configured readers: {}",
            if names.is_empty() {
                "none".to_owned()
            } else {
                names.join(", ")
            }
        );
    }

    let checkpoints = store.checkpoints(race.id)?;
    let checkpoint = checkpoints
        .iter()
        .find(|candidate| candidate.name == checkpoint_name)
        .ok_or_else(|| {
            let names: Vec<&str> = checkpoints.iter().map(|c| c.name.as_str()).collect();
            anyhow!(
                "race {:?} has no checkpoint named {checkpoint_name:?}. Checkpoints: {}",
                race.name,
                if names.is_empty() {
                    "none".to_owned()
                } else {
                    names.join(", ")
                }
            )
        })?;

    store.map_antenna(&reader, antenna, checkpoint.id)?;
    store.record_audit(
        actor,
        "reader.map",
        Some(reader_id),
        Some(&format!(
            r#"{{"antenna":{},"checkpoint":"{}"}}"#,
            antenna.map_or_else(|| "null".to_owned(), |a| a.to_string()),
            checkpoint.name
        )),
    )?;
    Ok(checkpoint.id)
}

/// Parses a selection rule as an operator would type it.
pub(crate) fn parse_selection_rule(value: &str) -> Result<SelectionRule> {
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        "first" => Ok(SelectionRule::First),
        "peak-rssi" | "peak_rssi" => Ok(SelectionRule::PeakRssi),
        other => {
            let floor = other
                .strip_prefix("first-above-rssi:")
                .or_else(|| other.strip_prefix("first_above_rssi:"))
                .ok_or_else(|| {
                    anyhow!(
                        "expected `first`, `peak-rssi`, or `first-above-rssi:<DBM>`, got {value:?}"
                    )
                })?;
            let floor_dbm: i16 = floor.trim().parse().with_context(|| {
                format!("{floor:?} is not a signal strength in dBm, such as -62")
            })?;
            if floor_dbm > 0 {
                bail!(
                    "an RSSI floor of {floor_dbm} dBm is above 0; readers report negative values"
                );
            }
            Ok(SelectionRule::FirstAboveRssi { floor_dbm })
        }
    }
}

/// Applies a policy change, race-wide or to one checkpoint.
pub(crate) fn set_policy(
    store: &mut ConfigStore,
    actor: &str,
    race: &Race,
    checkpoint: Option<&str>,
    min_interval_ms: Option<u64>,
    min_lap_ms: Option<u64>,
    selection_rule: Option<&str>,
) -> Result<TimingPolicy> {
    if min_interval_ms.is_none() && min_lap_ms.is_none() && selection_rule.is_none() {
        bail!(
            "nothing to set; pass at least one of --min-interval-ms, --min-lap-ms, or \
             --selection-rule"
        );
    }
    let rule = selection_rule.map(parse_selection_rule).transpose()?;
    let mut policy = store.policy(race.id)?;

    match checkpoint {
        None => {
            if let Some(ms) = min_interval_ms {
                policy.min_interval_ms = ms;
            }
            if let Some(ms) = min_lap_ms {
                policy.min_lap_ms = Some(ms);
            }
            if let Some(rule) = rule {
                policy.selection_rule = rule;
            }
        }
        Some(name) => {
            if min_lap_ms.is_some() {
                bail!(
                    "--min-lap-ms is race-wide; a per-checkpoint minimum lap has no meaning \
                     because a lap spans checkpoints"
                );
            }
            let checkpoints = store.checkpoints(race.id)?;
            let target = checkpoints
                .iter()
                .find(|candidate| candidate.name == name)
                .ok_or_else(|| anyhow!("race {:?} has no checkpoint named {name:?}", race.name))?;

            let mut existing = policy
                .per_checkpoint
                .get(&target.id)
                .copied()
                .unwrap_or(CheckpointPolicy::default());
            if let Some(ms) = min_interval_ms {
                existing.min_interval_ms = Some(ms);
            }
            if let Some(rule) = rule {
                existing.selection_rule = Some(rule);
            }
            policy = policy.with_checkpoint(target.id, existing);
        }
    }

    store.set_policy(race.id, &policy)?;
    let detail = serde_json::to_string(&policy).unwrap_or_default();
    store.record_audit(actor, "policy.set", Some(&race.name), Some(&detail))?;
    Ok(policy)
}

/// Writes a built-in fixture's configuration into the database.
pub fn load_fixture(store: &mut ConfigStore, actor: &str, slug: &str) -> Result<Race> {
    let fixture = splitforge_testkit::FixtureEvent::by_slug(slug).ok_or_else(|| {
        anyhow!(
            "unknown fixture {slug:?}; known fixtures: {}",
            splitforge_testkit::FixtureEvent::slugs().join(", ")
        )
    })?;
    let config = &fixture.config;

    store.save_event(&config.event)?;
    store.save_race(&config.race)?;
    for checkpoint in &config.checkpoints {
        store.save_checkpoint(checkpoint)?;
    }
    store.import_participants(&config.participants)?;
    store.import_assignments(&config.assignments)?;
    for reader in &config.readers {
        store.save_reader(reader)?;
    }
    for (reader, antenna, checkpoint) in config.antennas.entries() {
        store.map_antenna(reader, antenna, checkpoint)?;
    }
    store.set_policy(config.race.id, &config.policy)?;
    store.record_audit(actor, "fixture.load", Some(slug), None)?;

    Ok(config.race.clone())
}

fn summary_json(file: &std::path::Path, summary: ImportSummary) -> String {
    format!(
        r#"{{"file":"{}","inserted":{},"updated":{},"unchanged":{}}}"#,
        file.display()
            .to_string()
            .replace('\\', "/")
            .replace('"', ""),
        summary.inserted,
        summary.updated,
        summary.unchanged
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_rules_parse_the_way_an_operator_types_them() {
        assert_eq!(parse_selection_rule("first").unwrap(), SelectionRule::First);
        assert_eq!(
            parse_selection_rule("peak-rssi").unwrap(),
            SelectionRule::PeakRssi
        );
        assert_eq!(
            parse_selection_rule("first-above-rssi:-62").unwrap(),
            SelectionRule::FirstAboveRssi { floor_dbm: -62 }
        );
        assert_eq!(
            parse_selection_rule("  FIRST-ABOVE-RSSI: -55 ").unwrap(),
            SelectionRule::FirstAboveRssi { floor_dbm: -55 }
        );
    }

    #[test]
    fn a_positive_rssi_floor_is_refused_as_a_typo() {
        // Readers report negative dBm. A floor of `62` would accept nothing at all, and
        // the operator would spend the race wondering why the mat is dead.
        let error = parse_selection_rule("first-above-rssi:62").expect_err("must refuse");
        assert!(error.to_string().contains("above 0"), "got: {error}");
    }

    #[test]
    fn an_unknown_selection_rule_is_not_silently_substituted() {
        assert!(parse_selection_rule("strongest").is_err());
        assert!(parse_selection_rule("first-above-rssi:").is_err());
    }

    #[test]
    fn instants_must_carry_an_offset() {
        assert!(parse_instant(Some("2026-04-11T08:00:00Z")).is_ok());
        // No offset: guessing a timezone would move every result by hours.
        assert!(parse_instant(Some("2026-04-11 08:00:00")).is_err());
        assert!(parse_instant(Some("tomorrow")).is_err());
    }

    #[test]
    fn a_local_time_is_converted_rather_than_stored_as_written() {
        let parsed = parse_instant(Some("2026-04-11T10:00:00+02:00")).expect("parse");
        assert_eq!(parsed.offset(), time::UtcOffset::UTC);
        assert_eq!(parsed.hour(), 8);
    }
}
