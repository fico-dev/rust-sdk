//! Tests for following a platform run's logs, against the mock server.

mod mock_server;
mod support;

use std::time::{Duration, Instant};

use fico_sdk::Error;
use fico_sdk::decision::platform::RunLogsFollowParams;
use fico_sdk::decision::{PollingOptions, TimestampedRunLog};
use mock_server::{MockResponse, MockServer};
use serde_json::{Value, json};

const PATH: &str = "/v1/applications/hare/runs/burrow-123/logs/live";

/// Options that poll fast, with no limits.
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

fn params(polling: PollingOptions) -> RunLogsFollowParams {
    RunLogsFollowParams { polling }
}

/// A `logs/live` response with `items`, as `(timestamp, log)` pairs.
fn page(items: &[(&str, &str)], status: &str, token: Option<&str>, next: Option<f64>) -> Value {
    let items: Vec<Value> = items
        .iter()
        .map(|(timestamp, log)| json!({ "timestamp": timestamp, "log": log }))
        .collect();
    let mut page = json!({ "items": items, "status_v2": status });
    if let Some(token) = token {
        page["next_page_token"] = json!(token);
    }
    if let Some(next) = next {
        page["next_available_in_seconds"] = json!(next);
    }
    page
}

fn ok(body: Value) -> MockResponse {
    MockResponse::json(200, body)
}

/// Follows the logs of `burrow-123` in `hare`, and returns what the callback
/// got and the result.
async fn follow(
    server: &MockServer,
    polling: PollingOptions,
) -> (Vec<String>, fico_sdk::Result<Vec<TimestampedRunLog>>) {
    let mut seen = Vec::new();
    let result = support::platform(server)
        .follow_run_logs("hare", "burrow-123", params(polling), |entry| {
            seen.push(entry.log.clone())
        })
        .await;
    (seen, result)
}

fn logs(entries: &[TimestampedRunLog]) -> Vec<&str> {
    entries.iter().map(|entry| entry.log.as_str()).collect()
}

#[tokio::test]
async fn follows_until_the_run_succeeds() {
    let server = MockServer::start(vec![
        ok(page(
            &[("2026-10-07T14:19:02Z", "start")],
            "running",
            Some("2026-10-07T14:19:02Z"),
            None,
        )),
        ok(page(
            &[
                ("2026-10-07T14:19:04Z", "solving"),
                ("2026-10-07T14:19:03.5Z", "late"),
            ],
            "running",
            Some("2026-10-07T14:19:04Z"),
            None,
        )),
        ok(page(
            &[("2026-10-07T14:19:05Z", "done")],
            "succeeded",
            None,
            None,
        )),
    ])
    .await;
    let (seen, result) = follow(&server, fast()).await;
    let entries = result.unwrap();
    assert_eq!(seen, ["start", "solving", "late", "done"]);
    assert_eq!(logs(&entries), ["start", "late", "solving", "done"]);
    assert_eq!(entries[1].timestamp, "2026-10-07T14:19:03.5Z");

    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert_eq!(request.method, "GET");
        assert_eq!(request.target.split('?').next(), Some(PATH));
        assert_eq!(request.header("authorization"), Some("Bearer secret"));
    }
    assert!(requests[0].query().is_empty());
    let since = |i: usize| requests[i].query();
    assert_eq!(
        since(1),
        [("since".to_owned(), "2026-10-07T14:19:02Z".to_owned())]
    );
    assert_eq!(
        since(2),
        [("since".to_owned(), "2026-10-07T14:19:04Z".to_owned())]
    );
}

