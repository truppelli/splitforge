//! Who the operating system says is writing an audit row (ADR-0043).
//!
//! `--actor` is a name the command was told, and defaults to `operator`. Nothing checks it.
//! The CLI runs as `sudo -u splitforge`, so the account it runs as is the same whoever types
//! the command. What sets two operators apart is the account sudo was invoked from, which
//! sudo records in the environment of the process it starts.
//!
//! Read here, at the one place every audit row is written, rather than passed down from the
//! CLI: the journal writes audit rows of its own during recovery, and so does the service.

/// The operating system's account of the process writing an audit row.
///
/// All `None` on a row written before migration 9, which recorded none of it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessIdentity {
    /// The process's real uid, from the kernel. A process cannot report another.
    pub uid: Option<u32>,
    /// `SUDO_USER`: the login sudo was invoked from, when sudo started the process.
    ///
    /// Set by sudo, which discards a value the caller supplied unless the host's sudoers
    /// allows it to be kept. A process sudo did not start can set it to anything, which is
    /// why it is recorded beside `uid` and not instead of it.
    pub sudo_user: Option<String>,
    /// `SUDO_UID`: the uid sudo was invoked from, on the same terms as `sudo_user`.
    pub sudo_uid: Option<u32>,
}

impl ProcessIdentity {
    /// This process, now.
    #[must_use]
    pub fn current() -> Self {
        Self {
            uid: real_uid(),
            sudo_user: std::env::var("SUDO_USER")
                .ok()
                .filter(|name| !name.is_empty()),
            sudo_uid: std::env::var("SUDO_UID")
                .ok()
                .and_then(|uid| uid.parse().ok()),
        }
    }
}

#[cfg(unix)]
fn real_uid() -> Option<u32> {
    Some(rustix::process::getuid().as_raw())
}

#[cfg(not(unix))]
fn real_uid() -> Option<u32> {
    None
}
