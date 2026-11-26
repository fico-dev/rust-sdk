//! Retry policy: exponential backoff with jitter and `Retry-After` support.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::rand::random_fraction;
use crate::time::days_from_civil;
use crate::{Error, Method};

/// Controls whether and how failed requests are retried.
///
/// A request is retried when it fails with a connection error or a timeout,
/// or when the server responds with one of the retryable status codes, and
/// only if its method is one of the retryable methods. `POST` and `PATCH`
/// are not idempotent, and the server may already have processed a request
/// that timed out or got another status, so they are only retried on
/// connection errors, which include running out of the connect limit, and
/// on `429` or `503` responses.
///
/// Two budgets limit the retries of a request.
/// [`max_retries`](Self::max_retries) caps all of them, and
/// [`max_transport_retries`](Self::max_transport_retries) the retries after
/// an attempt that failed without a response: with [`Error::Connect`],
/// [`Error::Timeout`] or [`Error::Request`], such as a connection reset. So a
/// server that can't be reached fails fast, while one that answers `502`,
/// `503` or `504`, e.g. during a deploy, is retried for longer.
///
/// Before retry `n` (starting at 1), the client waits
/// `min(backoff_factor * 2^(n - 1), max_backoff) + random(0..=jitter)`. For
/// a retry after a failure without a response, `n` counts only those
/// retries, so they start again from `backoff_factor` even after many
/// retried responses. For a retry after a response, `n` counts all retries.
///
/// The server can ask for a wait of its own, which replaces the backoff: a
/// `429` response in its JSON body's `detail.retry_in_milliseconds`, else a
/// `429` or `503` response in its `Retry-After` header. The client then
/// waits that long plus jitter. If the server asks for longer than
/// `max_backoff`, the request is not retried and the response is returned as
/// [`Error::Status`]. The same goes for a `429` whose body sets
/// `detail.should_retry` to `false`.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use fico_sdk::{Method, RetryPolicy};
///
/// let policy = RetryPolicy::default()
///     .max_retries(3)
///     .backoff_factor(Duration::from_millis(500))
///     .statuses(&[429, 503])
///     .methods(&[Method::Get]);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicy {
    max_retries: u32,
    max_transport_retries: u32,
    backoff_factor: Duration,
    jitter: Duration,
    max_backoff: Duration,
    statuses: Vec<u16>,
    methods: Vec<Method>,
}

impl Default for RetryPolicy {
    /// Up to 10 retries, at most 2 of them after a failure without a
    /// response, with a 1s backoff factor, 100ms of jitter and a 60s cap, on
    /// status codes `429`, `502`, `503` and `504`, for `GET`, `POST`, `PUT`
    /// and `DELETE` requests.
    ///
    /// The client's time limits apply to each attempt, and only the
    /// optional whole-attempt limit bounds how long an attempt takes. With
    /// the client's default limits, a request to a server that refuses the
    /// connection, or to a host that doesn't resolve, fails in about 3s, and
    /// one to a host that drops packets in about 33s. A `GET` that keeps
    /// getting `502`, `503` or `504` takes its 11 attempts plus about 5
    /// minutes of backoff before failing. To bound the total time, wrap the
    /// call in [`tokio::time::timeout`]; dropping the future cancels the
    /// request.
    fn default() -> Self {
        Self {
            max_retries: 10,
            max_transport_retries: 2,
            backoff_factor: Duration::from_secs(1),
            jitter: Duration::from_millis(100),
            max_backoff: Duration::from_secs(60),
            statuses: vec![429, 502, 503, 504],
            methods: vec![Method::Get, Method::Post, Method::Put, Method::Delete],
        }
    }
}

impl RetryPolicy {
    /// A policy that never retries.
    pub fn none() -> Self {
        Self::default().max_retries(0)
    }

    /// Sets the maximum number of retries per request, of any kind, which
    /// also caps [`max_transport_retries`](Self::max_transport_retries). `0`
    /// disables retries.
    pub fn max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Sets the maximum number of retries per request after an attempt
    /// fails without a response: with [`Error::Connect`], [`Error::Timeout`]
    /// or [`Error::Request`]. Defaults to 2. These retries count towards
    /// [`max_retries`](Self::max_retries) too. `0` disables them, but not the
    /// retries after a response.
    pub fn max_transport_retries(mut self, max_transport_retries: u32) -> Self {
        self.max_transport_retries = max_transport_retries;
        self
    }

