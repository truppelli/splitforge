# ADR-0025: M3a proves durability above the transport; the adapter streams rather than polls

- **Status:** Accepted
- **Date:** 2026-08-31

## Context

[M3a's exit criterion](../roadmap.md#milestone-3a--one-serial-reader) has read, since
[ADR-0024](0024-serial-reader-adapter-before-llrp.md) split it out of M3:

> a serial module runs for several hours while every read is preserved through deliberately
> induced disconnections and service restarts, and the count of reads the module believes it
> sent matches the count in the journal.

[vendor-documents § 7](../readers/vendor-documents.md#7-m3as-exit-criterion-may-not-be-reachable-on-this-interface)
found — from documents already archived, before anything was ordered — that the second clause
may not be reachable on this interface, listed three ways out, and deliberately chose none of
them: *"Choosing between them is a separate review, not an edit here."* This is that review.

The finding stands. § 8.8.2 says the module cannot *"detect a broken communications interface
connection and stop streaming the tag results."* § 5.1.4.1 says *"Flow control is not
supported."* During an induced disconnection the module goes on streaming into a cable that is
not there, the buffer is explicitly circular, and neither end has a control line to notice.
Reads emitted in that window are gone — not delayed and not buffered — so *"the count of reads
the module believes it sent"* cannot be reconciled against the journal, and the first clause
fails over the same window.

### What the criterion actually bundles

The clause is unreachable because it names two different claims, and the transport M3 was
written against collapsed them into one measurement:

- **(i) SplitForge does not lose a read that reached it.** A claim about this project's code —
  the sidecar, the fsync ordering, the journal, the restart path. Transport-independent, and
  provable on any adapter.
- **(ii) The transport delivered everything the reader emitted.** A claim about the wire.
  Provable only where the transport tracks its own delivery.

LLRP runs over TCP, so M3b proves both in a single count: the transport knows what it
delivered, the reader buffers while it cannot deliver, and *"the count the reader believes it
sent"* is a question with an answer. Over three wires with no flow control, (ii) is not a
question this interface can be asked at all. M3a can prove (i), in full.

**This is not the module falling short of a bar.** It is a bar stated in a unit this transport
does not have. The distinction is what makes this worth an ADR rather than an edit: a criterion
that cannot be evaluated cannot be *failed* either, so it gates nothing, and would be satisfied
by whatever anybody chose to claim about it.

### Why this is not the softening ADR-0024 refused

ADR-0024's alternatives table rejects *"weaken M3's exit criteria to what a serial module can
close"*, because *"the whole value of an exit criterion is that it is not adjusted to fit what
was built."* That reasoning is intact, and this decision is bound by it.

What ADR-0024 did instead is the pattern being applied here. It closed the six support criteria
a serial module can close and **named the two it cannot** — a reader clock to measure offset
against, and per-antenna identity — as structurally unclosable rather than pending. Naming
clause (ii) the same way is that pattern applied to an exit criterion instead of to the support
checklist. Nothing is restated at a weaker strength: (ii) is declared out of reach on this
transport, and left at full strength where it can be met, which is M3b.

The test of whether this is a softening is whether M3a gets easier. It does not — clause 3
below is work the original criterion did not ask for.

### What this review found that § 7 did not

§ 7 lists polling the tag buffer (§ 8.8.1) as one of the three ways out, priced at *"a
throughput cost nobody has measured."* The price is far higher than that, and it is not a
throughput price.

**§ 8.8.1 deduplicates in hardware.** *"Duplicate tag reads do not result in additional
entries."* Every variant of `SelectionRule` in `crates/splitforge-domain/src/policy.rs`
operates on a burst of reads: `First` is *"earliest read in the burst"*, `FirstAboveRssi` is
*"earliest read at or above an RSSI floor"*, `PeakRssi` picks from a distribution. Under
polling there is no burst to select from. `first-above-rssi:-62` — the rule
[Milestone 2](../roadmap.md#milestone-2--local-event-console)'s own worked example configures —
would have one sample and could not be evaluated. § 8.8.3 confirms the shape from the other
side: for duplicate entries *"the user can decide if the meta data represents the first time
the tag was seen or reflects the meta data for the highest RSSI seen"* — one metadata set per
tag, chosen inside the module.

[Milestone 1](../roadmap.md#milestone-1--simulation-first-vertical-slice)'s exit criterion —
*"duplicate reads are preserved raw while reducing to one accepted timing event"* — would
become structurally unobservable on this adapter. The 638-raw-reads-to-24-crossings shape that
four milestones demonstrate against would arrive as roughly 24 entries carrying a read count.
That is [ADR-0005](0005-raw-read-append-only-journal.md) deleted at the wire, by the reader,
before SplitForge is given anything to preserve.

**And the 52-entry buffer bounds the wrong quantity.** § 8.8.1 holds *"as a rule of thumb […] a
maximum of 52 96-bit EPC tags"*, and because it deduplicates, those 52 are 52 *distinct* tags.
Streaming across a disconnection loses redundant reads of runners who are mostly also read
before and after it; a tag buffer that fills loses a distinct tag, which is a runner with no
crossing at all. In a pack finish — the most tags in the air, the least tolerance for losing
one — polling fails in the worse direction.

**Polling does buy one thing streaming cannot.** A poll is a command-response round trip under
§ 7's *"the reader never initiates"* discipline, so a poll that gets no response **is** a
detected disconnection, at the poll interval, for nothing. Streaming has no such signal *in
anything the user guide documents*: with no control lines and no flow control, a stream that has
gone quiet is indistinguishable from a checkpoint with nobody crossing it. (The SDK later turned
up a candidate the guide never mentions — see the correction under decision 3.) § 8.8.2 closes
the obvious workaround — the host cannot
signal that it wishes *"tag streaming to stop temporarily without stopping the reading of
tags"* — so a liveness probe costs a read gap, which is the thing it was meant to protect.

That asymmetry is real, and it is why clause 3 below exists rather than being assumed away.

## Decision

**Four decisions, and one thing deliberately left open.**

### 1. M3a's exit criterion is restated in three clauses

> A serial module runs for several hours while **every read the host receives is preserved**
> through deliberately induced disconnections and service restarts; **the journal never
> disagrees with what arrived**; and **every disconnection is detected and recorded as a
> bounded gap in the evidence**, rather than passing unnoticed.

What observing each clause takes:

1. **No loss above the transport.** Every frame taken off the port becomes a durable read,
   across induced disconnections, reconnects, and service restarts. This is claim (i), and it
   is the whole of what the original second clause was reaching for.
2. **The journal never disagrees with what arrived.** Sequence numbers contiguous from 1, and a
   sidecar and database that reconcile in both directions — the reconciliation `splitforge
   doctor` and `splitforge recover` already perform
   ([ADR-0018](0018-write-ahead-sidecar-journal.md)).
3. **Every disconnection is detected and recorded**, with a start, an end, and an honest
   statement of which kind of detection produced it.

### 2. The original's second clause is named structurally unclosable

*"The count of reads the module believes it sent matches the count in the journal"* joins the
reader clock and per-antenna identity as a thing this hardware cannot be asked, and is recorded
in [the reader notes](../readers/thingmagic-m7e-pico.md#why-this-cannot-become-supported)
beside them.

**M3b's exit criterion is untouched** and keeps that clause verbatim, because TCP gives it an
answer. **The support matrix stays empty**: neither this ADR nor completing M3a puts any device
in it.

### 3. The adapter streams. Polling the tag buffer is rejected.

The adapter runs in the streaming mode of § 8.8.2. Polling § 8.8.1 is rejected because its
hardware deduplication destroys the burst that all three `SelectionRule` variants operate on,
and makes Milestone 1's exit criterion unobservable on this adapter — a cost to the evidence
model, not to throughput.

**The accepted cost is named rather than mitigated:** no liveness signal is *known* to be
available while streaming, so a quiet stream must be treated as indistinguishable from a quiet
checkpoint.

**Corrected after this ADR was drafted, while it was still `Proposed`.** The sentence above
originally read *"streaming has no liveness signal"*, flatly. Reading the SDK for the command
set turned up two search flags the user guide never mentions —
`TMR_SR_SEARCH_FLAG_STATUS_REPORT_STREAMING` and `TMR_SR_SEARCH_FLAG_STATS_REPORT_STREAMING` —
and a branch in the continuous-read receive path for *"a status stream response"*, a non-tag
frame that arrives mid-stream. So a candidate keepalive exists. Whether it is *periodic*, on
what interval, and whether it arrives with **no tags in the field** — the only properties that
would make it one — are unestablished, and the header that would say is not in the mirror
([finding 12](../readers/vendor-documents.md#12-a-liveness-signal-may-exist-after-all-and-adr-0025-assumed-it-did-not)).
Nothing else in this ADR moves: the decision to stream rested on the deduplication argument,
which is untouched, and clause 3 is needed either way.

### 4. Detecting a disconnection is a deliverable of M3a

Two events are detectable, and both must be recorded:

- **The device node goes away.** A USB-UART bridge unplugged makes `read()` fail and
  `/dev/splitforge-reader` vanish. Unambiguous, immediate, and it is the disconnection an
  observer will actually induce.
- **The stream goes silent.** No frames for longer than a configured interval while a race is
  running. **Ambiguous by construction** — a checkpoint with nobody crossing it is also silent.

**The ambiguity survives into the evidence.** A gap records which detection produced it, and a
silence-derived gap is recorded as *suspected*, never as confirmed. This is a distinction the
project already draws twice: `measurement` beside `state` in the clock-source report, where
*"not measured"* and *"measured, and the clock is bad"* are different facts; and
`ReaderKind::None` beside `ReaderState`, so a device that never had a reader cannot be read as
one that has a working one. A gap that asserted the reader was dead because a 10K's tail end
was quiet would be that same defect a third time.

**A gap is append-only evidence and an input to derivation**, never a correction applied to
reads. [ADR-0011](0011-append-only-enforced-by-triggers.md) governs the table;
[ADR-0023](0023-manual-entries-are-derivation-inputs.md)'s rule governs its role. A gap
overlapping a race window is a fact the results have to be able to carry, not a number anybody
edits.

**A gap is bounded.** Reconnect writes its end. A gap whose end is the end of the journal is
itself a reportable state, not an omission.

**Health reports it.** The `ReaderHealth` shape being built on the read-path branch carries a
`ReaderState`; the gap states belong there, and an open gap degrades `/health` so a watchdog
sees it without parsing prose ([ADR-0021](0021-local-api-listens-on-a-unix-socket.md)). That
shape is not yet merged, which is worth saying plainly: this clause constrains work in flight
rather than describing work observed.

### What is deliberately left open

**The silence threshold's value.** Choosing it needs to know whether the module emits anything
at all during a continuous read with no tags in the field — which the user guide does not say,
and which `serial_reader_l3.c` may. Raised as
[Q14](../open-questions.md#q14-reader-silence-threshold) rather than guessed at here. Until it
is answered the threshold is a configured value with a conservative default and no claim
attached to it.

## Consequences

### What this makes easy

- **M3a has an exit criterion that can be failed.** That is the entire point of one, and the
  previous wording had quietly lost it.
- **The evidence model survives contact with the first physical adapter.** Streaming preserves
  the burst, which is what makes calibrating `--selection-rule first-above-rssi:` against real
  RSSI possible — listed in
  [the reader notes](../readers/thingmagic-m7e-pico.md#still-unknown-and-only-answerable-with-hardware-in-hand)
  as only answerable with hardware, and it stays answerable.
- **A disconnection becomes visible rather than inferred.** An organizer with a hole in their
  evidence is told there is a hole and how wide it is, which is strictly better than a results
  file that is quietly short.
- **M5's four Pi-side measurements are unaffected.** They need a real stream of real reads into
  real flash, which is exactly what this preserves.

### What this makes hard

- **M3a grew a feature it did not have.** A read-activity watchdog, a gap table and its
  migration, two detection paths, health states, and derivation handling — none of it in the
  original list, all of it because the module cannot tell us it stopped.
- **The threshold is a guess until [Q14](../open-questions.md#q14-reader-silence-threshold).**
  Too short and a quiet checkpoint manufactures gaps; too long and a dead reader goes unnoticed
  for that long.
- **Two detection paths with different confidence**, and the difference has to survive into
  health, `doctor`, and the exports — which is exactly where *suspected* quietly becomes
  *confirmed*.
- **The streaming decision forecloses the liveness polling would have given for free**, and
  clause 3 has to be built by hand as a direct result.

### What we accept

- **That reads lost into a dead cable are lost.** Nothing here recovers them. The criterion now
  requires knowing that a window exists, not that it is empty. That is less than M3b will
  prove, and it is what this transport permits.
- **That silence and death must be treated as indistinguishable while streaming**, so a
  suspected gap will sometimes be a quiet checkpoint. Erring toward reporting a gap that was not
  one is the safe direction — it warns about evidence that was fine rather than staying quiet
  about evidence that was not, the same direction already chosen for `Unsynced` in the
  clock-source work. If the status-report stream turns out to be a periodic keepalive, this cost
  shrinks and clause 3 gets cheaper to satisfy; the design does not depend on that going either
  way.
- **That this chooses evidence fidelity over failure detection.** Both matter, on this
  interface they trade against each other, and the project's whole design rests on raw reads
  being preserved. A timer that detects its own failures reliably but cannot apply the
  operator's selection rule has kept the wrong half.
- **That all of this reasoning is from documents rather than from a module** — the same
  standing the CRC had before a captured frame corrected it. The deduplication behavior, the
  52-entry rule of thumb, and whether a quiet stream is truly silent are documentation until
  somebody has one in hand.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Leave the criterion as written; M3a simply cannot close it | Honest, and it wastes the milestone. It leaves M3a permanently open on a clause that is not about SplitForge's code at all, while the durability claim that *is* about SplitForge's code goes unstated and therefore unmeasured |
| Poll the tag buffer (§ 8.8.1) so losses are bounded and countable | Hardware deduplication destroys the burst all three `SelectionRule` variants need, makes M1's exit criterion unobservable, and bounds *distinct tags* at 52 rather than bounding duplicates — losing a runner where streaming loses a redundant read. It buys real liveness, and the price is the evidence model |
| Stream, and stop the stream periodically to probe for liveness | § 8.8.2: the host cannot pause streaming *"without stopping the reading of tags."* Every probe is a read gap, deliberately created, to detect read gaps |
| Drop clause 3 and record only the disconnections the host happens to observe | Reachable today, and it leaves the worst failure mode — a module that dies silently mid-race — outside the criterion entirely. The whole reason the criterion needed revisiting is that this interface does not announce its own failures |
| Amend ADR-0024 rather than write this | ADR process: *"ADRs are not edited after acceptance except to change status."* ADR-0024 did not anticipate this and is not wrong; it is extended |
| Reword only, and leave detection to M3b | Puts the durability claim on the adapter that already had it, and leaves the one that needs it without. M3b gets detection free from TCP; M3a is the milestone where it has to be built |

## References

- [vendor-documents § 7](../readers/vendor-documents.md#7-m3as-exit-criterion-may-not-be-reachable-on-this-interface)
  — the finding this ADR decides, and the three options it declined to choose between
- [vendor-documents, the read-path quotes](../readers/vendor-documents.md#what-the-read-path-will-depend-on-quoted)
  — § 5.1.4.1, § 7, § 8.8.2, and § 8.8.3, quoted while the documents were open
- [ADR-0024](0024-serial-reader-adapter-before-llrp.md) — the split this refines; unamended,
  and the source of the name-what-is-unclosable pattern
- [ADR-0005](0005-raw-read-append-only-journal.md) — the evidence model polling would delete
- [ADR-0011](0011-append-only-enforced-by-triggers.md) — what a gap table has to obey
- [ADR-0023](0023-manual-entries-are-derivation-inputs.md) — the role a gap plays in derivation
- [ADR-0018](0018-write-ahead-sidecar-journal.md) — the reconciliation clause 2 rests on
- [ADR-0021](0021-local-api-listens-on-a-unix-socket.md) — where an open gap surfaces
- [Q14](../open-questions.md#q14-reader-silence-threshold) — the threshold this does not choose
- [`docs/readers/thingmagic-m7e-pico.md`](../readers/thingmagic-m7e-pico.md) — where the third
  structurally unclosable item is recorded
