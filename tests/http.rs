//! Integration tests for the HTTP client, run against an in-house mock
//! server.

mod mock_server;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use fico_sdk::tool::BoxFuture;
use fico_sdk::{Client, Error, Method, RetryPolicy, TokenSource};
use mock_server::{MockResponse, MockServer, unused_url};
use serde_json::json;

/// Retries without waiting, so tests stay fast.
fn fast_retry() -> RetryPolicy {
    RetryPolicy::default()
        .backoff_factor(Duration::ZERO)
        .jitter(Duration::ZERO)
}

fn client(server: &MockServer) -> Client {
    Client::builder(server.url())
        .bearer_token("secret")
        .retry(fast_retry())
        .build()
        .unwrap()
}

#[tokio::test]
async fn sends_default_headers() {
    let server = MockServer::start(vec![MockResponse::new(200)]).await;
    client(&server).get("/v1/apps").send().await.unwrap();

    let request = &server.requests()[0];
    assert_eq!(request.method, "GET");
    assert_eq!(request.target, "/v1/apps");
    assert_eq!(request.header("authorization"), Some("Bearer secret"));
    let user_agent = format!("fico-sdk/{}", fico_sdk::VERSION);
    assert_eq!(request.header("user-agent"), Some(user_agent.as_str()));
    assert_eq!(fico_sdk::VERSION, env!("CARGO_PKG_VERSION"));
    assert_eq!(request.header("content-type"), None);
}

#[tokio::test]
async fn appends_paths_to_the_base_path() {
    let server = MockServer::start(vec![MockResponse::new(200)]).await;
    let client = Client::new(format!("{}/api/", server.url())).unwrap();
    client.get("/v1/apps").send().await.unwrap();
    assert_eq!(server.requests()[0].target, "/api/v1/apps");
}

#[tokio::test]
async fn sends_every_method() {
    let methods = [
        Method::Get,
        Method::Post,
        Method::Put,
        Method::Patch,
        Method::Delete,
    ];
    let server = MockServer::start(vec![MockResponse::new(204); methods.len()]).await;
    let client = client(&server);
    for method in methods {
        client.request(method, "/x").send().await.unwrap();
    }
    let sent: Vec<String> = server.requests().into_iter().map(|r| r.method).collect();
    assert_eq!(sent, ["GET", "POST", "PUT", "PATCH", "DELETE"]);
}

#[tokio::test]
async fn overrides_defaults_per_request() {
    let server = MockServer::start(vec![MockResponse::new(200); 2]).await;
    let client = Client::builder(server.url())
        .bearer_token("secret")
        .header("x-team", "default-team")
        .user_agent("custom-agent/1.0")
        .build()
        .unwrap();

    client
        .get("/x")
        .query(&[("limit", "5")])
        .query(&json!({ "tags": ["a", "b"], "cursor": null }))
        .header("x-team", "other-team")
        .header("x-request-id", "abc-123")
        .remove_header("user-agent")
        .bearer_token("override")
        .send()
        .await
        .unwrap();
    client.get("/x").no_auth().send().await.unwrap();

    let requests = server.requests();
    let first = &requests[0];
    assert_eq!(
        first.query(),
        [("limit", "5"), ("tags", "a"), ("tags", "b")].map(|(k, v)| (k.to_owned(), v.to_owned()))
    );
    assert_eq!(first.header("x-team"), Some("other-team"));
    assert_eq!(first.header("x-request-id"), Some("abc-123"));
    assert_eq!(first.header("user-agent"), None);
    assert_eq!(first.header("authorization"), Some("Bearer override"));

    let second = &requests[1];
    assert_eq!(second.header("x-team"), Some("default-team"));
    assert_eq!(second.header("user-agent"), Some("custom-agent/1.0"));
    assert_eq!(second.header("authorization"), None);
}

