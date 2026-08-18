# Security Policy

## Project status

SplitForge is **pre-alpha**. There is no released version, no supported version, and no
production deployment we are aware of. Treat everything in this repository as
unreviewed.

| Version | Supported |
|---|---|
| `main` (unreleased) | Best effort |
| Everything else | Nothing else exists yet |

## Reporting a vulnerability

**Do not open a public issue for a security problem.**

Report privately through GitHub's private vulnerability reporting:
[Security → Report a vulnerability](https://github.com/truppelli/splitforge/security/advisories/new).

Please include:

- What the issue is, and which crate or component it affects
- How to reproduce it, ideally a minimal case
- What an attacker gains — race data corruption, network access, denial of timing service
- Whether it requires LAN access, physical access, or neither

If the report is about a device that has run an event, `splitforge doctor --bundle out.json`
produces the state of that device with no participant names, bib numbers, or chip
identifiers in it. **Send the bundle, not the database.** An event database holds a roster,
and a roster sent to a stranger to help debug a timing bug is a privacy incident caused by
trying to be helpful.

You should get an acknowledgment within 7 days. There is no bug bounty; SplitForge is an
unfunded open-source project.

## What we consider in scope

SplitForge's threat model is a machine sitting on an untrusted temporary network at a
sporting event, holding data that determines competitive outcomes. In scope:

- Anything that lets an unauthenticated party on the LAN read, alter, or delete event data
- Anything that causes silent loss of a recorded raw read
- Anything that lets malformed reader input crash, hang, or corrupt the timer
- Injection of fabricated timing events or results
- Credential exposure for outbound integrations
- Denial of service against the local API that prevents an event from being timed

See [`docs/threat-model.md`](docs/threat-model.md) for the full risk register.

## What is out of scope

- Physical theft of the device (mitigated operationally, not in software)
- Attacks requiring root on the Pi — root already owns the event database
- **RFID tag cloning and EPC spoofing at the RF layer.** This is a real attack on chip
  timing, and it is not one SplitForge can solve in software. We mitigate through audit
  trails that make tampering *detectable after the fact*, not preventable
- Vulnerabilities in Raspberry Pi OS, the kernel, or reader firmware — report those upstream

## Disclosure

We will coordinate a fix and disclose publicly once a patch exists, crediting the
reporter unless they prefer otherwise. If a vulnerability is being actively exploited
against live events, we will publish mitigation guidance before a full fix is ready.
