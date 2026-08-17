//! Scoring, publishing, and comparing result revisions.
//!
//! Everything here funnels through [`scoreboard`], which re-derives from the journal every
//! time rather than reading a cached table. Scoring is cheap and the journal is the truth;
//! a stored intermediate would be one more thing that can disagree with the evidence.
//!
//! The one command that cannot be undone is `publish`, so it is also the one with a
//! rehearsal — `preview` runs the identical scoring path and prints what would be written.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use splitforge_domain::{
    ParticipantId, RaceConfig, RawReadJournal, ResultEntry, ResultRevision, ResultRevisionId,
    ResultStatus, RevisionStatus, ScoringPolicy, StartMode, StatusDeclaration, StatusSource,
};
use splitforge_engine::{DerivationInput, derive};
use splitforge_export::format_duration_ms;
use splitforge_results::{ScoringInput, score};
use splitforge_storage::{ResultStore, RevisionSummary, SqliteJournal};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// One participant's line, shaped for output.
#[derive(Debug, Serialize)]
pub(crate) struct EntryView {
    pub(crate) place: Option<u32>,
    pub(crate) bib: String,
    pub(crate) name: String,
    pub(crate) status: &'static str,
    pub(crate) status_source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) status_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) gun_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) chip_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scoring_time: Option<String>,
    /// The same three durations in milliseconds. Formatted strings are for reading;
    /// anything that computes with a time needs the number, and deriving it back out of
    /// `0:17:32.132` is how rounding bugs get in.
    pub(crate) gun_time_ms: Option<i64>,
    pub(crate) chip_time_ms: Option<i64>,
    pub(crate) scoring_time_ms: Option<i64>,
    /// The crossing the time was measured from and to. Kept alongside the durations so a
    /// disputed result can be checked against the journal without a second command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) start_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) finish_at: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) flags: Vec<&'static str>,
}

impl EntryView {
    fn build(entry: &ResultEntry) -> Self {
        Self {
            place: entry.place,
            bib: entry.bib.as_str().to_owned(),
            name: entry.name.clone(),
            status: entry.status.as_str(),
            status_source: match entry.status_source {
                StatusSource::Derived => "derived",
                StatusSource::Declared => "declared",
            },
            status_reason: entry.status_reason.clone(),
            gun_time: entry.gun_time_ms.map(format_duration_ms),
            chip_time: entry.chip_time_ms.map(format_duration_ms),
            scoring_time: entry.scoring_time_ms.map(format_duration_ms),
            gun_time_ms: entry.gun_time_ms,
            chip_time_ms: entry.chip_time_ms,
            scoring_time_ms: entry.scoring_time_ms,
            start_at: entry.start_at.and_then(|at| at.format(&Rfc3339).ok()),
            finish_at: entry.finish_at.and_then(|at| at.format(&Rfc3339).ok()),
            flags: entry.flags.iter().map(|flag| flag.label()).collect(),
        }
    }
}

/// What scoring produced, before anybody decides to publish it.
#[derive(Debug, Serialize)]
pub(crate) struct ScoreboardView {
    pub(crate) race: String,
    pub(crate) start_mode: &'static str,
    pub(crate) gun_time: Option<String>,
    pub(crate) finished: usize,
    pub(crate) dnf: usize,
    pub(crate) dns: usize,
    pub(crate) dq: usize,
    /// Entries carrying at least one flag. The rows to look at before publishing.
    pub(crate) flagged: usize,
    pub(crate) entries: Vec<EntryView>,
}

/// A published revision, shaped for output.
#[derive(Debug, Serialize)]
pub(crate) struct RevisionView {
    pub(crate) revision: u32,
    pub(crate) status: &'static str,
    pub(crate) race: String,
    pub(crate) generated_at: String,
    pub(crate) actor: String,
    pub(crate) reason: String,
    pub(crate) start_mode: &'static str,
    pub(crate) gun_time: Option<String>,
    pub(crate) digest: String,
    pub(crate) finished: usize,
    pub(crate) dnf: usize,
    pub(crate) dns: usize,
    pub(crate) dq: usize,
    pub(crate) entries: Vec<EntryView>,
}

