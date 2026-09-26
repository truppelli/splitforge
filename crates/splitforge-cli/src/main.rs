//! The `splitforge` binary.
//!
//! Thin on purpose: parse, dispatch, and turn an error into an exit code. Everything worth
//! testing lives in the library, where an integration test can call it without a
//! subprocess.

// No panicking on any path reachable during an event: a corrupt frame, a missing
// field, or an out-of-range value must become an error the caller can act on, never a
// timer that stops mid-race. Test code is exempt, where panicking on the unexpected is
// the point. See CONTRIBUTING.md, "Code standards".
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::process::ExitCode;

use clap::Parser;
use splitforge_cli::Cli;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match splitforge_cli::run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // `{:#}` prints the whole anyhow chain on one line: "appending read X to the
            // journal: sqlite error: disk I/O error" says considerably more at 6 a.m. in a
            // car park than either half on its own.
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
