# ADR-0044: A CSV cell is never a formula

- **Status:** Proposed
- **Date:** 2026-09-26
- **Extends:** [ADR-0042](0042-a-result-says-when-its-time-was-typed.md) (on what the export contract's version rule allows)

## Context

The results CSV exists to be opened in a spreadsheet. [Milestone 6](../roadmap.md#milestone-6--integrations)
put its contract marker in a trailing column rather than a preamble line for exactly that
reason, because Excel is how organizers read it. The crossings CSV is the operator's
diagnostic, and gets opened the same way.

Both write names exactly as they were registered, and registration is public. The
2026-09-13 security review pointed out that a runner registered as
`=HYPERLINK("http://…","Results")` becomes a live formula on the organizer's machine when the
file is opened. Excel, LibreOffice and Google Sheets all treat a cell starting with `=`, `+`,
`-` or `@` as a formula, and a leading tab or carriage return can hide one. `bib` comes from
the same roster, `status_reason` is typed by an operator, and a checkpoint name and a chip id
are configured text.

The standard defence is to put `'` in front of such a cell, which every one of those
spreadsheets shows as text. But the results CSV is a versioned contract, and
`RESULTS_VERSION`'s rule is that it is bumped when a field *changes meaning or disappears*.
The review said to decide the fix under that rule.

## Decision

**1. A text cell a spreadsheet would run is written with `'` in front.** In the results CSV
that means `bib`, `name` and `status_reason`. In the crossings CSV it means `checkpoint`,
`bib`, `name` and `chip`. A cell is escaped when it starts with `=`, `+`, `-`, `@`, a tab or a
carriage return. One function does it, `splitforge_export::spreadsheet_safe`, and both CSVs
call it.

**2. Only text that came from outside is escaped.** Columns SplitForge formats itself (places,
times, milliseconds, instants, flags and the version) are not. The crossings CSV's RSSI is a
negative number, and escaping it would turn `-60` into text.

**3. The JSON is not escaped.** It is not opened in a spreadsheet, and a consumer of it gets
the name as it was registered.

**4. `RESULTS_VERSION` stays 1.** Each column keeps its meaning: `name` is still the
participant's name, as a spreadsheet will display it. The values that change are ones no real
name, bib or reason has, and a consumer that knew every earlier value meets a `'` in front of
something that was never a sensible value. [ADR-0042](0042-a-result-says-when-its-time-was-typed.md)
read the rule the same way when `flags` gained values.

## Consequences

### What this makes easy

- **Opening the file is safe.** No cell in either CSV runs as a formula, whatever somebody
  registered as.
- **The escaping is in one place**, with the list of characters beside it, so a new CSV or a
  new text column uses the same function.

### What this makes hard

- **A new text column has to remember to call it.** Nothing forces it. The test for the
  results CSV names the three columns it expects to be escaped.

### What we accept

- **A program that reads the CSV sees the `'`.** A consumer that joins the CSV's names against
  the roster will not match an escaped one. The JSON exists for programs, and carries the
  name unescaped.
- **A legitimate value that starts with `-` or `+` is escaped too.** A bib like `-12` would be
  written `'-12`. That is visible, and it is safer than a rule that tries to tell a number from
  a formula.
- **The roster still accepts such names.** Refusing them at import would not cover databases
  that already hold them, or reasons typed by operators, and a name is the registrant's to
  choose.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Bump `RESULTS_VERSION` to 2 | No column changed meaning, so the rule does not ask for it, and every consumer checking for version 1 would break, including JSON consumers that see no difference |
| Refuse such names at roster import instead | Does not cover existing databases, operator-typed reasons, or configured checkpoint names |
| Escape every cell | A negative RSSI is a number, and escaping it makes it text |
| Wrap such cells in `="…"` | That is itself a formula, and it breaks spreadsheets that do not evaluate it |
| Strip the leading character | Silently changes the name, where `'` is visible and displays as the name |

## References

- [roadmap, security review](../roadmap.md#security-review--2026-09-13): the finding
- [ADR-0042](0042-a-result-says-when-its-time-was-typed.md): the version rule read the same way
- `crates/splitforge-export/src/lib.rs`: `spreadsheet_safe`, `RESULTS_VERSION` and its rule
- [OWASP, CSV injection](https://owasp.org/www-community/attacks/CSV_Injection)