impl RevisionView {
    pub(crate) fn build(revision: &ResultRevision, race_name: &str) -> Self {
        let counts = revision.status_counts();
        let count_of = |status: ResultStatus| {
            counts
                .iter()
                .find(|(candidate, _)| *candidate == status)
                .map_or(0, |(_, count)| *count)
        };
        Self {
            revision: revision.number,
            status: revision.status.as_str(),
            race: race_name.to_owned(),
            generated_at: revision.generated_at.format(&Rfc3339).unwrap_or_default(),
            actor: revision.actor.clone(),
            reason: revision.reason.clone(),
            start_mode: revision.policy.start_mode.as_str(),
            gun_time: revision
                .policy
                .gun_time
                .and_then(|at| at.format(&Rfc3339).ok()),
            digest: revision.digest(),
            finished: count_of(ResultStatus::Finished),
            dnf: count_of(ResultStatus::Dnf),
            dns: count_of(ResultStatus::Dns),
            dq: count_of(ResultStatus::Dq),
            entries: revision.entries.iter().map(EntryView::build).collect(),
        }
    }
}

/// A revision as it appears in a list: header only.
#[derive(Debug, Serialize)]
pub(crate) struct RevisionSummaryView {
    pub(crate) revision: u32,
    pub(crate) status: &'static str,
    pub(crate) generated_at: String,
    pub(crate) actor: String,
    pub(crate) reason: String,
    pub(crate) entries: usize,
    pub(crate) digest: String,
}

impl RevisionSummaryView {
    fn build(summary: &RevisionSummary) -> Self {
        Self {
            revision: summary.number,
            status: summary.status.as_str(),
            generated_at: summary.generated_at.format(&Rfc3339).unwrap_or_default(),
            actor: summary.actor.clone(),
            reason: summary.reason.clone(),
            entries: summary.entries,
            digest: summary.digest.clone(),
        }
    }
}

/// One operator declaration, shaped for output.
#[derive(Debug, Serialize)]
pub(crate) struct DeclarationView {
    pub(crate) seq: u64,
    pub(crate) bib: String,
    pub(crate) name: String,
    pub(crate) status: &'static str,
    pub(crate) reason: String,
    pub(crate) actor: String,
    pub(crate) declared_at: String,
    /// Whether this is the declaration currently in force for that participant.
    pub(crate) in_force: bool,
}

/// How one participant's line differs between two revisions.
#[derive(Debug, Serialize)]
pub(crate) struct ChangeView {
    pub(crate) bib: String,
    pub(crate) name: String,
    pub(crate) change: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) status_from: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) status_to: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) place_from: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) place_to: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) time_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) time_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
}

/// The difference between two revisions.
#[derive(Debug, Serialize)]
pub(crate) struct DiffView {
    pub(crate) race: String,
    pub(crate) from: u32,
    pub(crate) to: u32,
    pub(crate) from_reason: String,
    pub(crate) to_reason: String,
    pub(crate) changed: usize,
    pub(crate) unchanged: usize,
    pub(crate) changes: Vec<ChangeView>,
}

/// The result of publishing.
#[derive(Debug, Serialize)]
pub(crate) struct PublishView {
    pub(crate) revision: u32,
    pub(crate) status: &'static str,
    pub(crate) race: String,
    pub(crate) reason: String,
    pub(crate) digest: String,
    pub(crate) entries: usize,
    pub(crate) finished: usize,
    pub(crate) dnf: usize,
    pub(crate) dns: usize,
    pub(crate) dq: usize,
    /// Whether these results differ from the previous revision.
    pub(crate) changed: bool,
}

/// Scores the race from the journal, without writing anything.
///
/// The single scoring path: `preview` and `publish` both come through here, so what a
/// rehearsal shows is by construction what a publication writes.
pub(crate) fn scoreboard(
    config: &RaceConfig,
    journal: &SqliteJournal,
    store: &ResultStore,
) -> Result<(Vec<ResultEntry>, ScoringPolicy)> {
    let reads = journal.read_all()?;
    let chips = config.chips().context("chip assignments overlap")?;
    let derivation = derive(&DerivationInput {
        reads: &reads,
        policy: &config.policy,
        chips: &chips,
        antennas: &config.antennas,
    });

    let declarations = store.declarations(config.race.id)?;
    let policy = ScoringPolicy {
        start_mode: config.start_mode,
        timing: config.policy.clone(),
        gun_time: config.gun_time(),
    };

    let entries = score(&ScoringInput {
        participants: &config.participants,
        checkpoints: &config.checkpoints,
        timing_events: &derivation.timing_events,
        declarations: &declarations,
        policy: &policy,
    })
    .context("scoring the race")?;

    Ok((entries, policy))
}

/// Builds the view for `results preview`.
pub(crate) fn preview(
    config: &RaceConfig,
    journal: &SqliteJournal,
    store: &ResultStore,
) -> Result<ScoreboardView> {
    let (entries, policy) = scoreboard(config, journal, store)?;
    let count = |status: ResultStatus| {
        entries
            .iter()
            .filter(|entry| entry.status == status)
            .count()
    };

    Ok(ScoreboardView {
        race: config.race.name.clone(),
        start_mode: policy.start_mode.as_str(),
        gun_time: policy.gun_time.and_then(|at| at.format(&Rfc3339).ok()),
        finished: count(ResultStatus::Finished),
        dnf: count(ResultStatus::Dnf),
        dns: count(ResultStatus::Dns),
        dq: count(ResultStatus::Dq),
        flagged: entries
            .iter()
            .filter(|entry| !entry.flags.is_empty())
            .count(),
        entries: entries.iter().map(EntryView::build).collect(),
    })
}

