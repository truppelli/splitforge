//! The systemd unit, checked against the binary it starts.
//!
//! A unit file is configuration that nothing compiles and no test suite normally reads, so
//! it drifts silently: someone changes a default path in `Args`, the unit still provisions
//! the old directory, and the service fails to start on a Pi at 6 a.m. with a permission
//! error that reads like a packaging problem.
//!
//! Every assertion here compares the unit against a fact taken from somewhere else — the
//! binary's own `--help` output, `splitforge_api::DEFAULT_SOCKET_PATH`, or the sysusers
//! file — rather than against a constant restated in this file. A test that restated the
//! path would drift in exactly the same way.
//!
//! What this cannot check is whether the unit *runs*: whether the syscall filter is wide
//! enough for `rusqlite`'s `fsync`, whether the hardening breaks something on the Pi's
//! kernel. That is validated on hardware, or by `systemd-analyze` — see docs/deployment.md.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The service binary, built by Cargo for this test run.
const SERVICE: &str = env!("CARGO_BIN_EXE_splitforge-edge");

fn deploy() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("splitforge-edge sits two directories below the workspace root")
        .join("deploy")
}

/// One unit file, as an ordered list of `Key=Value` pairs.
///
/// Section headers are dropped: the keys systemd defines are unique across `[Unit]` and
/// `[Service]`, and nothing here needs to know which section a directive came from.
struct Unit {
    entries: Vec<(String, String)>,
}

impl Unit {
    fn load() -> Self {
        let path = deploy().join("splitforge-edge.service");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));

        let mut entries = Vec::new();
        for line in text.lines() {
            let line = line.trim();

            // systemd joins a line ending in a backslash with the next one. Nothing here
            // uses that, and the parser below would misread it if something started to.
            assert!(
                !line.ends_with('\\'),
                "the unit uses a line continuation, which this parser does not handle: {line}"
            );

            if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .unwrap_or_else(|| panic!("not a directive and not a comment: {line}"));
            entries.push((key.trim().to_owned(), value.trim().to_owned()));
        }
        Self { entries }
    }

    /// Every value given for one key, in order.
    fn all(&self, key: &str) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
            .collect()
    }

    /// The single value for one key. Panics if it is missing or repeated, because a
    /// directive silently overriding an earlier one is its own class of bug.
    fn one(&self, key: &str) -> &str {
        let values = self.all(key);
        assert_eq!(
            values.len(),
            1,
            "expected exactly one {key}= in the unit, found {}",
            values.len()
        );
        values[0]
    }
}

/// A default path as the binary itself reports it.
///
/// Read out of `--help` rather than restated here, so this compares the unit against the
/// program rather than against a second copy of the same assumption.
fn default_for(option: &str) -> String {
    let help = Command::new(SERVICE)
        .arg("--help")
        .output()
        .expect("run splitforge-edge --help");
    assert!(help.status.success(), "--help failed");
    let help = String::from_utf8(help.stdout).expect("utf-8");

    let (_, after) = help
        .split_once(&format!("--{option} "))
        .unwrap_or_else(|| panic!("--help never mentions --{option}:\n{help}"));
    let (_, after) = after
        .split_once("[default: ")
        .unwrap_or_else(|| panic!("--{option} has no documented default:\n{help}"));
    let (value, _) = after.split_once(']').expect("an unterminated default");
    value.to_owned()
}

#[test]
fn the_unit_starts_the_binary_this_crate_builds() {
    let unit = Unit::load();
    let exec_start = unit.one("ExecStart");

    let program = exec_start.split_whitespace().next().expect("a program");
    assert_eq!(
        Path::new(program)
            .file_name()
            .and_then(std::ffi::OsStr::to_str),
        Some("splitforge-edge"),
        "ExecStart runs something other than this service: {exec_start}"
    );

    // No arguments. The binary's defaults and the directories provisioned below are the
    // same paths — asserted by the next two tests — so passing them again would create a
    // second place for them to disagree.
    assert_eq!(
        exec_start, program,
        "ExecStart passes arguments; the defaults are supposed to be enough: {exec_start}"
    );
}

#[test]
fn the_state_directory_is_where_the_binary_actually_writes() {
    let unit = Unit::load();
    let database = default_for("database");
    let state = unit.one("StateDirectory");

    assert_eq!(
        Path::new(&database).parent().expect("a parent directory"),
        Path::new("/var/lib").join(state),
        "StateDirectory={state} does not provision the directory the binary defaults to \
         ({database}). systemd creates and chowns exactly one of these, and the service \
         will fail to open the database it was given."
    );
}