    /// Sets the base delay of the backoff, which doubles with every retry.
    /// See [`RetryPolicy`] for how retries are numbered.
    pub fn backoff_factor(mut self, backoff_factor: Duration) -> Self {
        self.backoff_factor = backoff_factor;
        self
    }

    /// Sets the upper bound of the random delay added to every wait. It is a
    /// fixed amount of time and does not grow with the backoff.
    pub fn jitter(mut self, jitter: Duration) -> Self {
        self.jitter = jitter;
        self
    }

    /// Sets the longest the client waits before a single retry, excluding
    /// jitter. A longer wait asked for by the server stops the retries.
    pub fn max_backoff(mut self, max_backoff: Duration) -> Self {
        self.max_backoff = max_backoff;
        self
    }

    /// Sets the response status codes that trigger a retry. For `POST` and
    /// `PATCH` requests, which the server may already have processed, codes
    /// other than `429` and `503` are ignored. A `429` whose body sets
    /// `detail.should_retry` to `false` is never retried.
    pub fn statuses(mut self, statuses: &[u16]) -> Self {
        self.statuses = statuses.to_vec();
        self
    }

    /// Sets the request methods that may be retried. `POST` and `PATCH` are
    /// only retried on connection errors and `429` or `503` responses, since
    /// the server may already have processed them.
    pub fn methods(mut self, methods: &[Method]) -> Self {
        self.methods = methods.to_vec();
        self
    }

    pub(crate) fn allows_method(&self, method: Method) -> bool {
        self.methods.contains(&method)
    }

    pub(crate) fn allows_status(&self, method: Method, status: u16) -> bool {
        self.statuses.contains(&status) && (is_idempotent(method) || matches!(status, 429 | 503))
    }

    pub(crate) fn allows_error(&self, method: Method, error: &Error) -> bool {
        error.is_retryable() && (is_idempotent(method) || matches!(error, Error::Connect { .. }))
    }

    /// Whether a response with a retryable status may still be retried, so
    /// that its body is only read when it can matter.
    pub(crate) fn has_status_retries(&self, retries: &Retries) -> bool {
        retries.all < self.max_retries
    }

    /// Counts a retry after a response with a retryable status, and returns
    /// the wait before it, or `None` if no retries are left. The server's
    /// `wait`, if any, replaces the backoff, as in [`delay`](Self::delay).
    pub(crate) fn retry_status(
        &self,
        retries: &mut Retries,
        wait: Option<Duration>,
    ) -> Option<Duration> {
        if !self.has_status_retries(retries) {
            return None;
        }
        let delay = self.delay(retries.all + 1, wait)?;
        retries.all += 1;
        Some(delay)
    }

    /// Counts a retry after an attempt that failed without a response, and
    /// returns the wait before it, or `None` if either budget has run out.
    pub(crate) fn retry_transport(&self, retries: &mut Retries) -> Option<Duration> {
        if retries.all >= self.max_retries || retries.transport >= self.max_transport_retries {
            return None;
        }
        // Counted first: the delay is for the retry being counted (1-based).
        retries.all += 1;
        retries.transport += 1;
        self.delay(retries.transport, None)
    }

    /// Delay before retry number `retry` (1-based). A server-provided
    /// `retry_after` replaces the computed backoff, or prevents the retry
    /// (`None`) if it is longer than `max_backoff`.
    fn delay(&self, retry: u32, retry_after: Option<Duration>) -> Option<Duration> {
        let base = match retry_after {
            Some(retry_after) if retry_after > self.max_backoff => return None,
            Some(retry_after) => retry_after,
            None => self.backoff(retry),
        };
        Some(base.saturating_add(self.jitter.mul_f64(random_fraction())))
    }