/// Scores and publishes an immutable revision.
pub(crate) fn publish(
    config: &RaceConfig,
    journal: &SqliteJournal,
    store: &mut ResultStore,
    actor: &str,
    status: RevisionStatus,
    reason: &str,
    allow_unchanged: bool,
) -> Result<PublishView> {
    if reason.trim().is_empty() {
        bail!(
            "--reason must say something: a revision nobody can explain is one nobody can defend"
        );
    }

    let (entries, policy) = scoreboard(config, journal, store)?;
    let number = store.next_revision_number(config.race.id)?;

    let revision = ResultRevision {
        id: ResultRevisionId::new(),
        race: config.race.id,
        number,
        status,
        generated_at: OffsetDateTime::now_utc(),
        actor: actor.to_owned(),
        reason: reason.to_owned(),
        policy,
        entries,
    };

    let digest = revision.digest();
    let previous = store.latest_revision(config.race.id)?;
    let changed = previous
        .as_ref()
        .is_none_or(|earlier| earlier.digest() != digest);

    if !changed && !allow_unchanged {
        let earlier = previous.as_ref().map_or(0, |revision| revision.number);
        bail!(
            "these results are identical to revision {earlier}; \
             publish anyway with --allow-unchanged"
        );
    }

    store.publish(&revision)?;

    let counts = revision.status_counts();
    let count_of = |status: ResultStatus| {
        counts
            .iter()
            .find(|(candidate, _)| *candidate == status)
            .map_or(0, |(_, count)| *count)
    };

    Ok(PublishView {
        revision: number,
        status: status.as_str(),
        race: config.race.name.clone(),
        reason: reason.to_owned(),
        digest,
        entries: revision.entries.len(),
        finished: count_of(ResultStatus::Finished),
        dnf: count_of(ResultStatus::Dnf),
        dns: count_of(ResultStatus::Dns),
        dq: count_of(ResultStatus::Dq),
        changed,
    })
}

/// Loads a revision by number, or the most recent when none is named.
pub(crate) fn load_revision(
    store: &ResultStore,
    config: &RaceConfig,
    number: Option<u32>,
) -> Result<ResultRevision> {
    let revision = match number {
        Some(number) => store
            .revision(config.race.id, number)?
            .with_context(|| format!("{} has no revision {number}", config.race.name))?,
        None => store.latest_revision(config.race.id)?.with_context(|| {
            format!(
                "{} has no published results yet — run `splitforge results publish`",
                config.race.name
            )
        })?,
    };
    Ok(revision)
}

/// Lists every revision of a race.
pub(crate) fn list(store: &ResultStore, config: &RaceConfig) -> Result<Vec<RevisionSummaryView>> {
    Ok(store
        .revisions(config.race.id)?
        .iter()
        .map(RevisionSummaryView::build)
        .collect())
}

/// Records an operator's status declaration.
pub(crate) fn declare(
    store: &mut ResultStore,
    config: &RaceConfig,
    actor: &str,
    bib: &str,
    status: ResultStatus,
    reason: &str,
) -> Result<StatusDeclaration> {
    if reason.trim().is_empty() {
        bail!("--reason must say something: a status with no stated reason cannot be defended");
    }
    let participant = config
        .participant(bib)
        .with_context(|| format!("no participant with bib {bib} in {}", config.race.name))?;

    Ok(store.declare_status(
        config.race.id,
        participant.id,
        status,
        reason,
        actor,
        OffsetDateTime::now_utc(),
    )?)
}

/// Every declaration, with the ones currently in force marked.
pub(crate) fn declarations(
    store: &ResultStore,
    config: &RaceConfig,
) -> Result<Vec<DeclarationView>> {
    let declarations = store.declarations(config.race.id)?;

    // The highest sequence per participant is the one scoring uses.
    let mut in_force: BTreeMap<ParticipantId, u64> = BTreeMap::new();
    for declaration in &declarations {
        let held = in_force.entry(declaration.participant).or_default();
        *held = (*held).max(declaration.seq);
    }

    Ok(declarations
        .iter()
        .map(|declaration| {
            let participant = config.participant_by_id(declaration.participant);
            DeclarationView {
                seq: declaration.seq,
                bib: participant
                    .map(|p| p.bib.as_str().to_owned())
                    .unwrap_or_default(),
                name: participant.map(|p| p.name.clone()).unwrap_or_default(),
                status: declaration.status.as_str(),
                reason: declaration.reason.clone(),
                actor: declaration.actor.clone(),
                declared_at: declaration.declared_at.format(&Rfc3339).unwrap_or_default(),
                in_force: in_force.get(&declaration.participant) == Some(&declaration.seq),
            }
        })
        .collect())
}

