// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Retrying transient failures, one way for every client.
//!
//! [`Policy`] and [`retry_blocking`] need nothing; the reqwest helpers
//! ([`send`], [`send_blocking`]) and the async [`retry`] come with the
//! `http` feature.
//!
//! Each API client used to carry its own retry loop, and they did not
//! agree on what was worth retrying: some retried every error, some only
//! 5xx, one only 5xx but not a dropped connection, and only one counted
//! a 429. A run then failed or succeeded by which client it happened to
//! go through — a rebuild report lost a package's pull requests to one
//! dropped dist-git request (#63).
//!
//! Transient here means a transport error (connect, reset, timeout, DNS)
//! or a 429, 500, 502, 503 or 504. Everything else is the caller's to
//! judge: a 404 often means "does not exist", not "try again".

use std::time::Duration;

/// How many times to try, and how long to wait between attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Policy {
    /// Attempts in all, the first included; at least one is made.
    pub attempts: u32,
    /// The wait before the first retry, doubled for each after it.
    pub base: Duration,
    /// The longest wait between attempts, a `Retry-After` included.
    pub cap: Duration,
}

impl Policy {
    /// Four attempts, waiting 1, 2 and 4 s between them.
    pub const DEFAULT: Policy = Policy {
        attempts: 4,
        base: Duration::from_secs(1),
        cap: Duration::from_secs(60),
    };

    /// `attempts` attempts with no wait between them, for tests.
    pub const fn immediate(attempts: u32) -> Policy {
        Policy {
            attempts,
            base: Duration::ZERO,
            cap: Duration::ZERO,
        }
    }

    /// A policy with `attempts` attempts and the default waits.
    pub const fn attempts(attempts: u32) -> Policy {
        Policy {
            attempts,
            ..Policy::DEFAULT
        }
    }

    /// The wait before retry number `retry` (1 for the first), or the
    /// server's `Retry-After` when it asked for longer; never over
    /// [`cap`](Self::cap).
    pub fn delay(&self, retry: u32, retry_after: Option<Duration>) -> Duration {
        let backoff = self
            .base
            .saturating_mul(2u32.saturating_pow(retry.saturating_sub(1)));
        backoff.max(retry_after.unwrap_or_default()).min(self.cap)
    }
}

impl Default for Policy {
    fn default() -> Self {
        Policy::DEFAULT
    }
}

/// Whether a response status is worth another attempt: 429 and the
/// gateway-style 5xx (500, 502, 503, 504).
#[cfg(feature = "http")]
pub fn is_transient(status: reqwest::StatusCode) -> bool {
    use reqwest::StatusCode as S;
    matches!(
        status,
        S::TOO_MANY_REQUESTS
            | S::INTERNAL_SERVER_ERROR
            | S::BAD_GATEWAY
            | S::SERVICE_UNAVAILABLE
            | S::GATEWAY_TIMEOUT
    )
}

/// A `Retry-After` given in seconds. The HTTP-date form is ignored,
/// which only means the policy's own backoff is used.
#[cfg(feature = "http")]
pub fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
        .map(Duration::from_secs)
}

/// Send the request `build` makes, retrying transient failures under
/// `policy`. A transport error that outlasts the attempts is returned;
/// a transient status that does is returned as the last response, so
/// the caller judges it like any other status. `label` names the
/// request in the retry messages on stderr.
#[cfg(feature = "http")]
pub async fn send<F>(label: &str, policy: &Policy, build: F) -> reqwest::Result<reqwest::Response>
where
    F: Fn() -> reqwest::RequestBuilder,
{
    let attempts = policy.attempts.max(1);
    let mut retry = 0;
    loop {
        let (problem, after) = match build().send().await {
            Ok(resp) if retry + 1 < attempts && is_transient(resp.status()) => (
                format!("HTTP {}", resp.status()),
                retry_after(resp.headers()),
            ),
            Ok(resp) => return Ok(resp),
            Err(e) if retry + 1 < attempts => (e.to_string(), None),
            Err(e) => return Err(e),
        };
        retry += 1;
        let wait = policy.delay(retry, after);
        announce(label, &problem, wait, retry, attempts);
        tokio::time::sleep(wait).await;
    }
}

