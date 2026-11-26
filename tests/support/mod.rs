//! Helpers for the tests: a platform for the mock server, the decision
//! tools by name, request bodies, polling without waits, temporary
//! directories, unsigned JWTs and timestamps.
#![allow(dead_code, reason = "each test crate uses a subset")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fico_sdk::decision::PollingOptions;
use fico_sdk::decision::platform::Platform;
use fico_sdk::tool::{Context, Tool, ToolError};
use fico_sdk::{Client, RetryPolicy};
use serde_json::Value;

use crate::mock_server::{MockResponse, MockServer, RecordedRequest};

/// A platform that sends its requests to `server`, with the bearer token
/// `secret` and retries that do not wait.
pub fn platform(server: &MockServer) -> Platform {
    let retry = RetryPolicy::default()
        .backoff_factor(Duration::ZERO)
        .jitter(Duration::ZERO);
    let client = Client::builder(server.url())
        .bearer_token("secret")
        .retry(retry)
        .build()
        .unwrap();
    Platform::new(client)
}

/// The decision tool named `name`.
pub fn tool(name: &str) -> Tool {
    fico_sdk::decision::tools()
        .into_iter()
        .find(|tool| tool.name() == name)
        .unwrap_or_else(|| panic!("no tool named `{name}`"))
}

/// Calls the decision tool named `name` with `args`.
pub async fn call_tool(cx: Context, name: &str, args: Value) -> Result<Value, ToolError> {
    tool(name).call(cx, args).await
}

/// Calls the tool `name` with `args`, against a server that answers with
/// `responses`, and returns its result and the requests it sent.
pub async fn serve_tool(
    responses: Vec<MockResponse>,
    name: &str,
    args: Value,
) -> (Result<Value, ToolError>, Vec<RecordedRequest>) {
    let server = MockServer::start(responses).await;
    let cx = Context::new().with_platform(platform(&server));
    let result = call_tool(cx, name, args).await;
    (result, server.requests())
}

/// [`serve_tool`] for a call that must succeed: returns its result,
/// unwrapped, and the requests it sent.
pub async fn serve_tool_ok(
    responses: Vec<MockResponse>,
    name: &str,
    args: Value,
) -> (Value, Vec<RecordedRequest>) {
    let (result, requests) = serve_tool(responses, name, args).await;
    (result.unwrap(), requests)
}

/// The JSON body of `request`.
pub fn body(request: &RecordedRequest) -> Value {
    serde_json::from_slice(&request.body).unwrap()
}

/// `list` as owned pairs, to compare with [`RecordedRequest::query`].
pub fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Polling options that check as fast as possible, with no limits.
pub fn fast_polling() -> PollingOptions {
    PollingOptions {
        backoff: 0.0,
        delay: 0.0,
        initial_delay: 0.0,
        max_delay: 0.0,
        max_duration: None,
        max_tries: None,
        jitter: 0.0,
    }
}

/// A new empty directory, removed when dropped.
pub struct TempDir(PathBuf);

impl TempDir {
    /// A new empty directory in the system's temporary directory, with
    /// `name` in its name.
    pub fn new(name: &str) -> TempDir {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let name = format!(
            "fico-sdk-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let dir = std::env::temp_dir().join(name);
        // Left over from an earlier test process with the same ID.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A JWT whose payload is `claims`, unsigned, as an identity provider's ID
/// token: the SDK reads its claims without checking the signature.
pub fn jwt(claims: Value) -> String {
    format!(
        "{}.{}.signature",
        base64url(br#"{"alg":"RS256","typ":"JWT"}"#),
        base64url(claims.to_string().as_bytes())
    )
}

/// `bytes` in base64url without padding.
pub fn base64url(bytes: &[u8]) -> String {
    let alphabet: Vec<u8> = (b'A'..=b'Z')
        .chain(b'a'..=b'z')
        .chain(b'0'..=b'9')
        .chain(*b"-_")
        .collect();
    let mut text = String::new();
    for chunk in bytes.chunks(3) {
        let byte = |at: usize| u32::from(chunk.get(at).copied().unwrap_or(0));
        let group = byte(0) << 16 | byte(1) << 8 | byte(2);
        for at in 0..=chunk.len() {
            text.push(char::from(alphabet[(group >> (18 - 6 * at)) as usize & 63]));
        }
    }
    text
}

/// The current time, in seconds since the Unix epoch.
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// `secs` since the Unix epoch as an RFC 3339 timestamp, with microseconds
/// and a `+00:00` offset, e.g. for token expiries. The SDK's own copy of
/// this calendar is crate-private.
pub fn rfc3339(secs: i64) -> String {
    // Howard Hinnant's `civil_from_days`.
    let days = secs.div_euclid(86_400) + 719_468;
    let (era, day_of_era) = (days.div_euclid(146_097), days.rem_euclid(146_097));
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    let rest = secs.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.000000+00:00",
        rest / 3_600,
        rest / 60 % 60,
        rest % 60
    )
}