#[tokio::test]
async fn keeps_the_bearer_token_on_the_base_origin() {
    let api = MockServer::start(vec![]).await;
    let storage = MockServer::start(vec![MockResponse::new(200)]).await;
    let presigned = format!("{}/upload?signature=abc", storage.url());

    client(&api)
        .put(&presigned)
        .body("file contents")
        .send()
        .await
        .unwrap();

    let request = &storage.requests()[0];
    assert_eq!(request.target, "/upload?signature=abc");
    assert_eq!(request.header("authorization"), None);
    assert_eq!(request.body, b"file contents");
    assert!(api.requests().is_empty());
}

#[tokio::test]
async fn does_not_follow_redirects_off_the_base_origin() {
    let other = MockServer::start(vec![MockResponse::new(200)]).await;
    let api = MockServer::start(vec![
        MockResponse::new(302).header("location", &format!("{}/elsewhere", other.url())),
    ])
    .await;
    let client = Client::builder(api.url())
        .header("x-api-key", "k")
        .build()
        .unwrap();

    let err = client.get("/start").send().await.unwrap_err();

    let elsewhere = format!("{}/elsewhere", other.url());
    assert!(
        matches!(&err, Error::Redirect { status: 302, location, .. } if *location == elsewhere),
        "{err}"
    );
    assert!(other.requests().is_empty());
}

#[tokio::test]
async fn reports_unfollowed_redirects_with_an_absolute_location() {
    let api = MockServer::start(vec![
        MockResponse::new(300).header("location", "/choice"),
        MockResponse::new(304),
    ])
    .await;
    let client = client(&api);

    let err = client.get("/start").send().await.unwrap_err();
    let choice = format!("{}/choice", api.url());
    assert!(
        matches!(&err, Error::Redirect { status: 300, location, .. } if *location == choice),
        "{err}"
    );
    let response = client.get("/start").send().await.unwrap();
    assert_eq!(response.status(), 304);
}

#[tokio::test]
async fn follows_redirects_within_an_origin() {
    let api = MockServer::start(vec![
        MockResponse::new(302).header("location", "/moved"),
        MockResponse::new(200),
    ])
    .await;

    let response = client(&api).get("/start").send().await.unwrap();

    assert_eq!(response.status(), 200);
    let requests = api.requests();
    assert_eq!(requests[1].target, "/moved");
    assert_eq!(requests[1].header("authorization"), Some("Bearer secret"));
}

#[tokio::test]
async fn sends_json_and_raw_bodies() {
    let server = MockServer::start(vec![MockResponse::new(200); 2]).await;
    let client = client(&server);
    client
        .post("/json")
        .json(&json!({ "input": { "value": 10 } }))
        .send()
        .await
        .unwrap();
    client
        .post("/raw")
        .header("content-type", "text/csv")
        .body("a,b\n1,2\n")
        .send()
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests[0].header("content-type"), Some("application/json"));
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body, json!({ "input": { "value": 10 } }));
    assert_eq!(requests[1].header("content-type"), Some("text/csv"));
    assert_eq!(requests[1].body, b"a,b\n1,2\n");
}

#[tokio::test]
async fn exposes_the_response() {
    type App = HashMap<String, String>;

    let server = MockServer::start(vec![
        MockResponse::json(200, json!({ "id": "my-app" })).header("x-trace", "t-1"),
        MockResponse::json(200, json!({ "id": "my-app" })),
    ])
    .await;
    let client = client(&server);

    let response = client.get("/v1/apps/my-app").send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.header("X-Trace"), Some("t-1"));
    assert_eq!(response.url(), format!("{}/v1/apps/my-app", server.url()));
    assert_eq!(response.text().unwrap(), r#"{"id":"my-app"}"#);
    assert_eq!(response.json::<App>().unwrap()["id"], "my-app");

    let app: App = client.get("/v1/apps/my-app").send_json().await.unwrap();
    assert_eq!(app["id"], "my-app");
}

#[tokio::test]
async fn retries_retryable_statuses() {
    for status in [429, 502, 503, 504] {
        let server = MockServer::start(vec![
            MockResponse::new(status),
            MockResponse::new(status),
            MockResponse::new(200),
        ])
        .await;
        let response = client(&server).get("/x").send().await.unwrap();
        assert_eq!(response.status(), 200, "{status}");
        assert_eq!(server.requests().len(), 3, "{status}");
    }
}