/// [`send`] for a blocking client.
#[cfg(feature = "http")]
pub fn send_blocking<F>(
    label: &str,
    policy: &Policy,
    build: F,
) -> reqwest::Result<reqwest::blocking::Response>
where
    F: Fn() -> reqwest::blocking::RequestBuilder,
{
    let attempts = policy.attempts.max(1);
    let mut retry = 0;
    loop {
        let (problem, after) = match build().send() {
            Ok(resp) if retry + 1 < attempts && is_transient(resp.status()) => (
                format!("HTTP {}", resp.status()),
                retry_after(resp.headers()),
            ),
            Ok(resp) => return Ok(resp),
            Err(e) if retry + 1 < attempts => (e.to_string(), None),
            Err(e) => return Err(e),
        };
        retry += 1;
        let wait = policy.delay(retry, after);
        announce(label, &problem, wait, retry, attempts);
        std::thread::sleep(wait);
    }
}

/// Run `op` until it succeeds, its error is not `transient`, or the
/// attempts run out, and return its last result. For what is not a
/// single reqwest request: a Koji XML-RPC call, a login flow.
#[cfg(feature = "http")]
pub async fn retry<T, E, F, Fut>(
    label: &str,
    policy: &Policy,
    transient: impl Fn(&E) -> bool,
    mut op: F,
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let attempts = policy.attempts.max(1);
    let mut retry = 0;
    loop {
        match op().await {
            Err(e) if retry + 1 < attempts && transient(&e) => {
                retry += 1;
                let wait = policy.delay(retry, None);
                announce(label, &e.to_string(), wait, retry, attempts);
                tokio::time::sleep(wait).await;
            }
            done => return done,
        }
    }
}

/// [`retry`] for a blocking operation.
pub fn retry_blocking<T, E, F>(
    label: &str,
    policy: &Policy,
    transient: impl Fn(&E) -> bool,
    mut op: F,
) -> Result<T, E>
where
    F: FnMut() -> Result<T, E>,
    E: std::fmt::Display,
{
    let attempts = policy.attempts.max(1);
    let mut retry = 0;
    loop {
        match op() {
            Err(e) if retry + 1 < attempts && transient(&e) => {
                retry += 1;
                let wait = policy.delay(retry, None);
                announce(label, &e.to_string(), wait, retry, attempts);
                std::thread::sleep(wait);
            }
            done => return done,
        }
    }
}

