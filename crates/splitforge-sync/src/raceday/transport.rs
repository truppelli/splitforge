//! Sending to RaceDay Connect over HTTPS ([ADR-0046](../../../../docs/adr/0046-raceday-connect-is-reached-with-ureq-and-rustls-from-a-process-of-its-own.md)).
//!
//! ureq, blocking, with rustls and the ring provider, trusting the system's certificate store.
//! Nothing is pinned and verification cannot be switched off. Every request is bounded: 5 s to
//! connect, 15 s in all, and no redirect is followed.
//!
//! A write never fails here. It comes back as a [`delivery::Outcome`](super::delivery::Outcome)
//! with what happened, because the shipper's only question is what to do next. A failure with
//! no status to classify, a refused connection, a timeout or a certificate the system does not
//! trust, is [`Outcome::Retry`]. A Pi whose clock has not synchronised checks certificates
//! against the wrong day, and a captive portal on race-day Wi-Fi presents a certificate nobody
//! trusts, and neither is a reason to throw results away. A failed handshake sends nothing, so
//! retrying never sends to a server that has not proved it is RaceDay Connect.

use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

use super::contract::{CrossingsBatch, Manifest, PairRequest, PairResponse, ResultsRevision};
use super::delivery::{Outcome, classify};

/// How long to wait for a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a whole request may take. A starting point, not a measurement (ADR-0046).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Why a client could not be made, or pairing did not complete.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// The address is not one this client will send a token to.
    #[error("{0}")]
    Address(String),
    /// Pairing reached RaceDay Connect and was refused.
    #[error("RaceDay Connect refused the pairing code (HTTP {status}): {body}")]
    PairingRefused {
        /// The status it answered with.
        status: u16,
        /// What it said, as it said it.
        body: String,
    },
    /// Pairing did not reach RaceDay Connect, or its answer could not be read.
    #[error("pairing did not complete: {0}")]
    Pairing(String),
}

/// What a write came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    /// What to do with the message now.
    pub outcome: Outcome,
    /// The status and body, or the transport's error, for the operator and the log.
    pub detail: String,
}

/// A client for one RaceDay Connect instance.
#[derive(Debug, Clone)]
pub struct Client {
    agent: ureq::Agent,
    base: String,
}

