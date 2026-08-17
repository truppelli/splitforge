//! Free space on the volume holding the event database.
//!
//! The failure this guards against is slow and quiet. A journal grows all event; an SD card
//! does not. Nothing announces the transition from "plenty of room" to "the next read
//! cannot be written," and the moment it happens is the moment an operator is least able to
//! do anything about it.
//!
//! So the check is a **pre-race** one, where the answer is still actionable — free space,
//! swap the card, attach a USB SSD — rather than a mid-race alarm that arrives when the
//! only remaining option is to keep going and hope.
//!
//! ## Available, not free
//!
//! [`DiskSpace::available_bytes`] is the space available to *this process*, which on most
//! Unix filesystems is smaller than the raw free space because a percentage is reserved for
//! root. Reporting the larger number would be reporting space `splitforge` cannot use.

use std::path::Path;

use crate::StorageError;

/// A conservative floor, in bytes: 256 MiB.
///
/// Chosen against the shape of the data rather than by feel. The 638-read 5K fixture
/// produces a database and sidecar together well under a megabyte, so this is roughly three
/// orders of magnitude of headroom for a small event — which is the right margin when the
/// cost of being wrong is a race that stops recording and the cost of being conservative is
/// deleting an old event file.
///
/// It is a default, not a limit. A day-long event with several thousand entrants on a busy
/// mat should raise it; see `splitforge device set --min-free-mb`.
pub const DEFAULT_MIN_FREE_BYTES: u64 = 256 * 1024 * 1024;

/// What the filesystem holding a path has left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskSpace {
    /// Bytes this process may still write.
    pub available_bytes: u64,
    /// Size of the filesystem, for context in reports.
    pub total_bytes: u64,
}

impl DiskSpace {
    /// Available space in whole mebibytes, for humans and for messages.
    #[must_use]
    pub const fn available_mb(&self) -> u64 {
        self.available_bytes / (1024 * 1024)
    }

    /// Total space in whole mebibytes.
    #[must_use]
    pub const fn total_mb(&self) -> u64 {
        self.total_bytes / (1024 * 1024)
    }

    /// Whether at least `floor` bytes remain.
    #[must_use]
    pub const fn is_above(&self, floor: u64) -> bool {
        self.available_bytes >= floor
    }
}

/// Measures the filesystem holding `path`.
///
/// `path` need not exist yet — the nearest existing ancestor is measured instead, which is
/// what makes this usable before `splitforge init` has created anything.
///
/// # Errors
///
/// Returns [`StorageError`] if no ancestor of `path` exists or the filesystem cannot be
/// queried.
pub fn disk_space(path: &Path) -> Result<DiskSpace, StorageError> {
    let target = existing_ancestor(path).ok_or_else(|| {
        StorageError::Decode(format!(
            "no existing directory above {} to measure",
            path.display()
        ))
    })?;

    let available_bytes = fs4::available_space(target).map_err(|error| {
        StorageError::Decode(format!(
            "reading free space at {}: {error}",
            target.display()
        ))
    })?;
    let total_bytes = fs4::total_space(target).map_err(|error| {
        StorageError::Decode(format!(
            "reading volume size at {}: {error}",
            target.display()
        ))
    })?;

    Ok(DiskSpace {
        available_bytes,
        total_bytes,
    })
}

/// The nearest ancestor of `path` that exists, including `path` itself.
///
/// A relative database path with no parent component — `splitforge --database event.db` —
/// yields an empty `Path` from `parent()`, which exists on no platform. Falling back to `.`
/// keeps the common invocation working.
fn existing_ancestor(path: &Path) -> Option<&Path> {
    path.ancestors()
        .find(|candidate| !candidate.as_os_str().is_empty() && candidate.exists())
        .or_else(|| Path::new(".").exists().then(|| Path::new(".")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn a_real_directory_reports_plausible_space() {
        let dir = tempdir().expect("tempdir");
        let space = disk_space(dir.path()).expect("measure");

        assert!(space.total_bytes > 0, "a mounted filesystem has a size");
        assert!(
            space.available_bytes <= space.total_bytes,
            "available ({}) cannot exceed total ({})",
            space.available_bytes,
            space.total_bytes
        );
    }

    #[test]
    fn a_database_that_does_not_exist_yet_is_measured_by_its_directory() {
        // `splitforge doctor` runs before `splitforge init` has created anything, and it
        // still has to answer "is there room".
        let dir = tempdir().expect("tempdir");
        let absent = dir.path().join("not-created-yet.db");
        assert!(!absent.exists());

        let space = disk_space(&absent).expect("measure through a missing file");
        let directly = disk_space(dir.path()).expect("measure the directory");

        // Volume size, not free space: `available_bytes` moves between the two calls
        // whenever anything else on the machine writes a byte, so asserting on it would be
        // asserting that the test is running alone.
        assert_eq!(
            space.total_bytes, directly.total_bytes,
            "a missing file must resolve to the volume its directory is on"
        );
        assert!(space.available_bytes > 0);
    }

    #[test]
    fn a_bare_relative_filename_falls_back_to_the_working_directory() {
        // `--database event.db` has no parent component at all.
        let space = disk_space(Path::new("event.db")).expect("measure");
        assert!(space.total_bytes > 0);
    }

    #[test]
    fn the_floor_comparison_is_inclusive() {
        let space = DiskSpace {
            available_bytes: 100,
            total_bytes: 1_000,
        };
        assert!(space.is_above(100), "exactly at the floor is not below it");
        assert!(space.is_above(99));
        assert!(!space.is_above(101));
    }

    #[test]
    fn megabytes_round_down_so_a_report_never_overstates_headroom() {
        let space = DiskSpace {
            available_bytes: 2 * 1024 * 1024 - 1,
            total_bytes: 8 * 1024 * 1024,
        };
        assert_eq!(space.available_mb(), 1);
        assert_eq!(space.total_mb(), 8);
    }
}