/// Say on stderr what failed and when the next attempt comes.
fn announce(label: &str, problem: &str, wait: Duration, retry: u32, attempts: u32) {
    eprintln!(
        "{label}: {problem}; retrying in {}s ({retry}/{})",
        wait.as_secs_f32(),
        attempts - 1
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_double_and_respect_retry_after_within_the_cap() {
        let p = Policy::DEFAULT;
        assert_eq!(p.delay(1, None), Duration::from_secs(1));
        assert_eq!(p.delay(2, None), Duration::from_secs(2));
        assert_eq!(p.delay(3, None), Duration::from_secs(4));
        // A server asking for longer gets it, up to the cap.
        assert_eq!(
            p.delay(1, Some(Duration::from_secs(30))),
            Duration::from_secs(30)
        );
        assert_eq!(
            p.delay(1, Some(Duration::from_secs(600))),
            Duration::from_secs(60)
        );
        assert_eq!(
            Policy::immediate(3).delay(2, Some(Duration::from_secs(5))),
            Duration::ZERO
        );
    }

    #[test]
    fn blocking_operations_retry_what_the_caller_calls_transient() {
        let mut tries = 0;
        let r: Result<u32, String> = retry_blocking(
            "op",
            &Policy::immediate(3),
            |_| true,
            || {
                tries += 1;
                if tries < 3 {
                    Err("busy".into())
                } else {
                    Ok(tries)
                }
            },
        );
        assert_eq!(r.unwrap(), 3);
        // An error the caller does not call transient ends it at once.
        let mut calls = 0;
        let r: Result<(), String> = retry_blocking(
            "op",
            &Policy::immediate(5),
            |e: &String| e == "busy",
            || {
                calls += 1;
                Err("fault".into())
            },
        );
        assert_eq!((r.unwrap_err().as_str(), calls), ("fault", 1));
    }

    #[cfg(feature = "http")]
    mod http {
        use super::super::*;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        #[test]
        fn only_rate_limits_and_gateway_errors_are_transient() {
            for code in [429, 500, 502, 503, 504] {
                assert!(
                    is_transient(reqwest::StatusCode::from_u16(code).unwrap()),
                    "{code}"
                );
            }
            for code in [200, 400, 401, 403, 404, 501] {
                assert!(
                    !is_transient(reqwest::StatusCode::from_u16(code).unwrap()),
                    "{code}"
                );
            }
        }

        #[test]
        fn retry_after_is_read_in_seconds_only() {
            let mut h = reqwest::header::HeaderMap::new();
            assert_eq!(retry_after(&h), None);
            h.insert(reqwest::header::RETRY_AFTER, "7".parse().unwrap());
            assert_eq!(retry_after(&h), Some(Duration::from_secs(7)));
            h.insert(
                reqwest::header::RETRY_AFTER,
                "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
            );
            assert_eq!(retry_after(&h), None);
        }

        /// A server answering `/flaky` with each status in turn, then 200.
        async fn flaky(statuses: &[u16]) -> MockServer {
            let server = MockServer::start().await;
            for (i, code) in statuses.iter().enumerate() {
                Mock::given(method("GET"))
                    .and(path("/flaky"))
                    .respond_with(ResponseTemplate::new(*code))
                    .up_to_n_times(1)
                    .with_priority(u8::try_from(i + 1).unwrap())
                    .mount(&server)
                    .await;
            }
            Mock::given(method("GET"))
                .and(path("/flaky"))
                .respond_with(ResponseTemplate::new(200))
                .with_priority(100)
                .mount(&server)
                .await;
            server
        }

        #[tokio::test]
        async fn transient_statuses_are_retried_until_they_clear() {
            let server = flaky(&[503, 429]).await;
            crate::install_crypto_provider();
            let http = reqwest::Client::new();
            let url = format!("{}/flaky", server.uri());
            let resp = send("flaky", &Policy::immediate(4), || http.get(&url))
                .await
                .unwrap();
            assert_eq!(resp.status(), 200);
        }

        #[tokio::test]
        async fn the_last_transient_status_is_returned_not_turned_into_an_error() {
            let server = flaky(&[503, 503, 503]).await;
            crate::install_crypto_provider();
            let http = reqwest::Client::new();
            let url = format!("{}/flaky", server.uri());
            let resp = send("flaky", &Policy::immediate(2), || http.get(&url))
                .await
                .unwrap();
            assert_eq!(resp.status(), 503);
        }

        #[tokio::test]
        async fn a_404_is_the_caller_s_and_not_retried() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(404))
                .expect(1)
                .mount(&server)
                .await;
            crate::install_crypto_provider();
            let http = reqwest::Client::new();
            let url = format!("{}/x", server.uri());
            let resp = send("x", &Policy::immediate(4), || http.get(&url))
                .await
                .unwrap();
            assert_eq!(resp.status(), 404);
        }

        #[tokio::test]
        async fn transport_errors_are_retried_and_then_returned() {
            // Nothing listens on port 1.
            crate::install_crypto_provider();
            let http = reqwest::Client::new();
            let err = send("down", &Policy::immediate(3), || {
                http.get("http://127.0.0.1:1/")
            })
            .await
            .unwrap_err();
            assert!(err.is_connect() || err.is_request(), "{err}");
        }

        #[test]
        fn blocking_sends_retry_too() {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let server = rt.block_on(flaky(&[502]));
            let url = format!("{}/flaky", server.uri());
            let status = std::thread::spawn(move || {
                crate::install_crypto_provider();
                let http = reqwest::blocking::Client::new();
                send_blocking("flaky", &Policy::immediate(3), || http.get(&url))
                    .unwrap()
                    .status()
            })
            .join()
            .unwrap();
            assert_eq!(status, 200);
        }

        #[tokio::test]
        async fn the_caller_decides_which_errors_are_worth_retrying() {
            let mut calls = 0;
            let r: Result<(), String> = retry(
                "op",
                &Policy::immediate(5),
                |e: &String| e == "busy",
                || {
                    calls += 1;
                    let n = calls;
                    async move { Err(if n < 3 { "busy".into() } else { "fault".into() }) }
                },
            )
            .await;
            // Two busy answers retried; the fault ends it.
            assert_eq!((r.unwrap_err().as_str(), calls), ("fault", 3));
        }
    }
}
