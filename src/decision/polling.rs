//! Polling a resource until it reaches a terminal state.
//!
//! The get operations with a `polling` field call [`poll`]. There are no
//! `wait_for_*` helpers: callers pass [`PollingOptions`] instead. Following
//! a run's logs writes its own loop with a [`Poller`], which waits as long
//! as the server asks.

use std::future::Future;
use std::time::Duration;

use tokio::time::{Instant, sleep};

use crate::decision::{ExperimentStatus, PollingOptions};
use crate::rand::random_fraction;
use crate::{Error, Result};

/// Calls `check` until it returns `Some`, waiting between calls as `options`
/// says, and returns that value. `check` returns `None` while the resource
/// has not reached a terminal state.
///
/// After the n-th check, it waits `delay + backoff * 2^n` seconds, capped at
/// `max_delay`, plus up to `jitter` seconds at random. Polling gives up
/// right after the last try that `max_tries` allows, with no wait after it.
/// `options` is only read, so callers can share it.
///
/// # Errors
///
/// Returns the first error that `check` returns, [`Error::Wait`] when
/// `options` runs out of time or tries, or [`Error::Invalid`] before any
/// check if a duration in `options` is negative, not finite or too large.
/// A request that gets no response ends polling once the client's retries ran
/// out, by default after about 3 seconds, although the change keeps going on
/// the platform.
pub(crate) async fn poll<T, F, Fut>(options: &PollingOptions, mut check: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Option<T>>>,
{
    let mut poller = Poller::start(options).await?;
    loop {
        if let Some(value) = check().await? {
            return Ok(value);
        }
        poller.wait(None).await?;
    }
}

/// What a check of a background change finds: the change is done, it is
/// still going, or the platform reports that it failed, and why.
pub(crate) enum Change {
    /// The change is done.
    Done,
    /// The change is still going.
    Pending,
    /// The platform reports that the change failed.
    Failed(String),
}

/// Polls `check` as [`poll`] does, until the change it checks is done.
///
/// # Errors
///
/// Returns [`Error::Wait`] when the platform reports that the change failed,
/// or when `options` runs out of time or tries, and the first error that
/// `check` returns.
pub(crate) async fn poll_change<F, Fut>(options: &PollingOptions, mut check: F) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Change>>,
{
    poll(options, || {
        let check = check();
        async move {
            match check.await? {
                Change::Done => Ok(Some(())),
                Change::Pending => Ok(None),
                Change::Failed(reason) => Err(Error::Wait { reason }),
            }
        }
    })
    .await
}

/// [`Change::Done`] when `result`, a GET of what a delete removes, is a 404,
/// else what `pending` makes of the resource: [`Change::Pending`], or
/// [`Change::Failed`] when its state says the delete failed.
pub(crate) fn gone<T>(result: Result<T>, pending: impl FnOnce(T) -> Change) -> Result<Change> {
    match result {
        Ok(resource) => Ok(pending(resource)),
        Err(Error::Status { status: 404, .. }) => Ok(Change::Done),
        Err(error) => Err(error),
    }
}

/// What an experiment's `status` says of its delete: [`Change::Failed`] for
/// `delete-failed`, else [`Change::Pending`]. `noun` names the experiment in
/// the reason, e.g. `shadow test`.
pub(crate) fn deleting(status: Option<ExperimentStatus>, noun: &str) -> Change {
    match status {
        Some(ExperimentStatus::DeleteFailed) => {
            Change::Failed(format!("the platform could not delete the {noun}"))
        }
        _ => Change::Pending,
    }
}

/// The state of a polling loop that the caller writes itself, for checks
/// that keep state between calls or that the server tells when to repeat:
/// [`poll`] is this loop with no hint.
///
/// Make each check, and after each one that finds the resource still
/// changing, call [`Poller::wait`].
pub(crate) struct Poller<'a> {
    options: &'a PollingOptions,
    jitter: Duration,
    max_duration: Option<Duration>,
    /// When the first check started.
    start: Instant,
    /// Checks made so far.
    tries: u32,
}