    /// Exponential backoff for retry number `retry` (1-based), without jitter.
    fn backoff(&self, retry: u32) -> Duration {
        let exponent = retry.saturating_sub(1).min(1023) as i32;
        let secs = self.backoff_factor.as_secs_f64() * 2f64.powi(exponent);
        if secs.is_finite() && secs < self.max_backoff.as_secs_f64() {
            Duration::from_secs_f64(secs)
        } else {
            self.max_backoff
        }
    }
}

/// The retries a request has made so far.
#[derive(Debug, Default)]
pub(crate) struct Retries {
    /// Every retry.
    all: u32,
    /// The retries after an attempt that failed without a response.
    transport: u32,
}

/// Whether repeating a request has the same effect as sending it once
/// (RFC 9110 §9.2.2).
fn is_idempotent(method: Method) -> bool {
    !matches!(method, Method::Post | Method::Patch)
}

/// What a `429` response's body says about retrying it. The platform sends
/// `{"detail": {"should_retry": <bool>, "retry_in_milliseconds": <int>}}`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct BusyDetail {
    /// `false` when retrying soon won't help, e.g. over a concurrency limit
    /// or a plan's monthly run cap.
    pub(crate) should_retry: Option<bool>,
    /// How long the server asks the client to wait.
    pub(crate) retry_in: Option<Duration>,
}

/// Reads the `detail` of a `429` response's body. Each field is read on its
/// own, so a malformed one doesn't lose the other, and a body that is not
/// JSON gives neither.
pub(crate) fn parse_busy_detail(body: &[u8]) -> BusyDetail {
    let Ok(body) = serde_json::from_slice::<Value>(body) else {
        return BusyDetail::default();
    };
    let detail = &body["detail"];
    BusyDetail {
        should_retry: detail["should_retry"].as_bool(),
        retry_in: detail["retry_in_milliseconds"]
            .as_u64()
            .map(Duration::from_millis),
    }
}

/// Parses a `Retry-After` header value, given either as delay seconds or as
/// an HTTP date (IMF-fixdate, e.g. `Sun, 06 Nov 1994 08:49:37 GMT`).
pub(crate) fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if let Ok(secs) = value.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let at = UNIX_EPOCH.checked_add(Duration::from_secs(parse_http_date(value)?))?;
    Some(at.duration_since(now).unwrap_or(Duration::ZERO))
}

