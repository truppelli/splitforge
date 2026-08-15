# ADR-0007: License — GPL-3.0-or-later

- **Status:** Accepted
- **Date:** 2026-08-14

## Context

The repository was initialized with the GNU General Public License v3. Early planning
drafts raised Apache-2.0 and MPL-2.0 as alternatives, so the choice needed confirming
rather than inheriting by accident — licensing is close to irreversible once outside
contributions arrive, because relicensing then requires every contributor's agreement.

The decision matters more than usual here. SplitForge produces the record that determines
competitive outcomes. An organizer, a participant, or a governing body has a legitimate
interest in being able to inspect the software that generated a result they are being
asked to accept.

## Decision

**SplitForge is licensed GPL-3.0-or-later.** This is confirmed, not provisional.

- `LICENSE` at the repository root is the GPLv3 text
- Every crate declares `license = "GPL-3.0-or-later"`, set once in
  `[workspace.package]` and inherited
- Contributions are accepted under the same terms
- Downstream integrations (RaceDay Connect and others) communicate over documented
  export formats and network APIs, which does not make them derivative works

## Consequences

### What this makes easy

- A timing vendor cannot take SplitForge, ship a closed derivative on their own hardware,
  and leave organizers with an unauditable box
- Improvements made to distributed versions come back to the project
- The auditability promise extends from the data model to the software itself: if you can
  obtain the binary that timed your race, you can obtain its source

### What this makes hard

- **Library reuse is constrained.** `splitforge-domain` and friends cannot be linked into
  proprietary software. In Rust this bites harder than in some ecosystems, because
  everything is statically linked — anything depending on a SplitForge crate is a derived
  work
- Commercial timing companies may decline to adopt or contribute
- GPLv3's installation-information requirement applies when distributing on a "User
  Product." Anyone shipping preloaded SplitForge SD cards or turnkey Pi units must let
  the recipient install modified versions
- Corporate contributors often need legal review before touching GPL code

### What we accept

Narrower commercial adoption in exchange for a guarantee that the software behind a
result stays inspectable. For race timing specifically, that trade is the right way
round: the value of SplitForge is that its results can be trusted, and "you can read the
code that produced this" is part of that argument.

We also accept the library-reuse limitation knowingly. If a genuine case appears for a
permissively licensed subset — a shared export-format crate, say — that gets its own ADR
and a deliberate carve-out, not a quiet drift.

## Alternatives considered

| Alternative | Why not |
|---|---|
| **Apache-2.0** | Maximum adoption and an explicit patent grant, but permits a closed derivative running on a competitor's timing hardware — the outcome copyleft exists to prevent here |
| **MPL-2.0** | A reasonable middle ground: file-level copyleft, links freely with proprietary code. Weaker than GPL exactly where it matters — a vendor could keep their additions closed |
| **AGPL-3.0** | Closes the network-service gap. But SplitForge is local-first by design, so the hosted-SaaS loophole is largely theoretical, and AGPL deters contributors more sharply |
| **Dual license** | Enables commercial licensing revenue, requires a CLA, and a CLA on an unfunded volunteer project adds friction out of proportion to the benefit |

## References

- [LICENSE](../../LICENSE)
- [ADR-0005](0005-raw-read-append-only-journal.md) — the auditability argument this supports