#[tokio::test]
async fn retries_with_the_same_request() {
    let server = MockServer::start(vec![MockResponse::new(503), MockResponse::new(200)]).await;
    client(&server)
        .post("/runs")
        .query(&[("a", "1")])
        .json(&json!({ "n": 1 }))
        .send()
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].target, requests[1].target);
    assert_eq!(requests[0].body, requests[1].body);
    assert_eq!(requests[1].header("authorization"), Some("Bearer secret"));
}

#[tokio::test]
async fn does_not_retry_other_statuses() {
    for status in [400, 401, 404, 500] {
        let server = MockServer::start(vec![MockResponse::new(status)]).await;
        let err = client(&server).get("/x").send().await.unwrap_err();
        assert!(matches!(err, Error::Status { status: s, .. } if s == status));
        assert_eq!(server.requests().len(), 1, "{status}");
    }
}

#[tokio::test]
async fn does_not_retry_other_methods() {
    let server = MockServer::start(vec![MockResponse::new(503)]).await;
    let err = client(&server).patch("/x").send().await.unwrap_err();
    assert!(matches!(err, Error::Status { status: 503, .. }));
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn retries_post_only_when_it_was_not_processed() {
    let server = MockServer::start(vec![MockResponse::new(502), MockResponse::new(200)]).await;
    let err = client(&server).post("/runs").send().await.unwrap_err();
    assert!(matches!(err, Error::Status { status: 502, .. }));
    assert_eq!(server.requests().len(), 1);

    let server = MockServer::start(vec![
        MockResponse::new(200).delay(Duration::from_secs(5)),
        MockResponse::new(200),
    ])
    .await;
    let err = client(&server)
        .post("/runs")
        .timeout(Duration::from_millis(100))
        .send()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Timeout { .. }), "{err}");
    assert_eq!(server.requests().len(), 1);

    let server = MockServer::start(vec![MockResponse::new(429), MockResponse::new(200)]).await;
    client(&server).post("/runs").send().await.unwrap();
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn stops_after_max_retries() {
    let server = MockServer::start(vec![MockResponse::new(503); 5]).await;
    let err = client(&server)
        .get("/x")
        .retry(fast_retry().max_retries(2))
        .send()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Status { status: 503, .. }));
    assert_eq!(server.requests().len(), 3);
}