impl Client {
    /// A client for `base_url`, such as `https://raceday.example`.
    ///
    /// # Errors
    ///
    /// [`TransportError::Address`] unless the address is `https://`, or `http://` to this
    /// machine. A bearer token is never sent unencrypted across a network.
    pub fn new(base_url: &str) -> Result<Self, TransportError> {
        let base = base_url.trim_end_matches('/').to_owned();
        let allowed = base.starts_with("https://")
            || ["http://127.0.0.1", "http://localhost", "http://[::1]"]
                .iter()
                .any(|local| {
                    base.strip_prefix(local)
                        .is_some_and(|rest| rest.is_empty() || rest.starts_with([':', '/']))
                });
        if !allowed {
            return Err(TransportError::Address(format!(
                "{base_url:?} is not an https:// address; a token is only sent over TLS, or to \
                 this machine"
            )));
        }

        // Installed once per process. A second install fails because one is already in place,
        // which is the state wanted.
        let _ = rustls::crypto::ring::default_provider().install_default();

        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(Some(REQUEST_TIMEOUT))
            .max_redirects(0)
            .http_status_as_error(false)
            .tls_config(
                TlsConfig::builder()
                    .provider(TlsProvider::Rustls)
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .build()
            .into();
        Ok(Self { agent, base })
    }

    /// Trades a pairing code for this box's token. Run by an operator, so it fails rather than
    /// retrying: the code expires, and the person holding it is watching.
    ///
    /// # Errors
    ///
    /// [`TransportError::PairingRefused`] for any answer but 2xx, and
    /// [`TransportError::Pairing`] when there was no answer or it could not be read.
    pub fn pair(&self, request: &PairRequest) -> Result<PairResponse, TransportError> {
        let body = json(request).map_err(TransportError::Pairing)?;
        let mut response = self
            .agent
            .post(format!("{}/api/v1/timing/pair", self.base))
            .content_type("application/json")
            .send(body.as_bytes())
            .map_err(|error| TransportError::Pairing(error.to_string()))?;
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|error| TransportError::Pairing(error.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(TransportError::PairingRefused { status, body: text });
        }
        parse(&text).map_err(TransportError::Pairing)
    }

    /// Replaces the course for RaceDay Connect race `race`.
    #[must_use]
    pub fn put_manifest(&self, token: &str, race: &str, manifest: &Manifest) -> Sent {
        self.write("PUT", token, &format!("races/{race}/manifest"), manifest)
    }

    /// Sends a batch of crossings, each runner restated whole (ADR-0039).
    #[must_use]
    pub fn post_crossings(&self, token: &str, race: &str, batch: &CrossingsBatch) -> Sent {
        self.write("POST", token, &format!("races/{race}/crossings"), batch)
    }

    /// Sends one result revision, whole.
    #[must_use]
    pub fn post_results(&self, token: &str, race: &str, revision: &ResultsRevision) -> Sent {
        self.write("POST", token, &format!("races/{race}/results"), revision)
    }

    fn write(&self, method: &str, token: &str, path: &str, body: &impl Serialize) -> Sent {
        let body = match json(body) {
            Ok(body) => body,
            // Cannot happen for these types, and if it did, resending the same value would fail
            // the same way.
            Err(error) => {
                return Sent {
                    outcome: Outcome::Refused,
                    detail: error,
                };
            }
        };
        let url = format!("{}/api/v1/timing/{path}", self.base);
        let authorization = format!("Bearer {token}");
        let sent = if method == "PUT" {
            self.agent
                .put(&url)
                .header("Authorization", &authorization)
                .content_type("application/json")
                .send(body.as_bytes())
        } else {
            self.agent
                .post(&url)
                .header("Authorization", &authorization)
                .content_type("application/json")
                .send(body.as_bytes())
        };

        match sent {
            Ok(mut response) => {
                let status = response.status().as_u16();
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.trim().parse::<u64>().ok());
                let text = response.body_mut().read_to_string().unwrap_or_default();
                Sent {
                    outcome: classify(status, retry_after),
                    detail: format!("HTTP {status} {text}").trim_end().to_owned(),
                }
            }
            Err(error) => Sent {
                outcome: Outcome::Retry { after: None },
                detail: error.to_string(),
            },
        }
    }
}

fn json(value: &impl Serialize) -> Result<String, String> {
    serde_json::to_string(value).map_err(|error| format!("could not encode the request: {error}"))
}

