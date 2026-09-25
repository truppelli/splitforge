//! Serving the API on a Unix domain socket.
//!
//! This module is the whole of ADR-0021's mechanism, and it is `#[cfg(unix)]` because the
//! decision only exists on the platform SplitForge targets ([ADR-0002](../../../docs/adr/0002-raspberry-pi-target.md)).
//! There is no fallback listener for other platforms: a Windows or macOS build of the
//! service is a development convenience, and quietly binding a TCP port to make it work
//! would defeat the point of the decision on the machine it was made for.

use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use axum::Router;
use tokio::net::UnixListener;

/// `srw-rw----` — owner and group only.
///
/// The **entire** access control for the API, which is what makes this constant worth
/// asserting in a test rather than trusting. World-writable here would hand the API to
/// every account on the device, and the umask that is in force when the service happens to
/// start is not something to leave it to.
pub const SOCKET_MODE: u32 = 0o660;

/// Why the API could not be served.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The socket's directory did not exist and could not be created.
    #[error("creating {0}: {1}")]
    Directory(PathBuf, std::io::Error),
    /// What was already at the path could not be examined.
    #[error("checking what is at {0}: {1}")]
    Inspect(PathBuf, std::io::Error),
    /// Something other than a socket was at the path. It was left alone and nothing was
    /// bound, because it is not this service's to delete.
    #[error(
        "{0} is not a socket, so it was left alone and the API was not started. Check          --socket, or move the file if it should not be there"
    )]
    NotASocket(PathBuf),
    /// A socket file was already there and could not be removed.
    #[error("removing the stale socket at {0}: {1}")]
    Stale(PathBuf, std::io::Error),
    /// The socket could not be bound.
    #[error("binding {0}: {1}")]
    Bind(PathBuf, std::io::Error),
    /// The socket was bound but could not be locked down.
    #[error("setting permissions on {0}: {1}")]
    Permissions(PathBuf, std::io::Error),
    /// The server stopped with an error.
    #[error("serving: {0}")]
    Serve(std::io::Error),
}