#[tokio::test]
async fn no_retry_disables_retries() {
    let server = MockServer::start(vec![MockResponse::new(503), MockResponse::new(200)]).await;
    let err = client(&server)
        .get("/x")
        .no_retry()
        .send()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Status { status: 503, .. }));
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn honors_retry_after() {
    let server = MockServer::start(vec![
        MockResponse::new(429).header("retry-after", "1"),
        MockResponse::new(200),
    ])
    .await;
    let started = Instant::now();
    client(&server).get("/x").send().await.unwrap();
    assert!(started.elapsed() >= Duration::from_secs(1));
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn does_not_retry_when_retry_after_exceeds_max_backoff() {
    let server = MockServer::start(vec![
        MockResponse::new(503).header("retry-after", "3600"),
        MockResponse::new(200),
    ])
    .await;
    let started = Instant::now();
    let err = client(&server).get("/x").send().await.unwrap_err();
    assert!(matches!(err, Error::Status { status: 503, .. }), "{err}");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(server.requests().len(), 1);
}

/// A `429` with the platform's busy body.
fn busy(should_retry: bool, retry_in_milliseconds: u64) -> MockResponse {
    MockResponse::json(
        429,
        json!({
            "status": "Busy",
            "error": "rate limit exceeded",
            "detail": {
                "reason": "busy_api_limit",
                "should_retry": should_retry,
                "retry_in_milliseconds": retry_in_milliseconds,
            },
        }),
    )
}

#[tokio::test]
async fn honors_the_busy_body_over_retry_after() {
    let server = MockServer::start(vec![
        busy(true, 200).header("retry-after", "30"),
        MockResponse::new(200),
    ])
    .await;
    let started = Instant::now();
    client(&server).get("/x").send().await.unwrap();
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(200), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn honors_retry_after_when_the_body_has_no_wait() {
    let server = MockServer::start(vec![
        MockResponse::json(
            429,
            json!({ "status": "Busy", "detail": { "should_retry": true } }),
        )
        .header("retry-after", "1"),
        MockResponse::new(200),
    ])
    .await;
    let started = Instant::now();
    client(&server).get("/x").send().await.unwrap();
    assert!(started.elapsed() >= Duration::from_secs(1));
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn does_not_retry_when_the_body_says_not_to() {
    let server = MockServer::start(vec![busy(false, 10), MockResponse::new(200)]).await;
    let err = client(&server).post("/runs").send().await.unwrap_err();
    assert!(matches!(err, Error::Status { status: 429, .. }), "{err}");
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn does_not_retry_when_the_body_asks_for_longer_than_max_backoff() {
    let server = MockServer::start(vec![busy(true, 3_600_000), MockResponse::new(200)]).await;
    let started = Instant::now();
    let err = client(&server).get("/x").send().await.unwrap_err();
    assert!(matches!(err, Error::Status { status: 429, .. }), "{err}");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn reads_the_busy_body_only_on_429() {
    let body = json!({ "detail": { "should_retry": false, "retry_in_milliseconds": 3_600_000 } });
    let server =
        MockServer::start(vec![MockResponse::json(503, body), MockResponse::new(200)]).await;
    client(&server).get("/x").send().await.unwrap();
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn times_out() {
    let server =
        MockServer::start(vec![MockResponse::new(200).delay(Duration::from_secs(5))]).await;
    let err = client(&server)
        .get("/slow")
        .timeout(Duration::from_millis(100))
        .no_retry()
        .send()
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Timeout { after, .. } if *after == Duration::from_millis(100)),
        "{err}"
    );
}

/// A client whose connect limit is `read_timeout` too, so the response
/// head's limit is `2 * read_timeout`, or `3 * read_timeout` with a body.
fn read_limited(server: &MockServer, read_timeout: Duration) -> Client {
    Client::builder(server.url())
        .connect_timeout(read_timeout)
        .read_timeout(read_timeout)
        .retry(RetryPolicy::none())
        .build()
        .unwrap()
}

fn is_timeout_after(err: &Error, limit: Duration) -> bool {
    matches!(err, Error::Timeout { after, .. } if *after == limit)
}

#[tokio::test]
async fn reads_a_trickling_body_that_takes_longer_than_the_read_limit() {
    let server = MockServer::start(vec![
        MockResponse::new(200)
            .body("12345678")
            .trickle(Duration::from_millis(200)),
    ])
    .await;
    let read_timeout = Duration::from_secs(1);
    let started = Instant::now();
    let response = read_limited(&server, read_timeout)
        .get("/x")
        .send()
        .await
        .unwrap();
    assert!(started.elapsed() > read_timeout);
    assert_eq!(response.bytes(), b"12345678");
}

#[tokio::test]
async fn times_out_when_the_body_stalls() {
    let server = MockServer::start(vec![
        MockResponse::new(200)
            .body("12")
            .trickle(Duration::from_secs(2)),
    ])
    .await;
    let read_timeout = Duration::from_millis(300);
    let err = read_limited(&server, read_timeout)
        .get("/x")
        .send()
        .await
        .unwrap_err();
    assert!(is_timeout_after(&err, read_timeout), "{err}");
}

#[tokio::test]
async fn times_out_when_the_response_head_is_late() {
    let server =
        MockServer::start(vec![MockResponse::new(200).delay(Duration::from_secs(2))]).await;
    let read_timeout = Duration::from_millis(300);
    let err = read_limited(&server, read_timeout)
        .get("/x")
        .send()
        .await
        .unwrap_err();
    assert!(is_timeout_after(&err, read_timeout * 2), "{err}");
}

#[tokio::test]
async fn gives_the_answer_a_read_limit_of_its_own_after_a_slow_upload() {
    // `requests` limits sending the body and waiting for the answer one at a
    // time, so an upload and an answer that each fit in the read limit
    // succeed, even when together they take longer.
    let read_timeout = Duration::from_millis(500);
    let server = MockServer::start(vec![
        MockResponse::new(200)
            .read_body_slowly(Duration::from_millis(400))
            .delay(Duration::from_millis(400)),
    ])
    .await;
    let started = Instant::now();
    read_limited(&server, read_timeout)
        .post("/runs")
        .body(vec![b'x'; 1 << 20])
        .send()
        .await
        .unwrap();
    assert!(started.elapsed() > read_timeout);

    let server =
        MockServer::start(vec![MockResponse::new(200).delay(Duration::from_secs(5))]).await;
    let err = read_limited(&server, read_timeout)
        .post("/runs")
        .body("x")
        .send()
        .await
        .unwrap_err();
    assert!(is_timeout_after(&err, read_timeout * 3), "{err}");
}

#[tokio::test]
async fn limits_the_whole_attempt_when_asked() {
    let trickling = || {
        MockResponse::new(200)
            .body("12345678")
            .trickle(Duration::from_millis(200))
    };
    let limit = Duration::from_millis(500);

    let server = MockServer::start(vec![trickling()]).await;
    let client = Client::builder(server.url())
        .timeout(limit)
        .retry(RetryPolicy::none())
        .build()
        .unwrap();
    let err = client.get("/x").send().await.unwrap_err();
    assert!(is_timeout_after(&err, limit), "{err}");

    let server = MockServer::start(vec![trickling()]).await;
    let err = read_limited(&server, Duration::from_secs(1))
        .get("/x")
        .timeout(limit)
        .send()
        .await
        .unwrap_err();
    assert!(is_timeout_after(&err, limit), "{err}");
}

#[tokio::test]
async fn overrides_the_read_limit_per_request() {
    let server =
        MockServer::start(vec![MockResponse::new(200).delay(Duration::from_secs(1))]).await;
    read_limited(&server, Duration::from_millis(200))
        .get("/x")
        .read_timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();

    let server = MockServer::start(vec![
        MockResponse::new(200)
            .body("12")
            .trickle(Duration::from_secs(2)),
    ])
    .await;
    let read_timeout = Duration::from_millis(200);
    let err = read_limited(&server, Duration::from_secs(5))
        .get("/x")
        .read_timeout(read_timeout)
        .send()
        .await
        .unwrap_err();
    assert!(is_timeout_after(&err, read_timeout), "{err}");
}

#[tokio::test]
async fn retries_timeouts() {
    let server = MockServer::start(vec![
        MockResponse::new(200).delay(Duration::from_secs(5)),
        MockResponse::new(200),
    ])
    .await;
    let response = client(&server)
        .get("/slow")
        .timeout(Duration::from_millis(100))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(server.requests().len(), 2);
}

/// A response that comes too late for a request with a 100ms limit.
fn too_late() -> MockResponse {
    MockResponse::new(200).delay(Duration::from_secs(5))
}

#[tokio::test]
async fn stops_after_two_retries_without_a_response() {
    let server = MockServer::start(vec![too_late(); 4]).await;
    let err = client(&server)
        .get("/slow")
        .timeout(Duration::from_millis(100))
        .send()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Timeout { .. }), "{err}");
    assert_eq!(server.requests().len(), 3);
}

#[tokio::test]
async fn keeps_ten_retries_for_statuses() {
    let mut responses = vec![MockResponse::new(503); 11];
    responses.push(MockResponse::new(200));
    let server = MockServer::start(responses).await;
    let err = client(&server).get("/x").send().await.unwrap_err();
    assert!(matches!(err, Error::Status { status: 503, .. }), "{err}");
    assert_eq!(server.requests().len(), 11);
}

#[tokio::test]
async fn stops_mixed_failures_when_the_transport_retries_run_out() {
    let server = MockServer::start(vec![
        too_late(),
        MockResponse::new(503),
        too_late(),
        MockResponse::new(503),
        too_late(),
        MockResponse::new(200),
    ])
    .await;
    let err = client(&server)
        .get("/x")
        .timeout(Duration::from_millis(100))
        .send()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Timeout { .. }), "{err}");
    assert_eq!(server.requests().len(), 5);
}