#[tokio::test]
async fn hands_over_an_entry_that_the_platform_sends_again_once() {
    // The platform orders `…05.51Z` before `…05.5Z`, as strings, and takes
    // `since` from the last entry, so it sends `…05.51Z` again.
    let server = MockServer::start(vec![
        ok(page(
            &[
                ("2026-10-07T14:19:05.51Z", "late"),
                ("2026-10-07T14:19:05.5Z", "early"),
            ],
            "running",
            Some("2026-10-07T14:19:05.5Z"),
            None,
        )),
        ok(page(
            &[
                ("2026-10-07T14:19:05.51Z", "late"),
                ("2026-10-07T14:19:06Z", "next"),
            ],
            "running",
            Some("2026-10-07T14:19:06Z"),
            None,
        )),
        ok(page(
            &[("2026-10-07T14:19:07Z", "late")],
            "succeeded",
            None,
            None,
        )),
    ])
    .await;
    let (seen, result) = follow(&server, fast()).await;
    assert_eq!(seen, ["late", "early", "next", "late"]);
    let entries = result.unwrap();
    assert_eq!(logs(&entries), ["early", "late", "next", "late"]);
    assert_eq!(entries[3].timestamp, "2026-10-07T14:19:07Z");
}

#[tokio::test]
async fn stops_on_every_terminal_status() {
    for status in ["failed", "canceled", "succeeded"] {
        let server = MockServer::start(vec![
            ok(page(&[], "queued", None, None)),
            ok(page(
                &[("2026-10-07T14:19:02Z", "boom")],
                status,
                None,
                None,
            )),
        ])
        .await;
        let (seen, result) = follow(&server, fast()).await;
        assert_eq!(logs(&result.unwrap()), ["boom"], "{status}");
        assert_eq!(seen, ["boom"], "{status}");
        assert_eq!(server.requests().len(), 2, "{status}");
    }
}

#[tokio::test]
async fn keeps_the_last_since_without_a_new_token() {
    let server = MockServer::start(vec![
        ok(page(
            &[("2026-10-07T14:19:02Z", "start")],
            "running",
            Some("2026-10-07T14:19:02Z"),
            None,
        )),
        ok(page(&[], "running", None, None)),
        ok(page(&[], "running", Some(""), None)),
        ok(page(&[], "succeeded", None, None)),
    ])
    .await;
    let (_, result) = follow(&server, fast()).await;
    result.unwrap();
    let requests = server.requests();
    assert_eq!(requests.len(), 4);
    for request in &requests[1..] {
        assert_eq!(
            request.query(),
            [("since".to_owned(), "2026-10-07T14:19:02Z".to_owned())]
        );
    }
}

#[tokio::test]
async fn fetches_the_entries_held_back_when_the_run_is_done() {
    let server = MockServer::start(vec![
        // A wait the follow must not take once the run is done.
        ok(page(
            &[("2026-10-07T14:19:02Z", "one")],
            "succeeded",
            Some("2026-10-07T14:19:02Z"),
            Some(3_600.0),
        )),
        ok(page(
            &[("2026-10-07T14:19:03Z", "two")],
            "succeeded",
            None,
            None,
        )),
    ])
    .await;
    let polling = PollingOptions {
        delay: 3_600.0,
        max_delay: 3_600.0,
        ..fast()
    };
    let following = follow(&server, polling);
    let (seen, result) = tokio::time::timeout(Duration::from_secs(10), following)
        .await
        .unwrap();
    assert_eq!(logs(&result.unwrap()), ["one", "two"]);
    assert_eq!(seen, ["one", "two"]);
    let requests = server.requests();
    assert_eq!(
        requests[1].query(),
        [("since".to_owned(), "2026-10-07T14:19:02Z".to_owned())]
    );
}

#[tokio::test]
async fn waits_as_long_as_the_platform_asks() {
    let server = MockServer::start(vec![
        ok(page(&[], "running", None, Some(0.3))),
        ok(page(&[], "succeeded", None, None)),
    ])
    .await;
    // The backoff alone would wait an hour.
    let polling = PollingOptions {
        delay: 3_600.0,
        max_delay: 3_600.0,
        ..fast()
    };
    let start = Instant::now();
    let following = follow(&server, polling);
    let (_, result) = tokio::time::timeout(Duration::from_secs(10), following)
        .await
        .unwrap();
    result.unwrap();
    let waited = start.elapsed();
    assert!(waited >= Duration::from_millis(300), "{waited:?}");
}