/// Parses an IMF-fixdate into seconds since the Unix epoch. Years are
/// limited to 1970..=9999, which also keeps the arithmetic from overflowing.
fn parse_http_date(value: &str) -> Option<u64> {
    let mut parts = value.split_whitespace();
    let _weekday = parts.next()?.strip_suffix(',')?;
    let day: u64 = parts.next()?.parse().ok()?;
    let month = match parts.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: u64 = parts.next()?.parse().ok()?;
    let mut time = parts.next()?.split(':').map(|n| n.parse::<u64>().ok());
    let (hour, minute, second) = (time.next()??, time.next()??, time.next()??);
    if parts.next()? != "GMT" || parts.next().is_some() || time.next().is_some() {
        return None;
    }
    if !(1970..=9999).contains(&year)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let days = u64::try_from(days_from_civil(year as i64, month, day as i64)).ok()?;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> RetryPolicy {
        RetryPolicy::default()
            .backoff_factor(Duration::from_secs(1))
            .jitter(Duration::ZERO)
            .max_backoff(Duration::from_secs(60))
    }

    #[test]
    fn backoff_doubles_until_the_cap() {
        let policy = policy();
        let delays: Vec<u64> = (1..=8)
            .map(|n| policy.delay(n, None).unwrap().as_secs())
            .collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(policy.delay(u32::MAX, None), Some(Duration::from_secs(60)));
    }

    /// The waits `retry` returns until it says to stop.
    fn waits(mut retry: impl FnMut() -> Option<Duration>) -> Vec<u64> {
        std::iter::from_fn(|| retry().map(|wait| wait.as_secs())).collect()
    }

    #[test]
    fn allows_two_transport_retries_and_ten_in_total_by_default() {
        let policy = policy();
        let mut retries = Retries::default();
        assert_eq!(waits(|| policy.retry_transport(&mut retries)), [1, 2]);
        let mut retries = Retries::default();
        assert_eq!(
            waits(|| policy.retry_status(&mut retries, None)),
            [1, 2, 4, 8, 16, 32, 60, 60, 60, 60]
        );
    }

    #[test]
    fn numbers_transport_retries_on_their_own() {
        let policy = policy();
        let mut retries = Retries::default();
        let mut status = || policy.retry_status(&mut retries, None).unwrap().as_secs();
        let first: Vec<u64> = (0..6).map(|_| status()).collect();
        assert_eq!(first, [1, 2, 4, 8, 16, 32]);
        assert_eq!(waits(|| policy.retry_transport(&mut retries)), [1, 2]);
        // Retries after a response count every retry, so this is retry 9.
        assert_eq!(waits(|| policy.retry_status(&mut retries, None)), [60, 60]);
    }

    #[test]
    fn caps_transport_retries_by_max_retries() {
        let one = policy().max_retries(1).max_transport_retries(5);
        let mut retries = Retries::default();
        assert_eq!(waits(|| one.retry_transport(&mut retries)), [1]);

        let three = policy().max_retries(3);
        let mut retries = Retries::default();
        assert!(three.retry_status(&mut retries, None).is_some());
        assert!(three.retry_status(&mut retries, None).is_some());
        assert_eq!(waits(|| three.retry_transport(&mut retries)), [1]);
        assert_eq!(three.retry_status(&mut retries, None), None);

        let statuses_only = policy().max_transport_retries(0);
        let mut retries = Retries::default();
        assert_eq!(statuses_only.retry_transport(&mut retries), None);
        assert!(statuses_only.retry_status(&mut retries, None).is_some());
    }

    #[test]
    fn knows_when_status_retries_are_left() {
        let policy = policy().max_retries(1);
        let mut retries = Retries::default();
        assert!(policy.has_status_retries(&retries));
        assert!(policy.retry_status(&mut retries, None).is_some());
        assert!(!policy.has_status_retries(&retries));
        assert!(!RetryPolicy::none().has_status_retries(&Retries::default()));
    }

    #[test]
    fn stops_when_the_server_asks_for_too_long() {
        let policy = policy();
        let mut retries = Retries::default();
        let hour = Some(Duration::from_secs(3_600));
        assert_eq!(policy.retry_status(&mut retries, hour), None);
        assert_eq!(retries.all, 0);
    }

    #[test]
    fn zero_backoff_factor_never_waits() {
        let policy = policy().backoff_factor(Duration::ZERO);
        assert_eq!(policy.delay(1, None), Some(Duration::ZERO));
        assert_eq!(policy.delay(u32::MAX, None), Some(Duration::ZERO));
    }

    #[test]
    fn retry_after_replaces_backoff_unless_too_long() {
        let policy = policy();
        let five = Duration::from_secs(5);
        assert_eq!(policy.delay(1, Some(five)), Some(five));
        let minute = Duration::from_secs(60);
        assert_eq!(policy.delay(1, Some(minute)), Some(minute));
        let hour = Duration::from_secs(3_600);
        assert_eq!(policy.delay(1, Some(hour)), None);
    }

    #[test]
    fn jitter_stays_within_bounds() {
        let policy = policy().jitter(Duration::from_millis(100));
        for _ in 0..1_000 {
            let delay = policy.delay(1, None).unwrap();
            assert!(delay >= Duration::from_secs(1));
            assert!(delay <= Duration::from_millis(1_100));
        }
    }

    #[test]
    fn uncapped_backoff_does_not_overflow() {
        let policy = policy()
            .jitter(Duration::from_millis(100))
            .max_backoff(Duration::MAX);
        assert_eq!(policy.delay(100, None), Some(Duration::MAX));
    }

    #[test]
    fn parses_busy_details() {
        let body = br#"{"status":"Busy","error":"rate limit exceeded","detail":{"reason":"busy_api_limit","should_retry":true,"retry_in_milliseconds":1234}}"#;
        assert_eq!(
            parse_busy_detail(body),
            BusyDetail {
                should_retry: Some(true),
                retry_in: Some(Duration::from_millis(1234)),
            }
        );
        assert_eq!(
            parse_busy_detail(br#"{"detail":{"should_retry":false}}"#),
            BusyDetail {
                should_retry: Some(false),
                retry_in: None,
            }
        );
        assert_eq!(
            parse_busy_detail(br#"{"detail":{"should_retry":"no","retry_in_milliseconds":500}}"#),
            BusyDetail {
                should_retry: None,
                retry_in: Some(Duration::from_millis(500)),
            }
        );
        for wait in ["-1", "1.5", "\"500\"", "null"] {
            let body =
                format!(r#"{{"detail":{{"should_retry":true,"retry_in_milliseconds":{wait}}}}}"#);
            assert_eq!(
                parse_busy_detail(body.as_bytes()),
                BusyDetail {
                    should_retry: Some(true),
                    retry_in: None,
                },
                "{wait}"
            );
        }
    }

    #[test]
    fn ignores_bodies_without_busy_details() {
        for body in [
            "",
            "rate limit exceeded",
            "[]",
            r#"{"status":"Busy"}"#,
            r#"{"detail":null}"#,
            r#"{"detail":"busy"}"#,
        ] {
            assert_eq!(
                parse_busy_detail(body.as_bytes()),
                BusyDetail::default(),
                "{body}"
            );
        }
    }

    #[test]
    fn parses_retry_after_seconds() {
        let now = SystemTime::now();
        assert_eq!(
            parse_retry_after("120", now),
            Some(Duration::from_secs(120))
        );
        assert_eq!(parse_retry_after(" 0 ", now), Some(Duration::ZERO));
        assert_eq!(parse_retry_after("-1", now), None);
        assert_eq!(parse_retry_after("soon", now), None);
    }

    #[test]
    fn parses_retry_after_http_date() {
        let date = "Sun, 06 Nov 1994 08:49:37 GMT";
        assert_eq!(parse_http_date(date), Some(784_111_777));
        assert_eq!(parse_http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(
            parse_http_date("Tue, 29 Feb 2028 12:00:00 GMT"),
            Some(1_835_438_400)
        );

        let now = UNIX_EPOCH + Duration::from_secs(784_111_777 - 30);
        assert_eq!(parse_retry_after(date, now), Some(Duration::from_secs(30)));
        let later = UNIX_EPOCH + Duration::from_secs(784_111_777 + 30);
        assert_eq!(parse_retry_after(date, later), Some(Duration::ZERO));
        assert!(parse_retry_after("Fri, 31 Dec 9999 23:59:59 GMT", now).is_some());
        let huge = "Sun, 06 Nov 300000000000 08:49:37 GMT";
        assert_eq!(parse_retry_after(huge, now), None);
    }

    #[test]
    fn rejects_malformed_http_dates() {
        for value in [
            "",
            "Sun 06 Nov 1994 08:49:37 GMT",
            "Sun, 06 Foo 1994 08:49:37 GMT",
            "Sun, 06 Nov 1994 08:49 GMT",
            "Sun, 06 Nov 1994 08:49:37:00 GMT",
            "Sun, 06 Nov 1994 08:49:37 UTC",
            "Sun, 06 Nov 1994 08:49:37 GMT extra",
            "Sun, 32 Nov 1994 08:49:37 GMT",
            "Sun, 06 Nov 1994 24:00:00 GMT",
            "Sun, 06 Nov 1969 08:49:37 GMT",
            "Sun, 06 Nov 10000 08:49:37 GMT",
            "Sun, 06 Nov 300000000000 08:49:37 GMT",
        ] {
            assert_eq!(parse_http_date(value), None, "{value}");
        }
    }

    #[test]
    fn checks_methods_and_statuses() {
        let policy = RetryPolicy::default();
        assert!(policy.allows_method(Method::Get));
        assert!(!policy.allows_method(Method::Patch));
        assert!(policy.allows_status(Method::Get, 503));
        assert!(!policy.allows_status(Method::Get, 500));
        assert!(policy.allows_status(Method::Get, 502));
        assert!(!policy.allows_status(Method::Post, 502));
        assert!(policy.allows_status(Method::Post, 429));
        assert!(policy.allows_status(Method::Post, 503));
        let mut retries = Retries::default();
        assert_eq!(RetryPolicy::none().retry_status(&mut retries, None), None);
        assert_eq!(RetryPolicy::none().retry_transport(&mut retries), None);
    }
}