#[tokio::test]
async fn caps_retries_without_a_response_by_max_retries() {
    let server = MockServer::start(vec![too_late(); 3]).await;
    let err = client(&server)
        .get("/slow")
        .timeout(Duration::from_millis(100))
        .retry(fast_retry().max_retries(1))
        .send()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Timeout { .. }), "{err}");
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn reports_connection_errors() {
    let client = Client::builder(unused_url().await)
        .retry(fast_retry().max_retries(1))
        .build()
        .unwrap();
    let err = client.get("/x").send().await.unwrap_err();
    assert!(matches!(err, Error::Connect { .. }), "{err}");
    assert!(std::error::Error::source(&err).is_some());
}

#[tokio::test]
async fn reports_status_errors_with_the_body() {
    let server = MockServer::start(vec![MockResponse::new(404).body("app not found")]).await;
    let err = client(&server)
        .get("/v1/apps/nope")
        .send()
        .await
        .unwrap_err();
    let Error::Status {
        status, url, body, ..
    } = &err
    else {
        panic!("unexpected error: {err}");
    };
    assert_eq!(*status, 404);
    assert_eq!(url, &format!("{}/v1/apps/nope", server.url()));
    assert_eq!(body, "app not found");
    assert!(err.to_string().contains("404"));
}

#[tokio::test]
async fn reads_floats_exactly() {
    // The default float parser reads these one ULP off.
    let body = r#"{"a": 1.0000000000000107, "b": 1.9895788082817987}"#;
    let server = MockServer::start(vec![MockResponse::new(200).body(body)]).await;
    let read: HashMap<String, f64> = client(&server).get("/x").send_json().await.unwrap();
    assert_eq!(read["a"].to_string(), "1.0000000000000107");
    assert_eq!(read["b"].to_string(), "1.9895788082817987");
}