#[test]
fn the_runtime_directory_provisions_the_socket_the_api_names() {
    let unit = Unit::load();
    let socket = default_for("socket");

    // Three statements of the same path, from three places that can drift apart: the
    // library constant, the binary's parsed default, and the directory systemd creates.
    assert_eq!(
        socket,
        splitforge_api::DEFAULT_SOCKET_PATH,
        "the binary's default socket is not the one splitforge-api documents"
    );

    let runtime = unit.one("RuntimeDirectory");
    assert_eq!(
        Path::new(&socket).parent().expect("a parent directory"),
        Path::new("/run").join(runtime),
        "RuntimeDirectory={runtime} does not provision the directory the socket goes in \
         ({socket})"
    );
}

#[test]
fn the_service_does_not_wait_for_a_network_that_may_not_exist() {
    // ADR-0022. A checkpoint has no DHCP, no Internet, and often no switch. A `Wants=` on
    // any network target turns "the network is not coming" into a 90-second boot delay or
    // a service that never starts, which is the outage the offline-first design (ADR-0008)
    // exists to prevent.
    let unit = Unit::load();

    for directive in ["Wants", "Requires", "BindsTo", "Requisite"] {
        for value in unit.all(directive) {
            assert!(
                !value.contains("network"),
                "{directive}={value} makes the timer's startup depend on a network it may \
                 not have. Ordering with After= is free; requiring it is not. See ADR-0022."
            );
        }
    }

    let after = unit.one("After");
    assert!(
        after.contains("network.target"),
        "the unit should still be *ordered* after the network when there is one: {after}"
    );
    assert!(
        !after.contains("network-online.target"),
        "network-online.target is the one that blocks. Order after network.target instead \
         — see ADR-0022: {after}"
    );
}

#[test]
fn the_service_never_stops_restarting() {
    // ADR-0002 asks for Restart=always. On its own that is not enough: systemd's default
    // rate limit gives up permanently after 5 starts in 10 seconds, and a timer that has
    // given up records nothing for the rest of the event.
    let unit = Unit::load();

    assert_eq!(unit.one("Restart"), "always");
    assert_eq!(
        unit.one("StartLimitIntervalSec"),
        "0",
        "without this, systemd stops the service permanently after a short crash loop"
    );
}

#[test]
fn the_kernel_is_what_stops_this_service_binding_a_port() {
    // ADR-0021 says the API binds no port. `crates/splitforge-api/tests/socket.rs` proves
    // the crate contains no listener; this proves the deployment could not open one even
    // if it did — including from a dependency, which no source-reading test can see.
    let unit = Unit::load();
    let families = unit.one("RestrictAddressFamilies");

    assert_eq!(
        families, "AF_UNIX",
        "the service should be able to open Unix sockets and nothing else. Milestone 3 \
         adds a reader over TCP and will need AF_INET here — deliberately, and reviewed \
         as the security decision it is."
    );
}

#[test]
fn the_unit_and_the_sysusers_file_agree_on_the_account() {
    // Two files, one account. If they disagree, `systemctl start` fails with a message
    // about a user that does not exist, which looks like a broken install rather than a
    // typo in a file nobody diffed.
    let unit = Unit::load();
    let path = deploy().join("splitforge.sysusers.conf");
    let sysusers = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));

    let declared: Vec<&str> = sysusers
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("u "))
        .filter_map(|line| line.split_whitespace().nth(1))
        .collect();

    assert_eq!(declared.len(), 1, "expected one user, got {declared:?}");
    assert_eq!(unit.one("User"), declared[0]);
    assert_eq!(
        unit.one("Group"),
        declared[0],
        "sysusers gives a user its own group of the same name"
    );

    // The account exists to own a database and a socket. A login shell on it turns a
    // service compromise into a device compromise.
    assert!(
        sysusers.contains("nologin") || sysusers.contains("/bin/false"),
        "the service account should not be able to log in:\n{sysusers}"
    );
}

#[test]
fn nothing_the_service_writes_is_world_readable() {
    // The database holds participant names and the sidecar holds every raw read in plain
    // text (ADR-0018). Without a UMask= the service creates both 0644, which was the actual
    // state of a real service run before this line existed — see docs/deployment.md.
    let unit = Unit::load();
    let umask = unit.one("UMask");

    let bits = u32::from_str_radix(umask.trim_start_matches('0'), 8)
        .unwrap_or_else(|error| panic!("UMask={umask} is not octal: {error}"));

    assert_eq!(
        bits & 0o007,
        0o007,
        "UMask={umask} leaves the event database and the write-ahead sidecar readable by \
         every account on the device"
    );
}

#[test]
fn the_hardening_that_is_load_bearing_is_present() {
    // Not an audit of every directive — `systemd-analyze security` does that better, and
    // docs/deployment.md records its score. These four are the ones whose absence would
    // not be noticed until it mattered.
    let unit = Unit::load();

    for directive in [
        "NoNewPrivileges",
        "ProtectSystem",
        "ProtectHome",
        "PrivateTmp",
    ] {
        let value = unit.one(directive);
        assert!(
            value != "no" && value != "false",
            "{directive}={value} disables a protection the unit is supposed to apply"
        );
    }
}
