# ADR-0020: A diagnostic bundle carries no participant data, and is safe unread

- **Status:** Accepted
- **Date:** 2026-08-17
- **Deciders:** —

## Context

Milestone 5 calls for a "downloadable diagnostic bundle" — the artifact an operator produces
when something went wrong and a maintainer needs to see the device.

Everything else SplitForge writes stays where it was written. The journal, the roster, the
exports, the audit trail: their confidentiality is a question of who can physically reach
the Pi, which the threat model handles as S8 and explicitly places out of scope for
software. A bundle is different in kind. It is the only artifact this project builds
*specifically to be sent somewhere* — attached to an email, pasted into a public issue
tracker, dropped into a chat channel with whichever volunteers are still awake.

That makes it the first place participant data can leave the device by design rather than by
compromise, and the threat model had no risk row for it.

Three facts about the realistic situation shape the answer:

1. **Nobody reads a bundle before sending it.** It is produced at 22:00 by somebody who
   wants the problem to go away. A bundle that is safe only when the sender inspects it is a
   bundle that is eventually unsafe.
2. **The most useful diagnostic is the most dangerous one.** `doctor`'s `config.chips` check
   reports which entrants cannot be timed, and it reports them by bib. It is exactly what a
   maintainer wants and exactly what must not travel.
3. **Anonymized is not the same as useless.** The single most common real failure — a mat
   that works, reads that land, and nothing that scores — is diagnosed by correlating a chip
   in the journal against the assignment table. That correlation has to survive whatever
   anonymization is applied, or the bundle stops answering the question it exists for.

## Decision

**A diagnostic bundle contains no participant name, no bib number, and no raw chip
identifier. Not redacted on request, not redacted by default — never written.** The bundle
is safe to attach to a public issue without being read first, and that is stated inside the
file itself.

Four rules implement it.

**Identifiers become per-bundle salted hashes.** A chip or an operator name appears as `h:`
plus eight hex characters of SHA-256 over a salt drawn fresh for each bundle and then
discarded. Within one bundle the same value hashes to the same token, so correlation
survives; across bundles, or against a roster somebody holds, the token is worth nothing.

**Operator free text is withheld, never filtered.** Notes and reasons are prose, and
`--reason "bib 104 disqualified after review"` is this project's own documented example of
prose that names a competitor. No filter reliably finds a person in free text, so none is
attempted: the bundle records that a note exists and leaves the note on the device.

**Diagnostic messages travel only from a fail-closed allowlist.** `DETAIL_IS_SAFE_TO_SHARE`
in `crates/splitforge-cli/src/bundle.rs` names the checks whose message is known to carry
only counts, configuration, and instructions. A check absent from that list — including
every check added after the list was written — contributes its name and severity but not its
message, and says so in the file. Adding a check to `doctor` does not add it here.

**What does travel, deliberately:** event, race, and checkpoint names, and reader
identifiers. Those are course geography and equipment labelling, not people, and a bundle
that omitted them would not describe a device at all.

**Filesystem paths are substituted out, including inside allowlisted messages.** The
database's file name travels; its directories do not. A path on a Pi is
`/var/lib/splitforge/event.db` and says nothing, while the same path on a laptop runs
through a home directory and names the operator — and it arrives by a route the allowlist
does not cover, since `doctor`'s `storage.free_space` finding reports a measurement failure
by quoting the path it could not read. This is the one filter the design permits, because it
substitutes a handful of *exactly known* strings rather than guessing at prose.

**The promise is asserted against bytes, not against the serializer.**
`crates/splitforge-cli/tests/bundle.rs` runs a complete event through the binary — roster
imported, race run, results published, an entrant disqualified by bib with a reason naming
them — then searches the raw text of the resulting bundle for every name, bib, chip,
operator, sentence, and directory path the event contained. A test that walked the parsed
JSON would only cover the fields somebody remembered to check, and the leak that matters is
the one in the field nobody thought of.

## Consequences

### What this makes easy

- Attaching a bundle to a public issue without a judgement call, which is the only way the
  feature gets used at all.
- Telling a reporter "send the bundle, not the database" in `SECURITY.md`, and having that
  be honest advice rather than a hope.
- Adding fields later. The rules are about categories of data, not about a field list, so a
  new count or timestamp needs no privacy review and a new free-text field is refused by the
  rule that already exists.

### What this makes hard

- Diagnosing anything that genuinely needs an identity. "Which runner is affected" is not
  answerable from a bundle, and the answer is to ask the operator to run `doctor` on the
  device and read it there.
- Correlating two bundles from the same device. A fresh salt per bundle means a chip seen in
  both hashes differently, so "is this the same chip as last week" cannot be answered. That
  is the cost of the property that makes tokens safe at all.

### What we accept

- **The hash is not strong against someone who already holds the roster.** A bib number has
  a few thousand candidates and a salt held in memory does not change that. This is a
  boundary against the realistic reader — a maintainer in an issue tracker with no roster —
  not against an attacker who has the data already and would gain nothing.
- **A fail-closed allowlist will withhold useful messages.** Someone will add a `doctor`
  check whose message is perfectly safe, forget this file exists, and see `detail: null` in a
  bundle. That is the correct failure: the alternative failure mode is publishing a roster.
- **The allowlist is hand-maintained and can rot.** Nothing detects that a listed check's
  message has since been rewritten to include a bib. A test names `config.chips` explicitly
  so removing it is a visible failure, but the general case rests on review.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Include everything; let the operator redact | The operator is not going to read it. This is the failure mode, stated as a design |
| A `--redact` flag, off by default | The default is what gets sent. A safety property behind a flag is a safety property nobody has |
| An `--include-participants` escape hatch | Somebody would use it to "help", and the file would still look like a bundle. There is no shape of bundle that carries a roster and is also safe to paste in public |
| Regex-scrub free text for names and bibs | Names are not a pattern. A scrubber that works on "Runner 104" and fails on "Ada" is worse than withholding, because it produces confidence |
| A denylist of unsafe checks | Fails open. The next check somebody adds leaks until a reviewer catches it, and reviewers catch what they are looking for |
| Deterministic hashes, no salt | Would let bundles be correlated over time, which is useful — and would also let anyone with a roster reverse the whole table once, forever |
| A tar or zip archive of several files | A new dependency and a new format for no gain. Everything SplitForge emits is JSON, and one file is easier to attach than an archive |

## References

- [Milestone 5 — Field reliability](../roadmap.md#milestone-5--field-reliability)
- [Threat model — S11, and § 5 principle 6](../threat-model.md#4-risk-register)
- [SECURITY.md — reporting a vulnerability](../../SECURITY.md)
- `crates/splitforge-cli/src/bundle.rs`, `crates/splitforge-cli/tests/bundle.rs`