#[tokio::test]
async fn reports_decode_errors() {
    let server = MockServer::start(vec![MockResponse::new(200).body("not json")]).await;
    let err = client(&server)
        .get("/x")
        .send_json::<serde_json::Value>()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Decode { .. }), "{err}");
}

#[tokio::test]
async fn decodes_an_empty_body_as_null() {
    let server = MockServer::start(vec![
        MockResponse::new(204),
        MockResponse::new(204),
        MockResponse::new(204),
    ])
    .await;
    let client = client(&server);

    client.delete("/x").send_json::<()>().await.unwrap();
    let none = client
        .delete("/x")
        .send_json::<Option<u32>>()
        .await
        .unwrap();
    assert_eq!(none, None);
    let err = client
        .delete("/x")
        .send_json::<Vec<u32>>()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Decode { .. }), "{err}");
}

#[tokio::test]
async fn reports_invalid_requests_on_send() {
    let server = MockServer::start(vec![]).await;
    let client = client(&server);

    let err = client
        .get("/x")
        .header("bad name", "v")
        .send()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidHeader { .. }), "{err}");
    let err = client.get("ftp://host/x").send().await.unwrap_err();
    assert!(matches!(err, Error::InvalidUrl { .. }), "{err}");
    let err = client
        .get("/x")
        .query(&json!({ "a": { "b": 1 } }))
        .send()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Encode { .. }), "{err}");
    assert!(server.requests().is_empty());
}

fn page(items: serde_json::Value, next_page_token: Option<&str>) -> MockResponse {
    MockResponse::json(
        200,
        json!({ "items": items, "next_page_token": next_page_token }),
    )
}