impl<'a> Poller<'a> {
    /// Checks `options` and waits their `initial_delay`, ready for the
    /// first check. `options` is only read.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if a duration in `options` is negative,
    /// not finite or too large, and [`Error::Wait`] if `max_tries` is 0,
    /// without waiting.
    pub(crate) async fn start(options: &'a PollingOptions) -> Result<Poller<'a>> {
        let initial_delay = seconds("initial_delay", options.initial_delay)?;
        seconds("delay", options.delay)?;
        seconds("backoff", options.backoff)?;
        seconds("max_delay", options.max_delay)?;
        let jitter = seconds("jitter", options.jitter)?;
        let max_duration = match options.max_duration {
            Some(max) => Some(seconds("max_duration", max)?),
            None => None,
        };
        if options.max_tries == Some(0) {
            return Err(out_of_tries(0));
        }
        sleep(initial_delay).await;
        Ok(Poller {
            options,
            jitter,
            max_duration,
            start: Instant::now(),
            tries: 0,
        })
    }

    /// Counts a check that found the resource still changing, then waits
    /// before the next one: `hint`, the wait the server asked for, if set,
    /// else the backoff plus jitter, as in [`poll`]. A `hint` is cut short
    /// where `max_duration` runs out, so that a long wait from the server
    /// never outlasts it: one last check comes then.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Wait`], without waiting, if the checks so far used
    /// up `max_duration` or `max_tries`.
    pub(crate) async fn wait(&mut self, hint: Option<Duration>) -> Result<()> {
        self.tries = self.tries.saturating_add(1);
        let elapsed = self.start.elapsed();
        if let Some(max) = self.max_duration
            && elapsed >= max
        {
            return Err(Error::Wait {
                reason: format!(
                    "no terminal state after {:.1}s, past `max_duration` ({}s)",
                    elapsed.as_secs_f64(),
                    max.as_secs_f64()
                ),
            });
        }
        if let Some(max) = self.options.max_tries
            && self.tries >= max
        {
            return Err(out_of_tries(max));
        }
        let wait = match hint {
            Some(hint) => self
                .max_duration
                .map_or(hint, |max| hint.min(max.saturating_sub(elapsed))),
            None => Duration::from_secs_f64(backoff(self.options, self.tries))
                .saturating_add(self.jitter.mul_f64(random_fraction())),
        };
        sleep(wait).await;
        Ok(())
    }
}

/// Seconds to wait, before jitter, after the `tries`-th check:
/// `delay + backoff * 2^tries`, capped at `max_delay`. The options must have
/// passed [`seconds`].
fn backoff(options: &PollingOptions, tries: u32) -> f64 {
    let exponent = tries.min(1023) as i32;
    (options.delay + options.backoff * 2f64.powi(exponent)).min(options.max_delay)
}

/// The polling option `name`, `value` seconds long.
fn seconds(name: &str, value: f64) -> Result<Duration> {
    Duration::try_from_secs_f64(value).map_err(|_| Error::Invalid {
        reason: format!(
            "polling option `{name}` must be a number of seconds, at least 0, got {value}"
        ),
    })
}

/// The error for polling that used up its `max` tries.
fn out_of_tries(max: u32) -> Error {
    Error::Wait {
        reason: format!("no terminal state after {max} tries, the `max_tries` limit"),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// Options that poll as fast as possible, with no limits.
    fn fast() -> PollingOptions {
        PollingOptions {
            backoff: 0.0,
            delay: 0.001,
            initial_delay: 0.0,
            max_delay: 0.001,
            max_duration: None,
            max_tries: None,
            jitter: 0.0,
        }
    }

    #[tokio::test]
    async fn returns_the_value_once_the_check_is_done() {
        let calls = Cell::new(0);
        let options = fast();
        let before = options.clone();
        let value = poll(&options, || {
            calls.set(calls.get() + 1);
            let done = calls.get() == 3;
            async move { Ok(done.then_some("done")) }
        })
        .await
        .unwrap();
        assert_eq!(value, "done");
        assert_eq!(calls.get(), 3);
        assert_eq!(options, before);
    }

    #[tokio::test]
    async fn returns_the_first_error_of_the_check() {
        let calls = Cell::new(0);
        let result: Result<()> = poll(&fast(), || {
            calls.set(calls.get() + 1);
            async {
                Err(Error::Invalid {
                    reason: "boom".to_owned(),
                })
            }
        })
        .await;
        assert!(matches!(result, Err(Error::Invalid { .. })), "{result:?}");
        assert_eq!(calls.get(), 1);
    }

    #[tokio::test]
    async fn gives_up_after_max_tries() {
        let calls = Cell::new(0);
        let options = PollingOptions {
            max_tries: Some(3),
            ..fast()
        };
        let before = options.clone();
        let result: Result<()> = poll(&options, || {
            calls.set(calls.get() + 1);
            async { Ok(None) }
        })
        .await;
        let Err(Error::Wait { reason }) = result else {
            panic!("{result:?}");
        };
        assert_eq!(
            reason,
            "no terminal state after 3 tries, the `max_tries` limit"
        );
        assert_eq!(calls.get(), 3);
        assert_eq!(options, before);
    }

    #[tokio::test]
    async fn does_not_wait_after_the_last_try() {
        let options = PollingOptions {
            delay: 3_600.0,
            max_delay: 3_600.0,
            max_tries: Some(1),
            ..fast()
        };
        let polling = poll(&options, || async { Ok(None::<()>) });
        let result = tokio::time::timeout(Duration::from_secs(10), polling).await;
        assert!(matches!(result, Ok(Err(Error::Wait { .. }))), "{result:?}");
    }

    #[tokio::test]
    async fn makes_no_check_and_no_wait_with_zero_max_tries() {
        let calls = Cell::new(0);
        let options = PollingOptions {
            initial_delay: 3_600.0,
            max_tries: Some(0),
            ..fast()
        };
        let polling = poll(&options, || {
            calls.set(calls.get() + 1);
            async { Ok(None::<()>) }
        });
        let result = tokio::time::timeout(Duration::from_secs(10), polling).await;
        assert!(matches!(result, Ok(Err(Error::Wait { .. }))), "{result:?}");
        assert_eq!(calls.get(), 0);
    }

    #[tokio::test]
    async fn gives_up_after_max_duration() {
        let calls = Cell::new(0);
        let options = PollingOptions {
            delay: 0.01,
            max_delay: 0.01,
            max_duration: Some(0.05),
            ..fast()
        };
        let start = Instant::now();
        let result: Result<()> = poll(&options, || {
            calls.set(calls.get() + 1);
            async { Ok(None) }
        })
        .await;
        let Err(Error::Wait { reason }) = result else {
            panic!("{result:?}");
        };
        assert!(reason.contains("past `max_duration` (0.05s)"), "{reason}");
        assert!(start.elapsed() >= Duration::from_millis(50));
        assert!(calls.get() >= 2, "{}", calls.get());
    }

    #[tokio::test]
    async fn rejects_invalid_durations_before_any_check() {
        let invalid = [
            PollingOptions {
                delay: -1.0,
                ..fast()
            },
            PollingOptions {
                backoff: f64::NAN,
                ..fast()
            },
            PollingOptions {
                jitter: f64::INFINITY,
                ..fast()
            },
            PollingOptions {
                max_duration: Some(-0.5),
                ..fast()
            },
            PollingOptions {
                initial_delay: 1e30,
                ..fast()
            },
        ];
        for options in invalid {
            let calls = Cell::new(0);
            let result: Result<()> = poll(&options, || {
                calls.set(calls.get() + 1);
                async { Ok(None) }
            })
            .await;
            assert!(
                matches!(result, Err(Error::Invalid { .. })),
                "{options:?}: {result:?}"
            );
            assert_eq!(calls.get(), 0);
        }
    }

    #[tokio::test]
    async fn waits_as_long_as_the_hint_says() {
        let options = PollingOptions {
            delay: 3_600.0,
            max_delay: 3_600.0,
            ..fast()
        };
        let mut poller = Poller::start(&options).await.unwrap();
        let start = Instant::now();
        poller.wait(Some(Duration::from_millis(30))).await.unwrap();
        let waited = start.elapsed();
        assert!(waited >= Duration::from_millis(30), "{waited:?}");
        assert!(waited < Duration::from_secs(10), "{waited:?}");
    }

    #[tokio::test]
    async fn backs_off_without_a_hint() {
        let options = PollingOptions {
            delay: 0.03,
            max_delay: 0.03,
            ..fast()
        };
        let mut poller = Poller::start(&options).await.unwrap();
        let start = Instant::now();
        poller.wait(None).await.unwrap();
        assert!(start.elapsed() >= Duration::from_millis(30));
    }

    #[tokio::test]
    async fn cuts_a_hint_short_at_max_duration() {
        let options = PollingOptions {
            max_duration: Some(0.05),
            ..fast()
        };
        let mut poller = Poller::start(&options).await.unwrap();
        let waiting = poller.wait(Some(Duration::from_secs(3_600)));
        let result = tokio::time::timeout(Duration::from_secs(10), waiting).await;
        assert!(matches!(result, Ok(Ok(()))), "{result:?}");
        let result = poller.wait(Some(Duration::from_secs(3_600))).await;
        assert!(matches!(result, Err(Error::Wait { .. })), "{result:?}");
    }

    #[tokio::test]
    async fn counts_tries_across_waits() {
        let options = PollingOptions {
            max_tries: Some(2),
            ..fast()
        };
        let before = options.clone();
        let mut poller = Poller::start(&options).await.unwrap();
        poller.wait(Some(Duration::ZERO)).await.unwrap();
        let Err(Error::Wait { reason }) = poller.wait(Some(Duration::ZERO)).await else {
            panic!("the second wait should be out of tries");
        };
        assert_eq!(
            reason,
            "no terminal state after 2 tries, the `max_tries` limit"
        );
        assert_eq!(options, before);
    }

    #[test]
    fn backs_off_exponentially_up_to_max_delay() {
        let options = PollingOptions::default();
        let waits: Vec<f64> = (1..=6).map(|tries| backoff(&options, tries)).collect();
        let expected = [1.9, 3.7, 7.3, 14.5, 20.0, 20.0];
        for (wait, expected) in waits.iter().zip(expected) {
            assert!((wait - expected).abs() < 1e-9, "{waits:?}");
        }
        assert_eq!(backoff(&options, u32::MAX), 20.0);
        let no_backoff = PollingOptions {
            backoff: 0.0,
            ..options
        };
        assert_eq!(backoff(&no_backoff, u32::MAX), 0.1);
    }
}