/// Serves `router` on a Unix socket at `path` until `shutdown` resolves.
///
/// Removes the socket on the way out, so a clean stop does not leave a file that the next
/// start has to reason about.
///
/// # Errors
///
/// Returns [`ServeError`] if the socket cannot be created, secured, or served.
pub async fn serve_on_socket(
    path: &Path,
    router: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServeError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| ServeError::Directory(parent.to_path_buf(), error))?;
    }

    // A socket left by a crashed process is not an error worth refusing to start over.
    // `bind` fails with `EADDRINUSE` on an existing file whether or not anything is
    // listening, and the threat model is explicit that availability is a security property
    // here: a timer that will not start because of a leftover file has caused the harm it
    // was protecting against.
    //
    // **Only a socket.** Anything else at the path is not a leftover of this service, and a
    // `--socket` typo is enough to name the event database. Refusing costs a service that
    // does not start and says why; removing costs the event. `symlink_metadata`, so that a
    // link is judged as itself and never followed to something it would be wrong to delete.
    match std::fs::symlink_metadata(path) {
        Ok(existing) if existing.file_type().is_socket() => std::fs::remove_file(path)
            .map_err(|error| ServeError::Stale(path.to_path_buf(), error))?,
        Ok(_) => return Err(ServeError::NotASocket(path.to_path_buf())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(ServeError::Inspect(path.to_path_buf(), error)),
    }

    let listener =
        UnixListener::bind(path).map_err(|error| ServeError::Bind(path.to_path_buf(), error))?;

    // After binding, not before: the file does not exist until `bind` creates it. There is
    // a window between the two in which the socket carries whatever the umask gave it, and
    // closing it properly needs a umask dance around the bind. It is left open deliberately
    // — the window is microseconds at service start, and the alternative is fiddling with
    // process-global state from a library.
    let permissions = std::fs::Permissions::from_mode(SOCKET_MODE);
    std::fs::set_permissions(path, permissions)
        .map_err(|error| ServeError::Permissions(path.to_path_buf(), error))?;

    let result = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(ServeError::Serve);

    // Best effort. If this fails the next start removes it anyway, and failing to clean up
    // is not a reason to report an otherwise clean shutdown as an error.
    let _ = std::fs::remove_file(path);

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::{Health, HealthSource};

    fn source() -> Arc<dyn HealthSource> {
        Arc::new(|| Health::ok("0.0.0", 1, "event.db", 4, 638, 256))
    }

    #[tokio::test]
    async fn the_socket_is_not_readable_by_the_world() {
        // The socket's mode is the entire access control for the API (ADR-0021), which
        // makes this the test that the decision is actually implemented rather than merely
        // documented.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("api.sock");

        let (stop, wait) = tokio::sync::oneshot::channel::<()>();
        let serving = tokio::spawn({
            let path = path.clone();
            async move {
                serve_on_socket(&path, crate::router(source()), async {
                    let _ = wait.await;
                })
                .await
            }
        });

        wait_for(&path).await;
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(
            mode & 0o777,
            SOCKET_MODE,
            "expected {SOCKET_MODE:o}, got {:o}",
            mode & 0o777
        );

        stop.send(()).expect("stop");
        serving.await.expect("join").expect("serve");
    }

    #[tokio::test]
    async fn a_socket_left_by_a_crash_does_not_stop_the_next_start() {
        // `/run` is a tmpfs so a reboot clears it, but a crash does not — and refusing to
        // start because of a leftover socket would be the check causing the outage.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("api.sock");
        // A listener that is dropped without being unlinked is exactly what a killed process
        // leaves behind: the file stays, and nothing is listening on it.
        drop(std::os::unix::net::UnixListener::bind(&path).expect("bind a socket to leave"));
        let left = std::fs::symlink_metadata(&path).expect("the socket was left");
        assert!(left.file_type().is_socket());

        let (stop, wait) = tokio::sync::oneshot::channel::<()>();
        let serving = tokio::spawn({
            let path = path.clone();
            async move {
                serve_on_socket(&path, crate::router(source()), async {
                    let _ = wait.await;
                })
                .await
            }
        });

        wait_for(&path).await;
        stop.send(()).expect("stop");
        serving
            .await
            .expect("join")
            .expect("a stale socket must not be fatal");
    }

    /// Starts a server at `path` that would run until the test ends, and returns what it
    /// returned, or `None` if it was still serving after a few seconds.
    async fn start_at(path: &Path) -> Option<Result<(), ServeError>> {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            serve_on_socket(path, crate::router(source()), std::future::pending()),
        )
        .await
        .ok()
    }

    #[tokio::test]
    async fn a_file_that_is_not_a_socket_is_left_alone() {
        // A `--socket` typo in a drop-in is enough to point this at the event database. What
        // is there is not this service's to delete, so nothing is bound and the file is kept.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("event.db");
        std::fs::write(&path, b"every raw read of the event").expect("write the file");

        let outcome = start_at(&path).await;

        assert!(
            matches!(outcome, Some(Err(ServeError::NotASocket(ref at))) if *at == path),
            "expected a refusal naming {}, got {outcome:?}",
            path.display()
        );
        assert_eq!(
            std::fs::read(&path).expect("the file is still there"),
            b"every raw read of the event"
        );
    }

    #[tokio::test]
    async fn a_symlink_at_the_socket_path_is_left_alone() {
        // `symlink_metadata`, not `metadata`: a link is refused as itself, whatever it points
        // at, and neither the link nor its target is touched.
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("event.db");
        std::fs::write(&target, b"every raw read of the event").expect("write the target");
        let path = dir.path().join("api.sock");
        std::os::unix::fs::symlink(&target, &path).expect("link");

        let outcome = start_at(&path).await;

        assert!(
            matches!(outcome, Some(Err(ServeError::NotASocket(_)))),
            "expected a refusal, got {outcome:?}"
        );
        assert!(
            std::fs::symlink_metadata(&path)
                .expect("the link is still there")
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read(&target).expect("the target is still there"),
            b"every raw read of the event"
        );
    }

    #[tokio::test]
    async fn a_clean_shutdown_takes_the_socket_with_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("api.sock");

        let (stop, wait) = tokio::sync::oneshot::channel::<()>();
        let serving = tokio::spawn({
            let path = path.clone();
            async move {
                serve_on_socket(&path, crate::router(source()), async {
                    let _ = wait.await;
                })
                .await
            }
        });

        wait_for(&path).await;
        stop.send(()).expect("stop");
        serving.await.expect("join").expect("serve");

        assert!(
            !path.exists(),
            "a clean stop should not leave a file the next start has to reason about"
        );
    }

    #[tokio::test]
    async fn the_directory_is_created_when_it_is_missing() {
        // systemd's `RuntimeDirectory=` makes `/run/splitforge` for the real service, but
        // nothing does for a developer running the binary by hand.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("deeper").join("api.sock");

        let (stop, wait) = tokio::sync::oneshot::channel::<()>();
        let serving = tokio::spawn({
            let path = path.clone();
            async move {
                serve_on_socket(&path, crate::router(source()), async {
                    let _ = wait.await;
                })
                .await
            }
        });

        wait_for(&path).await;
        assert!(path.exists());
        stop.send(()).expect("stop");
        serving.await.expect("join").expect("serve");
    }

    /// Waits for the socket to appear, so a test never races the server's first bind.
    async fn wait_for(path: &Path) {
        for _ in 0..200 {
            if path.exists() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the socket never appeared at {}", path.display());
    }
}
