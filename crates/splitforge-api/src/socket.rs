//! Serving the API on a Unix domain socket.
//!
//! This module is the whole of ADR-0021's mechanism, and it is `#[cfg(unix)]` because the
//! decision only exists on the platform SplitForge targets ([ADR-0002](../../../docs/adr/0002-raspberry-pi-target.md)).
//! There is no fallback listener for other platforms: a Windows or macOS build of the
//! service is a development convenience, and quietly binding a TCP port to make it work
//! would defeat the point of the decision on the machine it was made for.

use std::os::unix::fs::PermissionsExt as _;
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
    if path.exists() {
        std::fs::remove_file(path).map_err(|error| ServeError::Stale(path.to_path_buf(), error))?;
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
        // start because of a leftover file would be the check causing the outage.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("api.sock");
        std::fs::write(&path, b"left over from a crash").expect("write a stale file");
        assert!(path.exists());

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