#[tokio::test]
async fn collects_all_pages() {
    let server = MockServer::start(vec![
        page(json!([1, 2]), Some("t1")),
        page(json!([]), Some("t2")),
        page(json!([3]), None),
    ])
    .await;
    let items: Vec<u32> = client(&server)
        .get("/v1/apps")
        .query(&[("status", "active")])
        .paginate()
        .collect()
        .await
        .unwrap();
    assert_eq!(items, [1, 2, 3]);

    let queries: Vec<Vec<(String, String)>> = server.requests().iter().map(|r| r.query()).collect();
    let pair = |k: &str, v: &str| (k.to_owned(), v.to_owned());
    assert_eq!(
        queries,
        [
            vec![pair("status", "active"), pair("pagereturn", "true")],
            vec![
                pair("status", "active"),
                pair("pagereturn", "true"),
                pair("pagetoken", "t1")
            ],
            vec![
                pair("status", "active"),
                pair("pagereturn", "true"),
                pair("pagetoken", "t2")
            ],
        ]
    );
}

#[tokio::test]
async fn fetches_pages_lazily() {
    let server = MockServer::start(vec![
        page(json!([1, 2]), Some("t1")),
        page(json!([3]), None),
    ])
    .await;
    let mut pages = client(&server).get("/v1/apps").paginate::<u32>();

    assert_eq!(pages.next().await.unwrap(), Some(1));
    assert_eq!(server.requests().len(), 1);
    assert_eq!(pages.next().await.unwrap(), Some(2));
    assert_eq!(pages.next().await.unwrap(), Some(3));
    assert_eq!(server.requests().len(), 2);
    assert_eq!(pages.next().await.unwrap(), None);
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn uses_a_custom_items_key() {
    let server =
        MockServer::start(vec![MockResponse::json(200, json!({ "runs": ["a", "b"] }))]).await;
    let items: Vec<String> = client(&server)
        .get("/v1/runs")
        .paginate()
        .items_key("runs")
        .collect()
        .await
        .unwrap();
    assert_eq!(items, ["a", "b"]);
}

#[tokio::test]
async fn stops_at_max_pages() {
    let server = MockServer::start(vec![
        page(json!([1]), Some("t1")),
        page(json!([2]), Some("t2")),
        page(json!([3]), None),
    ])
    .await;
    let err = client(&server)
        .get("/v1/apps")
        .paginate::<u32>()
        .max_pages(2)
        .collect()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Pagination { .. }), "{err}");
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn rejects_malformed_pages() {
    for (body, expected) in [
        (json!([1, 2]), "is not a JSON object"),
        (json!({ "items": "nope" }), "non-list `items`"),
        (
            json!({ "items": [], "next_page_token": 5 }),
            "non-string `next_page_token`",
        ),
    ] {
        let server = MockServer::start(vec![MockResponse::json(200, body)]).await;
        let err = client(&server)
            .get("/v1/apps")
            .paginate::<u32>()
            .collect()
            .await
            .unwrap_err();
        assert!(err.to_string().contains(expected), "{err}");
    }

    let server = MockServer::start(vec![page(json!(["not a number"]), None)]).await;
    let err = client(&server)
        .get("/v1/apps")
        .paginate::<u32>()
        .collect()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Decode { .. }), "{err}");
}

#[tokio::test]
async fn keeps_failing_after_a_bad_page() {
    for bad_page in [
        page(json!([1, 2, "x"]), Some("t1")),
        MockResponse::json(200, json!({ "items": [1, 2], "next_page_token": 5 })),
    ] {
        let server = MockServer::start(vec![bad_page, page(json!([3]), None)]).await;
        let mut pages = client(&server).get("/v1/apps").paginate::<u32>();

        let first = pages.next().await.unwrap_err();
        assert!(
            matches!(first, Error::Decode { .. } | Error::Pagination { .. }),
            "{first}"
        );
        let second = pages.next().await.unwrap_err();
        assert!(
            second.to_string().contains("an earlier page failed"),
            "{second}"
        );
        assert_eq!(server.requests().len(), 1);
    }
}

