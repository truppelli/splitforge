//! Who the audit trail says did something (ADR-0043).
//!
//! The 2026-09-13 review: attribution was whatever `--actor` said, defaulting to `operator`, and
//! the CLI runs as `sudo -u splitforge`, so every operator's rows were the same account under
//! whatever name they typed. These run the binary as sudo would start it, and check that the
//! trail keeps the claim and records the operating system's account beside it.

#![cfg(unix)]

use std::os::unix::fs::MetadataExt;
use std::path::Path;

use tempfile::TempDir;
use tokio::process::Command;

const BINARY: &str = env!("CARGO_BIN_EXE_splitforge");

/// Runs `splitforge` with `env` in place of whatever sudo variables the test itself has.
async fn run(args: &[&str], database: &Path, env: &[(&str, &str)]) -> String {
    let mut command = Command::new(BINARY);
    command
        .args(args)
        .arg("--database")
        .arg(database)
        .env_remove("SUDO_USER")
        .env_remove("SUDO_UID")
        .env_remove("SUDO_GID")
        .env_remove("SUDO_COMMAND");
    for (name, value) in env {
        command.env(name, value);
    }
    let output = command.output().await.expect("run splitforge");
    assert!(
        output.status.success(),
        "splitforge {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf-8")
}

async fn audit_trail(database: &Path) -> Vec<serde_json::Value> {
    let stdout = run(&["audit", "--limit", "100"], database, &[]).await;
    serde_json::from_str::<serde_json::Value>(&stdout)
        .expect("json")
        .as_array()
        .expect("an array")
        .clone()
}

/// The uid this test runs as, which is the uid the binary it starts runs as. Read from a file
/// it just created, so the test needs nothing that calls `getuid` itself.
fn this_uid(directory: &TempDir) -> u64 {
    let probe = directory.path().join("uid-probe");
    std::fs::write(&probe, b"").expect("write the probe");
    u64::from(std::fs::metadata(&probe).expect("stat the probe").uid())
}

#[tokio::test]
async fn a_row_written_under_sudo_names_who_invoked_it_beside_who_it_claims() {
    let directory = TempDir::new().expect("temp dir");
    let database = directory.path().join("event.db");

    run(
        &[
            "fixture",
            "load",
            "--fixture",
            "five-k",
            "--actor",
            "somebody else",
        ],
        &database,
        &[("SUDO_USER", "alice"), ("SUDO_UID", "1001")],
    )
    .await;

    let trail = audit_trail(&database).await;
    assert!(!trail.is_empty(), "loading a fixture is audited");
    for row in &trail {
        assert_eq!(
            row["actor"], "somebody else",
            "the claim is kept as made: {row}"
        );
        assert_eq!(row["sudo_user"], "alice", "{row}");
        assert_eq!(row["sudo_uid"], 1001, "{row}");
        assert_eq!(row["process_uid"], this_uid(&directory), "{row}");
    }
}

#[tokio::test]
async fn a_row_written_without_sudo_still_records_the_account_it_ran_as() {
    let directory = TempDir::new().expect("temp dir");
    let database = directory.path().join("event.db");

    run(&["fixture", "load", "--fixture", "five-k"], &database, &[]).await;

    let trail = audit_trail(&database).await;
    assert!(!trail.is_empty(), "loading a fixture is audited");
    for row in &trail {
        assert_eq!(row["actor"], "operator", "{row}");
        assert_eq!(row["process_uid"], this_uid(&directory), "{row}");
        assert!(row["sudo_user"].is_null(), "no sudo, no sudo user: {row}");
        assert!(row["sudo_uid"].is_null(), "{row}");
    }
}
