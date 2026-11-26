//! Following a run's logs while it runs, on
//! `v1/applications/{workspace_id}/runs/{run_id}/logs/live`. No tool serves
//! it: it hands each entry to a callback as it arrives.

use std::time::Duration;

use serde::Deserialize;

use super::Platform;
use crate::decision::path::api_path;
use crate::decision::polling::Poller;
use crate::decision::types::or_unknown;
use crate::decision::{PollingOptions, RunStatus, TimestampedRunLog};
use crate::{Error, Result};

/// Parameters for [`Platform::follow_run_logs`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunLogsFollowParams {
    /// The waits between requests when the platform does not say how long
    /// to wait, and the limits of the whole follow. With the defaults,
    /// following has no time or try limit.
    pub polling: PollingOptions,
}

/// A response of `GET …/logs/live`.
#[derive(Debug, Deserialize)]
struct LiveLogs {
    #[serde(default)]
    items: Vec<TimestampedRunLog>,
    #[serde(default, deserialize_with = "or_unknown")]
    status_v2: RunStatus,
    /// Seconds to wait before the next request.
    #[serde(default)]
    next_available_in_seconds: Option<f64>,
    /// The `since` of the next request: the timestamp of the last entry
    /// sent so far.
    #[serde(default)]
    next_page_token: Option<String>,
}

impl Platform {
    /// Follows the logs of a run until it reaches a terminal state, handing
    /// each new entry to `on_entry` as it arrives, in order, on the calling
    /// task. Returns every entry, sorted by timestamp.
    ///
    /// Between requests it waits as long as the platform asks, and with the
    /// backoff of `params.polling` when the platform does not say. Once the
    /// run is done, it fetches the entries that the platform still has
    /// right away, without waiting. Following has no limit unless
    /// `max_duration` or `max_tries` is set, and a status this SDK does not
    /// know is not terminal, so if the platform adds a final status,
    /// following goes on until a limit or the caller's own timeout stops
    /// it. Dropping the future stops following.
    ///
    /// Each entry goes out once: the platform can send an entry again in
    /// the next response, which this skips.
    ///
    /// The platform only has the logs of runs created in the last 24 hours.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Wait`] if following runs out of time or tries, also
    /// while it fetches the entries left once the run is done (the reason
    /// says so then), and [`Error::Invalid`] before any request if a
    /// duration in `params.polling` is not valid. A failed request returns
    /// the [errors of every method](Platform#errors), such as
    /// [`Error::Status`].
    pub async fn follow_run_logs<F>(
        &self,
        workspace_id: &str,
        run_id: &str,
        params: RunLogsFollowParams,
        mut on_entry: F,
    ) -> Result<Vec<TimestampedRunLog>>
    where
        F: FnMut(&TimestampedRunLog) + Send,
    {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "runs",
            run_id,
            "logs",
            "live",
        ])?;
        let mut poller = Poller::start(&params.polling).await?;
        let mut since: Option<String> = None;
        let mut entries = Vec::new();
        // The entries handed over that are later than `since`. The platform
        // orders a response's entries as strings and takes the token from
        // the last one, so with fractions of different lengths, `since` can
        // be earlier than entries it already sent, and it sends them again.
        let mut recent: Vec<TimestampedRunLog> = Vec::new();
        loop {
            let mut request = self.client.get(&path);
            if let Some(since) = &since {
                request = request.query(&[("since", since)]);
            }
            let page: LiveLogs = request.send_json().await?;
            let earlier = recent.len();
            for entry in page.items {
                if recent[..earlier].contains(&entry) {
                    continue;
                }
                on_entry(&entry);
                recent.push(entry.clone());
                entries.push(entry);
            }
            // An empty token would send `since` as the run's start, and the
            // platform would send every entry again.
            let token = page.next_page_token.filter(|token| !token.is_empty());
            let terminal = page.status_v2.is_terminal();
            // A done run's response has a token only when it holds back
            // entries, which the next requests fetch.
            if terminal && token.is_none() {
                break;
            }
            if let Some(token) = token {
                // The platform sends again only what is later than `since`.
                recent.retain(|entry| sort_key(&entry.timestamp) > sort_key(&token));
                since = Some(token);
            }
            let hint = if terminal {
                Some(Duration::ZERO)
            } else {
                page.next_available_in_seconds
                    .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
                    .filter(|wait| !wait.is_zero())
            };
            poller.wait(hint).await.map_err(|err| match err {
                Error::Wait { .. } if terminal => Error::Wait {
                    reason: "the run ended, but fetching the rest of its logs used up \
                        `max_duration` or `max_tries`"
                        .to_owned(),
                },
                err => err,
            })?;
        }
        entries.sort_by(|a, b| sort_key(&a.timestamp).cmp(&sort_key(&b.timestamp)));
        Ok(entries)
    }
}

/// The parts of an RFC 3339 `timestamp` that order it: the date and time
/// to the second, the fraction of a second in nanoseconds, and the offset.
///
/// The platform writes Go's `RFC3339Nano`, which drops the trailing zeros of
/// the fraction, so `…:05Z` and `…:05.5Z` don't sort as strings. A
/// timestamp without the date and time sorts by its whole text.
fn sort_key(timestamp: &str) -> (&str, u32, &str) {
    let Some((seconds, rest)) = timestamp.split_at_checked(19) else {
        return (timestamp, 0, "");
    };
    let Some(fraction) = rest.strip_prefix('.') else {
        return (seconds, 0, rest);
    };
    let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
    let (fraction, offset) = fraction.split_at(digits);
    let nanos = fraction
        .bytes()
        .chain(std::iter::repeat(b'0'))
        .take(9)
        .fold(0, |nanos, digit| nanos * 10 + u32::from(digit - b'0'));
    (seconds, nanos, offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_fractions_by_their_value() {
        let whole = sort_key("2026-10-07T14:19:05Z");
        let half = sort_key("2026-10-07T14:19:05.5Z");
        let nanos = sort_key("2026-10-07T14:19:05.000000001Z");
        assert_eq!(whole, ("2026-10-07T14:19:05", 0, "Z"));
        assert_eq!(half, ("2026-10-07T14:19:05", 500_000_000, "Z"));
        assert!(whole < nanos && nanos < half);
        assert!(half < sort_key("2026-10-07T14:19:06Z"));
        assert_eq!(
            sort_key("2026-10-07T14:19:05.1234567891+02:00"),
            ("2026-10-07T14:19:05", 123_456_789, "+02:00")
        );
    }

    #[test]
    fn sorts_other_text_as_it_is() {
        assert_eq!(sort_key("yesterday"), ("yesterday", 0, ""));
        assert_eq!(sort_key(""), ("", 0, ""));
        // Not a character boundary at 19.
        assert_eq!(
            sort_key("2026-10-07T14:19:0é"),
            ("2026-10-07T14:19:0é", 0, "")
        );
    }
}
