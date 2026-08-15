//! The `splitforge` binary.
//!
//! Thin on purpose: parse, dispatch, and turn an error into an exit code. Everything worth
//! testing lives in the library, where an integration test can call it without a
//! subprocess.

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
