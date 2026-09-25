//! What a RaceDay Connect response means for the message that was sent.
//!
//! The outbox has to decide, for every answer, whether to forget the message, send it
//! again, or stop trying. Getting that wrong fails in one of two quiet ways: a message
//! retried forever parks everything queued behind it, and a message dropped on a
//! transient error is results that never arrive. Neither affects timing (ADR-0006); both
//! affect what spectators are told.
//!
//! | Answer | Outcome | Why |
//! |---|---|---|
//! | 2xx | [`Outcome::Delivered`] | Stored, or already held (a replay is still delivered) |
//! | 401 | [`Outcome::Unpaired`] | The race director unpaired this box, or the token is wrong. Nothing will succeed until the operator pairs again |
//! | 408, 429, 5xx | [`Outcome::Retry`] | Transient; RaceDay Connect answers 503 with `Retry-After` when two batches race |
//! | any other 4xx | [`Outcome::Refused`] | The same bytes will get the same answer. Set aside and reported, never retried |
//!
//! A transport failure — no route, DNS, a reset connection — is [`Outcome::Retry`] too,
//! decided by the transport rather than here, because there is no status to classify.

use std::time::Duration;

/// What to do with a sent message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Done. Remove it from the outbox.
    Delivered,
    /// Send it again later, no sooner than `after` when the server named a time.
    Retry {
        /// The server's `Retry-After`, when it sent one.
        after: Option<Duration>,
    },
    /// This box is no longer paired. Stop publishing this race until it is paired again.
    Unpaired,
    /// Refused on its merits. Set it aside and tell the operator; do not resend it.
    Refused,
}

/// Classifies an HTTP status, with the `Retry-After` seconds if the response carried one.
#[must_use]
pub fn classify(status: u16, retry_after_secs: Option<u64>) -> Outcome {
    match status {
        200..=299 => Outcome::Delivered,
        401 => Outcome::Unpaired,
        408 | 429 | 500..=599 => Outcome::Retry {
            after: retry_after_secs.map(Duration::from_secs),
        },
        _ => Outcome::Refused,
    }
}

/// How long to wait before attempt `attempt` (counting from 1), when the server said nothing.
///
/// Doubles from one second and stops at a minute. A minute is the longest a spectator's
/// board should lag a restored connection; nothing is gained by waiting longer, because a
/// retry costs one request against a server that is either back or not.
#[must_use]
pub fn backoff(attempt: u32) -> Duration {
    const CAP_SECS: u64 = 60;
    let exponent = attempt.saturating_sub(1).min(6);
    Duration::from_secs((1_u64 << exponent).min(CAP_SECS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_and_replay_are_both_delivered() {
        assert_eq!(classify(200, None), Outcome::Delivered);
        assert_eq!(classify(204, None), Outcome::Delivered);
    }

    #[test]
    fn an_unpaired_box_stops_rather_than_retrying() {
        assert_eq!(classify(401, None), Outcome::Unpaired);
    }

    #[test]
    fn transient_answers_are_retried_and_honour_retry_after() {
        assert_eq!(
            classify(503, Some(1)),
            Outcome::Retry {
                after: Some(Duration::from_secs(1))
            }
        );
        assert_eq!(classify(500, None), Outcome::Retry { after: None });
        assert_eq!(classify(429, None), Outcome::Retry { after: None });
        assert_eq!(classify(408, None), Outcome::Retry { after: None });
    }

    #[test]
    fn a_refusal_on_the_merits_is_never_retried() {
        // 409: an older revision, or one number with two digests. 422: incomplete.
        // 413: too large. Resending the same bytes gets the same answer.
        for status in [400, 403, 404, 409, 413, 422] {
            assert_eq!(classify(status, None), Outcome::Refused, "{status}");
        }
    }

    #[test]
    fn backoff_doubles_and_stops_at_a_minute() {
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(2), Duration::from_secs(2));
        assert_eq!(backoff(6), Duration::from_secs(32));
        assert_eq!(backoff(7), Duration::from_secs(60));
        assert_eq!(backoff(u32::MAX), Duration::from_secs(60));
        assert_eq!(backoff(0), Duration::from_secs(1));
    }
}
