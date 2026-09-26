# ADR-0043: The audit trail records who the system says acted, beside who the command was told

- **Status:** Proposed
- **Date:** 2026-09-26
- **Extends:** [ADR-0011](0011-append-only-enforced-by-triggers.md)

## Context

[Threat model § 5](../threat-model.md#5-design-decisions-that-follow-from-this-model) makes
insider fabrication a matter of detection: it cannot be prevented, so it has to be visible
afterward. S4 relies on the same thing, and says every manual entry is *"attributed and
audit-logged"*.

The 2026-09-13 security review found that the attribution was a name the command was told.
Every `audit_log` row carries `actor`, which is `--actor`, which defaults to `operator` and is
checked by nothing. When the service created the database, the CLI runs as
`sudo -u splitforge` ([deployment.md](../deployment.md#who-can-do-what)), so two operators on
the same device write rows under the same account, told apart only by whatever each typed.
Nearly every row says `operator`. An operator who wanted a row to name somebody else only had
to say so.

The account sudo was invoked from is known to the process: sudo puts it in the environment of
the command it starts, as `SUDO_USER` and `SUDO_UID`. The kernel knows the account the process
runs as. Neither was recorded.

## Decision

**1. Every audit row records three facts from the operating system beside `actor`**, in new
nullable columns on `audit_log` (migration 9):

| Column | Source | What it says |
|---|---|---|
| `process_uid` | `getuid()` | The real uid of the process that wrote the row |
| `sudo_user` | `SUDO_USER` | The login sudo was invoked from, when sudo started the process |
| `sudo_uid` | `SUDO_UID` | The uid sudo was invoked from, likewise |

**2. They are captured where the row is written**, in storage's one audit insert, rather than
passed down from the CLI. Recovery writes audit rows from inside the journal, and the service
writes its own. Capturing at the insert covers every row, including ones written by code that
does not exist yet.

**3. `actor` stays, and stays a claim.** It is still how an operator names themself, and it is
how the service's rows say `splitforge-edge`. The CLI's audit view documents it as the name the
command was told.

**4. Nothing is refused.** A row with no sudo is written as before, with `process_uid` and
nulls. Rows written before migration 9 read back with all three null. That means "not
recorded", not "nobody".

## Consequences

### What this makes easy

- **Telling operators apart under a shared service account.** `sudo -u splitforge` records
  the login that ran it, so `alice` and `bob` are different rows whatever `--actor` said.
- **Seeing a false claim.** A row whose `actor` names somebody other than its `sudo_user` is
  visible in `splitforge audit`, which is the detection § 5 promises.

### What this makes hard

- **Consumers of `splitforge audit`'s JSON meet three new fields.** They are additions, and
  the audit view is not a published contract the way results are.

### What we accept

- **The sudo half is only as good as sudo.** sudo discards `SUDO_*` values supplied by its
  caller unless the host's sudoers says to keep them (`SETENV`, or `env_keep`). A process sudo
  did not start can set them to anything. So `process_uid` is recorded beside them and not
  instead of them: a row claiming `sudo_user = alice` from a process that ran as `alice`
  herself was not written through sudo at all. The deployment guide says not to relax
  `env_reset` for this account.
- **Anyone who can write the database file can write any row.** Root, or the `splitforge`
  account with a shell, can insert rows directly with `sqlite3`, identity columns included.
  This records honest operators accurately and makes a casual false claim visible. It does not
  stop an administrator from fabricating, which the threat model already says cannot be
  prevented on a device the adversary administers.
- **A uid, not a name, for the process.** Resolving it to a name at write time would record
  what `/etc/passwd` said that day, and uids are what the kernel actually knows. `sudo_user`
  is a name because sudo supplies one.
- **Storage reads two environment variables.** That is a process-level fact read in a storage
  crate, which is otherwise given everything it knows. It is confined to one module,
  `identity.rs`, and the alternative (decision 2) misses rows.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Default `--actor` to `SUDO_USER` | Still a claim, still overridable, and it loses the difference between what was typed and what the system saw, which is what detection needs |
| Refuse `--actor` values that differ from `SUDO_USER` | An operator entering a time for a colleague who called it in is legitimate, and the audit row should say both |
| Put the identity in `detail_json` | Not every row has detail, the column is free-form, and a fact every row carries belongs in a column a query can use |
| Capture the identity in the CLI and pass it down | Recovery and the service write audit rows the CLI never sees |
| Record the process's login name via `getlogin()` | It reads the controlling terminal's utmp entry, which is absent under systemd and over non-interactive SSH, and is not the account the process runs as |

## References

- [roadmap, security review](../roadmap.md#security-review--2026-09-13): the finding
- [threat model § 5](../threat-model.md#5-design-decisions-that-follow-from-this-model): detection over prevention
- [deployment.md, who can do what](../deployment.md#who-can-do-what): why the CLI runs as the service account
- `crates/splitforge-storage/src/identity.rs`: where the identity is read