/// A token source that hands out `token-1`, `token-2`, … and counts how
/// often it was asked.
#[derive(Clone, Default)]
struct Counter(Arc<AtomicUsize>);

impl TokenSource for Counter {
    fn token(&self) -> BoxFuture<'_, fico_sdk::Result<String>> {
        let n = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move { Ok(format!("token-{n}")) })
    }
}

#[tokio::test]
async fn asks_the_token_source_before_each_request() {
    let page = |token: Option<&str>| {
        MockResponse::json(200, json!({"items": [1], "next_page_token": token}))
    };
    let server = MockServer::start(vec![
        MockResponse::new(200),
        page(Some("p2")),
        page(None),
        MockResponse::new(503),
        MockResponse::new(200),
    ])
    .await;
    let counter = Counter::default();
    let client = Client::builder(server.url())
        .token_source(counter.clone())
        .retry(fast_retry())
        .build()
        .unwrap();
    assert_eq!(counter.0.load(Ordering::SeqCst), 0, "building asks nothing");

    client.get("/a").send().await.unwrap();
    let items: Vec<i32> = client.get("/b").paginate().collect().await.unwrap();
    assert_eq!(items, [1, 1]);
    client.get("/c").send().await.unwrap();

    let auth: Vec<_> = server
        .requests()
        .iter()
        .map(|r| r.header("authorization").unwrap().to_owned())
        .collect();
    // One token per request and per page; a retry reuses its request's.
    assert_eq!(
        auth,
        [
            "Bearer token-1",
            "Bearer token-2",
            "Bearer token-3",
            "Bearer token-4",
            "Bearer token-4"
        ]
    );
}

#[tokio::test]
async fn keeps_the_token_source_on_its_origin() {
    let server = MockServer::start(vec![MockResponse::new(200), MockResponse::new(200)]).await;
    let other = MockServer::start(vec![MockResponse::new(200)]).await;
    let counter = Counter::default();
    let client = Client::builder(server.url())
        .token_source(counter.clone())
        .build()
        .unwrap();
    client
        .get(format!("{}/x", other.url()))
        .send()
        .await
        .unwrap();
    client.get("/x").no_auth().send().await.unwrap();
    client.get("/x").bearer_token("own").send().await.unwrap();

    assert_eq!(counter.0.load(Ordering::SeqCst), 0);
    assert_eq!(other.requests()[0].header("authorization"), None);
    assert_eq!(server.requests()[0].header("authorization"), None);
    assert_eq!(
        server.requests()[1].header("authorization"),
        Some("Bearer own")
    );
}

#[tokio::test]
async fn fails_the_request_when_the_token_source_fails() {
    // Any SDK error, as a real source would return.
    struct Failing;
    impl TokenSource for Failing {
        fn token(&self) -> BoxFuture<'_, fico_sdk::Result<String>> {
            Box::pin(async { Client::new("not a url").map(|_| String::new()) })
        }
    }
    let server = MockServer::start(vec![]).await;
    let client = Client::builder(server.url())
        .token_source(Failing)
        .build()
        .unwrap();
    let err = client.get("/x").send().await.unwrap_err();
    assert!(matches!(err, Error::InvalidUrl { .. }), "{err}");
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn replaces_the_token_while_sharing_the_client() {
    let server = MockServer::start(vec![MockResponse::new(200), MockResponse::new(200)]).await;
    let counter = Counter::default();
    let client = Client::builder(server.url())
        .token_source(counter.clone())
        .header("x-team", "t1")
        .build()
        .unwrap();
    let other = client.with_bearer_token("fixed").unwrap();
    other.get("/x").send().await.unwrap();
    client.get("/x").send().await.unwrap();

    let requests = server.requests();
    assert_eq!(requests[0].header("authorization"), Some("Bearer fixed"));
    assert_eq!(requests[0].header("x-team"), Some("t1"));
    assert_eq!(requests[1].header("authorization"), Some("Bearer token-1"));
    assert!(matches!(
        client.with_bearer_token("a\nb").unwrap_err(),
        Error::InvalidHeader { .. }
    ));
}