#[tokio::test]
async fn backs_off_when_the_platform_does_not_say() {
    for next in [None, Some(0.0)] {
        let server = MockServer::start(vec![
            ok(page(&[], "running", None, next)),
            ok(page(&[], "running", None, next)),
            ok(page(&[], "succeeded", None, None)),
        ])
        .await;
        let polling = PollingOptions {
            delay: 0.1,
            max_delay: 0.1,
            ..fast()
        };
        let start = Instant::now();
        let (_, result) = follow(&server, polling).await;
        result.unwrap();
        let waited = start.elapsed();
        assert!(waited >= Duration::from_millis(200), "{next:?}: {waited:?}");
        assert_eq!(server.requests().len(), 3);
    }
}

#[tokio::test]
async fn gives_up_after_max_tries() {
    let server = MockServer::start(vec![
        ok(page(
            &[("2026-10-07T14:19:02Z", "one")],
            "running",
            None,
            None,
        )),
        ok(page(
            &[("2026-10-07T14:19:03Z", "two")],
            "running",
            None,
            None,
        )),
    ])
    .await;
    let polling = PollingOptions {
        max_tries: Some(2),
        ..fast()
    };
    let (seen, result) = follow(&server, polling).await;
    let Err(Error::Wait { reason, .. }) = result else {
        panic!("{result:?}");
    };
    assert!(reason.contains("`max_tries`"), "{reason}");
    assert_eq!(seen, ["one", "two"]);
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn gives_up_after_max_duration_even_if_the_platform_asks_to_wait() {
    let server = MockServer::start(vec![
        ok(page(&[], "running", None, Some(3_600.0))),
        ok(page(&[], "running", None, Some(3_600.0))),
    ])
    .await;
    let polling = PollingOptions {
        max_duration: Some(0.1),
        ..fast()
    };
    let following = follow(&server, polling);
    let (_, result) = tokio::time::timeout(Duration::from_secs(10), following)
        .await
        .unwrap();
    assert!(matches!(result, Err(Error::Wait { .. })), "{result:?}");
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn says_when_the_run_ended_before_its_logs_were_all_fetched() {
    let server = MockServer::start(vec![
        ok(page(&[], "running", None, Some(3_600.0))),
        ok(page(
            &[("2026-10-07T14:19:02Z", "one")],
            "succeeded",
            Some("2026-10-07T14:19:02Z"),
            None,
        )),
    ])
    .await;
    let polling = PollingOptions {
        max_duration: Some(0.1),
        ..fast()
    };
    let following = follow(&server, polling);
    let (seen, result) = tokio::time::timeout(Duration::from_secs(10), following)
        .await
        .unwrap();
    let Err(Error::Wait { reason, .. }) = result else {
        panic!("{result:?}");
    };
    assert!(reason.starts_with("the run ended"), "{reason}");
    assert_eq!(seen, ["one"]);
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn reports_a_missing_run() {
    let server = MockServer::start(vec![MockResponse::json(
        404,
        json!({ "error": "not found" }),
    )])
    .await;
    let (seen, result) = follow(&server, fast()).await;
    let Err(Error::Status { status, .. }) = result else {
        panic!("{result:?}");
    };
    assert_eq!(status, 404);
    assert!(seen.is_empty());
}

#[tokio::test]
async fn runs_on_a_spawned_task() {
    let server = MockServer::start(vec![ok(page(
        &[("2026-10-07T14:19:02Z", "done")],
        "succeeded",
        None,
        None,
    ))])
    .await;
    let platform = support::platform(&server);
    let task = tokio::spawn(async move {
        let mut count = 0;
        let entries = platform
            .follow_run_logs("hare", "burrow-123", params(fast()), |_| count += 1)
            .await;
        (count, entries)
    });
    let (count, entries) = task.await.unwrap();
    assert_eq!(count, 1);
    assert_eq!(entries.unwrap().len(), 1);
}
