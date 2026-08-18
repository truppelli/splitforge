//! The API over a real socket, spoken to the way a caller would.
//!
//! The requests here are hand-written HTTP/1.1 rather than built by a client library. That
//! is deliberate: adding an HTTP client as a dev-dependency to test a one-endpoint API
//! would roughly double what this crate pulls in, and a `GET` plus a status line is little
//! enough to write out. It also means the test asserts what actually goes over the socket,
//! rather than what a client library and this server happen to agree on.
//!
//! Unix only, because the API is (ADR-0021). On other platforms this file compiles to
//! nothing rather than testing a fallback that deliberately does not exist.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use splitforge_api::{Health, HealthSource, router, serve_on_socket};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;

/// A server on a temporary socket, stopped when the test drops it.
struct Server {
    _directory: tempfile::TempDir,
    path: std::path::PathBuf,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    joined: Option<tokio::task::JoinHandle<Result<(), splitforge_api::ServeError>>>,
}

impl Server {
    async fn start(health: Health) -> Self {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("api.sock");
        let (stop, wait) = tokio::sync::oneshot::channel::<()>();

        let source: Arc<dyn HealthSource> = Arc::new(move || health.clone());
        let joined = tokio::spawn({
            let path = path.clone();
            async move {
                serve_on_socket(&path, router(source), async {
                    let _ = wait.await;
                })
                .await
            }
        });

        for _ in 0..300 {
            if path.exists() {
                return Self {
                    _directory: directory,
                    path,
                    stop: Some(stop),
                    joined: Some(joined),
                };
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the socket never appeared");
    }

    /// Sends one request and returns the whole response, headers included.
    async fn get(&self, target: &str) -> String {
        let mut stream = UnixStream::connect(&self.path).await.expect("connect");
        let request =
            format!("GET {target} HTTP/1.1\r\nHost: splitforge\r\nConnection: close\r\n\r\n");
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write request");

        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .await
            .expect("read response");
        response
    }

    async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(joined) = self.joined.take() {
            joined.await.expect("join").expect("serve");
        }
    }
}

/// Splits a raw response into its status line and its body.
fn split(response: &str) -> (&str, &str) {
    let (head, body) = response
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("no header/body boundary in: {response}"));
    let status = head.lines().next().expect("a status line");
    (status, body)
}

fn healthy() -> Health {
    Health::ok("0.0.0", 61, "event.db", 4, 638, 256)
}

#[tokio::test]
async fn a_healthy_device_answers_200_with_its_numbers() {
    let server = Server::start(healthy()).await;
    let response = server.get("/health").await;
    let (status, body) = split(&response);

    assert!(status.contains("200 OK"), "got: {status}");

    let json: serde_json::Value = serde_json::from_str(body).expect("a JSON body");
    assert_eq!(json["status"], "ok");
    assert_eq!(json["degraded_by"].as_array().expect("array").len(), 0);
    assert_eq!(json["raw_reads"], 638);
    assert_eq!(json["schema_version"], 4);
    assert_eq!(json["database"], "event.db");
    assert_eq!(json["uptime_seconds"], 61);

    server.stop().await;
}

#[tokio::test]
async fn a_degraded_device_answers_503_so_a_monitor_need_not_parse_the_body() {
    // The status code is the contract for `curl --fail`, a systemd watchdog, and a shell
    // script. The body says the same thing for whoever is reading it.
    let mut health = healthy();
    health.degrade("12 MB free, below the 256 MB floor");
    let server = Server::start(health).await;

    let response = server.get("/health").await;
    let (status, body) = split(&response);

    assert!(status.contains("503"), "got: {status}");

    let json: serde_json::Value = serde_json::from_str(body).expect("a JSON body");
    assert_eq!(json["status"], "degraded");
    assert_eq!(json["degraded_by"][0], "12 MB free, below the 256 MB floor");

    server.stop().await;
}

#[tokio::test]
async fn an_unknown_route_is_a_404_rather_than_a_crash() {
    let server = Server::start(healthy()).await;
    let response = server.get("/results").await;
    let (status, _body) = split(&response);

    assert!(status.contains("404"), "got: {status}");

    // And the process is still serving afterwards, which is the part that matters: the
    // read path does not traverse this API, but a service that fell over on a stray request
    // would still be a service that needed restarting mid-event.
    let (status, _) = {
        let again = server.get("/health").await;
        let (status, body) = split(&again);
        (status.to_owned(), body.to_owned())
    };
    assert!(status.contains("200 OK"), "got: {status}");

    server.stop().await;
}

#[tokio::test]
async fn the_endpoint_survives_being_polled() {
    // Health is polled, forever, and hardest when the device is already struggling. This is
    // not a benchmark — it is a check that repeated connections do not leak the socket or
    // wedge the accept loop.
    let server = Server::start(healthy()).await;

    for _ in 0..50 {
        let response = server.get("/health").await;
        let (status, _) = split(&response);
        assert!(status.contains("200 OK"), "got: {status}");
    }

    server.stop().await;
}

#[tokio::test]
async fn the_endpoint_is_a_file_rather_than_an_address() {
    let server = Server::start(healthy()).await;

    let metadata = std::fs::metadata(&server.path).expect("stat");
    assert!(
        !metadata.is_dir() && !metadata.is_file(),
        "a bound Unix socket is neither a directory nor a regular file"
    );

    server.stop().await;
}

#[test]
fn this_crate_contains_no_tcp_listener() {
    // ADR-0021's central claim, enforced the way ADR-0012 enforces the dependency rules:
    // by a test rather than by review. The decision is not "we currently bind a socket" —
    // it is "there is no code path that binds a port", because a loopback listener added
    // for a console is one line from a LAN listener and nobody reviews that line as a
    // security decision.
    //
    // Deliberately reads the source rather than checking behavior. A TCP listener behind a
    // feature flag, a config option, or an `if` would pass every runtime test in this file
    // and still be the thing this ADR forbids.
    let mut offenders = Vec::new();

    for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src")).expect("read src") {
        let path = entry.expect("entry").path();
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read source");

        for (number, line) in source.lines().enumerate() {
            // Comments are where the decision gets explained, so they are not evidence of
            // it being broken.
            let code = line.split("//").next().unwrap_or("").trim();
            let banned = ["TcpListener", "SocketAddr", "0.0.0.0", "127.0.0.1"];
            if let Some(found) = banned.iter().find(|needle| code.contains(**needle)) {
                offenders.push(format!(
                    "{}:{}: {found} in `{code}`",
                    path.file_name().expect("name").to_string_lossy(),
                    number + 1
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "splitforge-api must never bind a port (ADR-0021), but found:\n  {}",
        offenders.join("\n  ")
    );
}
