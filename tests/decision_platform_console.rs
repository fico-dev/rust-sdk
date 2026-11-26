//! Tests for the console links and the `console_url` of the platform run
//! reads, against the mock server.

mod mock_server;
mod support;

use fico_sdk::decision::platform::{DEFAULT_CONSOLE_URL, Platform, RunGetParams};
use fico_sdk::tool::Context;
use mock_server::{MockResponse, MockServer};
use serde_json::{Value, json};

const BASE: &str = "https://console.example.com";

/// A platform with no server behind it, for the links, which make no
/// request.
fn offline() -> Platform {
    Platform::new(fico_sdk::Client::new("https://api.example.com").unwrap())
}

/// A succeeded run's information, as `GET …/runs/run-1/metadata` returns
/// it: the platform sends no `console_url`.
fn information_json() -> Value {
    json!({
        "id": "run-1",
        "user_email": "ana@example.com",
        "name": "Monday",
        "description": "",
        "metadata": {
            "status": "succeeded",
            "status_v2": "succeeded",
            "created_at": "2026-10-07T14:19:02Z",
            "duration": 5021,
            "input_size": 1024,
            "output_size": 2048,
            "error": "",
            "application_id": "routing",
            "application_instance_id": "production",
            "application_version_id": "v1",
            "execution_class": "6c9500mb870s",
            "runtime": "ghcr.io/nextmv-io/runtime/default:latest",
            "run_type": { "type": "standard", "definition_id": "", "reference_id": "" },
            "format": { "input": { "type": "json" }, "output": { "type": "json" } },
        },
    })
}

/// `information_json` with its output, as `GET …/runs/run-1` returns it.
fn result_json() -> Value {
    let mut result = information_json();
    result["output"] = json!({ "metrics": { "cost": 12.5 } });
    result
}

#[test]
fn links_a_run_and_a_workspace() {
    let platform = offline().with_console_url(BASE);
    assert_eq!(
        platform.run_console_url("routing", "run-1").as_deref(),
        Some("https://console.example.com/app/routing/run/run-1?view=details")
    );
    assert_eq!(
        platform.workspace_console_url("routing").as_deref(),
        Some("https://console.example.com/app/routing/runs")
    );
}

#[test]
fn encodes_the_ids() {
    let platform = offline().with_console_url(BASE);
    assert_eq!(
        platform.run_console_url("my ws", "run%1?x#y").as_deref(),
        Some("https://console.example.com/app/my%20ws/run/run%251%3Fx%23y?view=details")
    );
    assert_eq!(
        platform.workspace_console_url("é..").as_deref(),
        Some("https://console.example.com/app/%C3%A9../runs")
    );
}

#[test]
fn drops_trailing_slashes_from_the_base() {
    let platform = offline().with_console_url("https://console.example.com/fico//");
    assert_eq!(
        platform.workspace_console_url("routing").as_deref(),
        Some("https://console.example.com/fico/app/routing/runs")
    );
}

#[test]
fn makes_no_link_without_a_base() {
    for platform in [
        offline().with_console_url(""),
        offline().with_console_url("/"),
    ] {
        assert_eq!(platform.run_console_url("routing", "run-1"), None);
        assert_eq!(platform.workspace_console_url("routing"), None);
    }
}

#[test]
fn starts_from_the_default_console_url() {
    let platform = offline();
    let expected = DEFAULT_CONSOLE_URL.map(|base| {
        let base = base.trim_end_matches('/');
        format!("{base}/app/routing/run/run-1?view=details")
    });
    assert_eq!(platform.run_console_url("routing", "run-1"), expected);
}

#[test]
fn makes_no_link_for_ids_that_change_the_path() {
    let platform = offline().with_console_url(BASE);
    for id in ["", ".", "..", "a/b", "../x", "a\\b"] {
        assert_eq!(platform.run_console_url("routing", id), None, "{id:?}");
        assert_eq!(platform.run_console_url(id, "run-1"), None, "{id:?}");
        assert_eq!(platform.workspace_console_url(id), None, "{id:?}");
    }
}

#[tokio::test]
async fn fills_the_console_url_of_run_information() {
    let server = MockServer::start(vec![MockResponse::json(200, information_json())]).await;
    let information = support::platform(&server)
        .with_console_url(BASE)
        .get_run_information("routing", "run-1")
        .await
        .unwrap();
    assert_eq!(
        information.console_url,
        "https://console.example.com/app/routing/run/run-1?view=details"
    );
}

#[tokio::test]
async fn fills_the_console_url_of_a_run() {
    let server = MockServer::start(vec![
        MockResponse::json(200, information_json()),
        MockResponse::json(200, result_json()),
    ])
    .await;
    let result = support::platform(&server)
        .with_console_url(BASE)
        .get_run("routing", "run-1", RunGetParams::default())
        .await
        .unwrap();
    assert_eq!(
        result.information.console_url,
        "https://console.example.com/app/routing/run/run-1?view=details"
    );
    let emitted = serde_json::to_value(&result).unwrap();
    assert_eq!(emitted["console_url"], result.information.console_url);
}

#[tokio::test]
async fn leaves_the_console_url_without_a_base() {
    let server = MockServer::start(vec![
        MockResponse::json(200, information_json()),
        MockResponse::json(200, information_json()),
        MockResponse::json(200, result_json()),
    ])
    .await;
    let platform = support::platform(&server).with_console_url("");
    let information = platform
        .get_run_information("routing", "run-1")
        .await
        .unwrap();
    assert_eq!(information.console_url, "");
    let result = platform
        .get_run("routing", "run-1", RunGetParams::default())
        .await
        .unwrap();
    assert_eq!(result.information.console_url, "");
    assert_eq!(server.requests().len(), 3);
}

#[tokio::test]
async fn serves_the_console_url_in_the_information_tool() {
    let server = MockServer::start(vec![MockResponse::json(200, information_json())]).await;
    let cx = Context::new().with_platform(support::platform(&server).with_console_url(BASE));
    let args = json!({ "workspace_id": "routing", "run_id": "run-1" });
    let result = support::call_tool(cx, "decision_platform_run_information_get", args)
        .await
        .unwrap();
    assert_eq!(
        result["console_url"],
        "https://console.example.com/app/routing/run/run-1?view=details"
    );
}