fn parse<T: DeserializeOwned>(text: &str) -> Result<T, String> {
    serde_json::from_str(text).map_err(|error| format!("could not read the answer: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// One request as the server saw it.
    #[derive(Debug)]
    struct Seen {
        method: String,
        path: String,
        authorization: Option<String>,
        body: String,
    }

    /// A loopback server that answers each request it gets with the next canned response, and
    /// reports what it saw. Plain HTTP, which `Client::new` allows only to this machine.
    fn serve(responses: Vec<String>) -> (String, mpsc::Receiver<Seen>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let (seen, requests) = mpsc::channel();
        std::thread::spawn(move || {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                reader.read_line(&mut line).expect("request line");
                let mut parts = line.split_whitespace();
                let method = parts.next().unwrap_or_default().to_owned();
                let path = parts.next().unwrap_or_default().to_owned();
                let (mut length, mut authorization) = (0, None);
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).expect("header");
                    let header = header.trim_end();
                    if header.is_empty() {
                        break;
                    }
                    let (name, value) = header.split_once(':').expect("a header");
                    match name.to_ascii_lowercase().as_str() {
                        "content-length" => length = value.trim().parse().expect("a length"),
                        "authorization" => authorization = Some(value.trim().to_owned()),
                        _ => {}
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).expect("body");
                let _ = seen.send(Seen {
                    method,
                    path,
                    authorization,
                    body: String::from_utf8(body).expect("utf-8"),
                });
                stream.write_all(response.as_bytes()).expect("respond");
            }
        });
        (base, requests)
    }

    fn answer(status: &str, headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
            body.len()
        )
    }

    fn batch() -> CrossingsBatch {
        CrossingsBatch {
            crossings: Vec::new(),
            replaces: Vec::new(),
        }
    }

    #[test]
    fn pairing_posts_the_code_and_returns_the_token() {
        let (base, seen) = serve(vec![answer(
            "200 OK",
            "",
            r#"{"raceId":"r1","raceName":"Spring 5K","slug":"spring-5k","token":"rdc_abc"}"#,
        )]);
        let paired = Client::new(&base)
            .expect("client")
            .pair(&PairRequest {
                code: "K7QM-4XPD".to_owned(),
                device_name: None,
            })
            .expect("paired");

        assert_eq!(paired.token, "rdc_abc");
        assert_eq!(paired.race_id, "r1");
        let request = seen.recv().expect("a request");
        assert_eq!(
            (request.method.as_str(), request.path.as_str()),
            ("POST", "/api/v1/timing/pair")
        );
        assert_eq!(
            request.authorization, None,
            "pairing is how a token is obtained"
        );
        assert_eq!(request.body, r#"{"code":"K7QM-4XPD"}"#);
    }

    #[test]
    fn a_refused_pairing_code_is_an_error_with_what_the_server_said() {
        let (base, _seen) = serve(vec![answer("404 Not Found", "", "no such code")]);
        let error = Client::new(&base)
            .expect("client")
            .pair(&PairRequest {
                code: "WRONG".to_owned(),
                device_name: None,
            })
            .expect_err("refused");
        assert!(
            matches!(&error, TransportError::PairingRefused { status: 404, body } if body == "no such code"),
            "{error}"
        );
    }

    #[test]
    fn a_write_carries_the_token_to_the_race_path_and_is_delivered() {
        let (base, seen) = serve(vec![answer("200 OK", "", r#"{"accepted":0}"#)]);
        let sent = Client::new(&base)
            .expect("client")
            .post_crossings("rdc_abc", "r1", &batch());

        assert_eq!(sent.outcome, Outcome::Delivered, "{}", sent.detail);
        let request = seen.recv().expect("a request");
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/v1/timing/races/r1/crossings");
        assert_eq!(request.authorization.as_deref(), Some("Bearer rdc_abc"));
    }

    #[test]
    fn each_answer_is_classified_as_adr_0039_says() {
        for (response, outcome) in [
            (
                answer("503 Service Unavailable", "Retry-After: 7\r\n", "busy"),
                Outcome::Retry {
                    after: Some(Duration::from_secs(7)),
                },
            ),
            (answer("401 Unauthorized", "", ""), Outcome::Unpaired),
            (
                answer("409 Conflict", "", "older revision"),
                Outcome::Refused,
            ),
            (
                answer("302 Found", "Location: /elsewhere\r\n", ""),
                Outcome::Retry { after: None },
            ),
        ] {
            let (base, seen) = serve(vec![response.clone()]);
            let sent =
                Client::new(&base)
                    .expect("client")
                    .post_crossings("rdc_abc", "r1", &batch());
            assert_eq!(sent.outcome, outcome, "{response:?}: {}", sent.detail);
            seen.recv().expect("one request");
            assert!(
                seen.recv_timeout(Duration::from_millis(200)).is_err(),
                "nothing followed: {response:?}"
            );
        }
    }

    #[test]
    fn no_answer_at_all_is_retried() {
        // A port nothing listens on: the transport fails, and there is no status to classify.
        let base = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            format!("http://{}", listener.local_addr().expect("address"))
        };
        let sent = Client::new(&base)
            .expect("client")
            .post_crossings("rdc_abc", "r1", &batch());
        assert_eq!(sent.outcome, Outcome::Retry { after: None });
        assert!(!sent.detail.is_empty());
    }

    #[test]
    fn a_token_is_sent_only_over_tls_or_to_this_machine() {
        for allowed in [
            "https://raceday.example",
            "https://raceday.example/",
            "http://127.0.0.1:8080",
            "http://localhost:5000",
            "http://[::1]:5000",
        ] {
            assert!(Client::new(allowed).is_ok(), "{allowed}");
        }
        for refused in [
            "http://raceday.example",
            "http://localhost.attacker.example",
            "http://127.0.0.1.attacker.example",
            "ftp://raceday.example",
            "raceday.example",
        ] {
            assert!(
                matches!(Client::new(refused), Err(TransportError::Address(_))),
                "{refused}"
            );
        }
    }
}