/// Compares two revisions line by line.
///
/// Answers the question a revision list raises but cannot settle: *what actually changed,
/// and for whom?*
pub(crate) fn diff(
    store: &ResultStore,
    config: &RaceConfig,
    from: u32,
    to: u32,
) -> Result<DiffView> {
    let earlier = store
        .revision(config.race.id, from)?
        .with_context(|| format!("{} has no revision {from}", config.race.name))?;
    let later = store
        .revision(config.race.id, to)?
        .with_context(|| format!("{} has no revision {to}", config.race.name))?;

    let mut changes = Vec::new();
    let mut unchanged = 0_usize;

    for entry in &later.entries {
        match earlier.entry(entry.participant) {
            Some(before) => {
                let status_changed = before.status != entry.status;
                let place_changed = before.place != entry.place;
                let time_changed = before.scoring_time_ms != entry.scoring_time_ms;
                if !status_changed && !place_changed && !time_changed {
                    unchanged += 1;
                    continue;
                }
                changes.push(ChangeView {
                    bib: entry.bib.as_str().to_owned(),
                    name: entry.name.clone(),
                    change: if status_changed {
                        "status"
                    } else {
                        "placement"
                    },
                    status_from: status_changed.then_some(before.status.as_str()),
                    status_to: status_changed.then_some(entry.status.as_str()),
                    place_from: place_changed.then_some(before.place).flatten(),
                    place_to: place_changed.then_some(entry.place).flatten(),
                    time_from: time_changed
                        .then_some(before.scoring_time_ms)
                        .flatten()
                        .map(format_duration_ms),
                    time_to: time_changed
                        .then_some(entry.scoring_time_ms)
                        .flatten()
                        .map(format_duration_ms),
                    reason: entry.status_reason.clone(),
                });
            }
            None => changes.push(ChangeView {
                bib: entry.bib.as_str().to_owned(),
                name: entry.name.clone(),
                change: "added",
                status_from: None,
                status_to: Some(entry.status.as_str()),
                place_from: None,
                place_to: entry.place,
                time_from: None,
                time_to: entry.scoring_time_ms.map(format_duration_ms),
                reason: entry.status_reason.clone(),
            }),
        }
    }

    // Anyone in the earlier revision who is gone from the later one — a roster correction
    // that removed somebody who should never have been entered.
    for entry in &earlier.entries {
        if later.entry(entry.participant).is_none() {
            changes.push(ChangeView {
                bib: entry.bib.as_str().to_owned(),
                name: entry.name.clone(),
                change: "removed",
                status_from: Some(entry.status.as_str()),
                status_to: None,
                place_from: entry.place,
                place_to: None,
                time_from: entry.scoring_time_ms.map(format_duration_ms),
                time_to: None,
                reason: None,
            });
        }
    }

    Ok(DiffView {
        race: config.race.name.clone(),
        from,
        to,
        from_reason: earlier.reason.clone(),
        to_reason: later.reason.clone(),
        changed: changes.len(),
        unchanged,
        changes,
    })
}

/// Renders a revision as CSV or JSON, per the published export contract.
pub(crate) fn export(revision: &ResultRevision, shape: crate::cli::ExportFormat) -> Result<String> {
    Ok(match shape {
        crate::cli::ExportFormat::Csv => splitforge_export::results_csv(revision)?,
        crate::cli::ExportFormat::Json => splitforge_export::results_json(revision)?,
    })
}

/// Parses an operator-supplied result status, surfacing the accepted values.
pub(crate) fn parse_status(value: &str) -> Result<ResultStatus> {
    ResultStatus::parse(value).map_err(|expected| anyhow::anyhow!("status {value:?}: {expected}"))
}

/// Parses an operator-supplied revision status.
pub(crate) fn parse_revision_status(value: &str) -> Result<RevisionStatus> {
    RevisionStatus::parse(value)
        .map_err(|expected| anyhow::anyhow!("revision status {value:?}: {expected}"))
}

/// Parses an operator-supplied start mode.
pub(crate) fn parse_start_mode(value: &str) -> Result<StartMode> {
    StartMode::parse(value).map_err(|expected| anyhow::anyhow!("start mode {value:?}: {expected}"))
}
