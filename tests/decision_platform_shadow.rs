//! Tests for the platform shadow test operations and their tools, against
//! the mock server.

mod mock_server;
mod support;

use fico_sdk::Error;
use fico_sdk::decision::platform::{
    ShadowCreateParams, ShadowDeleteParams, ShadowStopParams, ShadowUpdateParams,
};
use fico_sdk::decision::{
    ExperimentStatus, StartEvents, StopIntent, TerminationEvents, TestComparison,
};
use fico_sdk::tool::ToolError;
use mock_server::{MockResponse, MockServer};
use serde_json::{Value, json};

const SHADOWS: &str = "/v1/applications/routing/experiments/shadow";
const SHADOW: &str = "/v1/applications/routing/experiments/shadow/shadow-1";

/// A shadow test as the platform returns it from a create or a list: the
/// comparisons as a list, and both event objects, even when empty.
fn shadow_json() -> Value {
    json!({
        "id": "shadow-1",
        "name": "Faster routes",
        "description": "Staging against production.",
        "app_id": "routing",
        "created_at": "2026-10-13T14:00:00Z",
        "updated_at": "2026-10-13T14:00:00Z",
        "status": "draft",
        "comparisons": [
            { "baseline_instance_id": "production", "candidate_instance_ids": ["staging"] },
        ],
        "start_events": {},
        "termination_events": { "maximum_runs": 100, "time": "2026-10-20T14:00:00Z" },
    })
}

/// A started shadow test as `GET X/shadow/{id}` returns it, with the
/// summaries of its runs' metrics.
fn shadow_result_json() -> Value {
    let mut shadow = shadow_json();
    shadow["status"] = json!("started");
    shadow["grouped_distributional_summaries"] = json!([{
        "group_keys": ["instanceID", "versionID"],
        "group_values": ["production", "v1"],
        "indicator_keys": ["cost"],
        "indicator_distributions": {
            "cost": {
                "min": 10.0,
                "max": 12.5,
                "count": 2,
                "mean": 11.25,
                "std": 1.25,
                "shifted_geometric_mean": { "value": 11.18, "shift": 10.0 },
                "percentiles": {
                    "p01": 10.0, "p05": 10.0, "p10": 10.0, "p25": 10.0, "p50": 11.25,
                    "p75": 12.5, "p90": 12.5, "p95": 12.5, "p99": 12.5,
                },
            },
        },
        "number_of_runs_total": 2,
    }]);
    shadow
}

/// A run of the shadow test as `GET …/runs` lists it, with the legacy
/// `status` next to `status_v2`.
fn run_json(id: &str, environment_id: &str) -> Value {
    json!({
        "id": id,
        "user_email": "dev@example.com",
        "name": "",
        "description": "",
        "created_at": "2026-10-13T14:05:00Z",
        "application_id": "routing",
        "application_instance_id": environment_id,
        "application_version_id": "v1",
        "run_type": { "type": "standard", "definition_id": "", "reference_id": "" },
        "execution_class": "6c9500mb870s",
        "runtime": "ghcr.io/nextmv-io/runtime/default:latest",
        "status": "succeeded",
        "status_v2": "succeeded",
        "experiment_id": "shadow-1",
    })
}

/// Shadow test metadata as `GET …/metadata` returns it.
fn metadata_json() -> Value {
    json!({
        "id": "shadow-1",
        "name": "Faster routes",
        "description": "Staging against production.",
        "app_id": "routing",
        "created_at": "2026-10-13T14:00:00Z",
        "updated_at": "2026-10-13T15:00:00Z",
        "status": "started",
    })
}

fn comparison(baseline: &str, candidates: &[&str]) -> TestComparison {
    TestComparison {
        baseline_environment_id: baseline.to_owned(),
        candidate_environment_ids: candidates.iter().map(|c| c.to_string()).collect(),
    }
}

/// Parameters with only the required fields set: one comparison, and at
/// most `maximum_runs` runs.
fn required_params(maximum_runs: u32) -> ShadowCreateParams {
    ShadowCreateParams {
        shadow_id: None,
        name: None,
        description: None,
        comparisons: vec![comparison("production", &["staging"])],
        termination_events: TerminationEvents {
            maximum_runs,
            ..Default::default()
        },
        start_events: None,
    }
}

#[tokio::test]
async fn creates_a_shadow_test() {
    let server = MockServer::start(vec![MockResponse::json(200, shadow_json())]).await;
    let params = ShadowCreateParams {
        shadow_id: Some("shadow-1".to_owned()),
        name: Some("Faster routes".to_owned()),
        description: Some("Staging against production.".to_owned()),
        comparisons: vec![
            comparison("production", &["staging", "canary"]),
            comparison("legacy", &["next"]),
        ],
        termination_events: TerminationEvents {
            maximum_runs: 100,
            time: Some("2026-10-20T14:00:00Z".to_owned()),
        },
        start_events: Some(StartEvents {
            time: Some("2026-10-14T08:00:00+02:00".to_owned()),
        }),
    };
    let shadow = support::platform(&server)
        .create_shadow("routing", params)
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].target, SHADOWS);
    // The comparisons go as a map from each baseline to its candidates.
    assert_eq!(
        support::body(&requests[0]),
        json!({
            "id": "shadow-1",
            "name": "Faster routes",
            "description": "Staging against production.",
            "comparisons": { "production": ["staging", "canary"], "legacy": ["next"] },
            "termination_events": { "maximum_runs": 100, "time": "2026-10-20T14:00:00Z" },
            "start_events": { "time": "2026-10-14T08:00:00+02:00" },
        })
    );
    assert_eq!(shadow.shadow_id.as_deref(), Some("shadow-1"));
    assert_eq!(shadow.workspace_id.as_deref(), Some("routing"));
    assert_eq!(shadow.status, Some(ExperimentStatus::Draft));
    assert_eq!(
        shadow.comparisons,
        Some(vec![comparison("production", &["staging"])])
    );
    assert_eq!(shadow.start_events, Some(StartEvents::default()));
    let termination = shadow.termination_events.unwrap();
    assert_eq!(termination.maximum_runs, 100);
    assert_eq!(termination.time.as_deref(), Some("2026-10-20T14:00:00Z"));
    assert_eq!(shadow.runs, None);
}

#[tokio::test]
async fn sends_only_the_required_fields_when_nothing_else_is_set() {
    let server = MockServer::start(vec![MockResponse::json(200, shadow_json())]).await;
    let params = ShadowCreateParams {
        shadow_id: Some("shadow-1".to_owned()),
        ..required_params(1)
    };
    support::platform(&server)
        .create_shadow("routing", params)
        .await
        .unwrap();
    assert_eq!(
        support::body(&server.requests()[0]),
        json!({
            "id": "shadow-1",
            "name": "shadow-1",
            "comparisons": { "production": ["staging"] },
            "termination_events": { "maximum_runs": 1 },
        })
    );
}

#[tokio::test]
async fn generates_an_id_when_none_is_set() {
    let server = MockServer::start(vec![MockResponse::json(200, shadow_json())]).await;
    let params = ShadowCreateParams {
        shadow_id: Some(String::new()),
        name: Some(String::new()),
        ..required_params(300)
    };
    support::platform(&server)
        .create_shadow("routing", params)
        .await
        .unwrap();
    let body = support::body(&server.requests()[0]);
    let id = body["id"].as_str().unwrap();
    let suffix = id.strip_prefix("shadow-").unwrap();
    assert_eq!(suffix.len(), 8, "{id}");
    assert!(
        suffix
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
        "{id}"
    );
    assert_eq!(body["name"], id);
}

#[tokio::test]
async fn needs_between_1_and_300_maximum_runs() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    for maximum_runs in [0, 301] {
        let err = platform
            .create_shadow("routing", required_params(maximum_runs))
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Invalid { .. }),
            "{maximum_runs}: {err}"
        );
        assert_eq!(
            err.to_string(),
            "invalid argument: `termination_events.maximum_runs` must be between 1 and 300"
        );
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn needs_rfc3339_times() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    let mut params = required_params(10);
    params.termination_events.time = Some("2026-10-20".to_owned());
    let err = platform.create_shadow("routing", params).await.unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "invalid argument: `termination_events.time` must be an RFC 3339 timestamp, such as \
         `2026-10-01T12:00:00Z`"
    );
    let params = ShadowCreateParams {
        start_events: Some(StartEvents {
            time: Some("tomorrow".to_owned()),
        }),
        ..required_params(10)
    };
    let err = platform.create_shadow("routing", params).await.unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert!(err.to_string().contains("`start_events.time`"), "{err}");
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn leaves_out_empty_times() {
    let server = MockServer::start(vec![MockResponse::json(200, shadow_json())]).await;
    let mut params = ShadowCreateParams {
        shadow_id: Some("shadow-1".to_owned()),
        start_events: Some(StartEvents {
            time: Some(String::new()),
        }),
        ..required_params(10)
    };
    params.termination_events.time = Some(String::new());
    support::platform(&server)
        .create_shadow("routing", params)
        .await
        .unwrap();
    // An empty time counts as unset, as the platform reads it.
    assert_eq!(
        support::body(&server.requests()[0]),
        json!({
            "id": "shadow-1",
            "name": "shadow-1",
            "comparisons": { "production": ["staging"] },
            "termination_events": { "maximum_runs": 10 },
            "start_events": {},
        })
    );
}

#[tokio::test]
async fn rejects_a_baseline_in_two_comparisons() {
    let server = MockServer::start(Vec::new()).await;
    let params = ShadowCreateParams {
        comparisons: vec![
            comparison("production", &["staging"]),
            comparison("production", &["canary"]),
        ],
        ..required_params(10)
    };
    let err = support::platform(&server)
        .create_shadow("routing", params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn gets_a_shadow_test_with_all_its_runs() {
    let server = MockServer::start(vec![
        MockResponse::json(200, shadow_result_json()),
        MockResponse::json(
            200,
            json!({
                "runs": [run_json("run-1", "production"), run_json("run-2", "staging")],
                "next_page_token": "page-2",
            }),
        ),
        MockResponse::json(200, json!({ "runs": [run_json("run-3", "production")] })),
    ])
    .await;
    let shadow = support::platform(&server)
        .get_shadow("routing", "shadow-1")
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].target, SHADOW);
    let runs_path = format!("{SHADOW}/runs");
    for request in &requests[1..] {
        assert_eq!(request.method, "GET");
        assert_eq!(request.target.split_once('?').unwrap().0, runs_path);
    }
    assert_eq!(
        requests[1].query(),
        support::pairs(&[("pagereturn", "true")])
    );
    assert_eq!(
        requests[2].query(),
        support::pairs(&[("pagereturn", "true"), ("pagetoken", "page-2")])
    );

    assert_eq!(shadow.status, Some(ExperimentStatus::Started));
    let summaries = shadow.grouped_distributional_summaries.as_ref().unwrap();
    assert_eq!(summaries[0]["number_of_runs_total"], 2);
    let runs = shadow.runs.as_ref().unwrap();
    let ids: Vec<&str> = runs.iter().map(|run| run.id.as_str()).collect();
    assert_eq!(ids, ["run-1", "run-2", "run-3"]);
    assert_eq!(runs[1].environment_id, "staging");
    // Emitted JSON uses the API names, with the runs attached.
    let wire = serde_json::to_value(&shadow).unwrap();
    assert_eq!(wire["id"], "shadow-1");
    assert_eq!(wire["app_id"], "routing");
    assert_eq!(wire["comparisons"][0]["baseline_instance_id"], "production");
    assert_eq!(wire["runs"][2]["application_instance_id"], "production");
    assert!(wire.get("shadow_id").is_none(), "{wire}");
    assert!(wire.get("workspace_id").is_none(), "{wire}");
}

#[tokio::test]
async fn decodes_a_failed_shadow_test_without_runs() {
    let mut failed = shadow_result_json();
    failed["status"] = json!("failed");
    failed["completed_at"] = json!("2026-10-13T16:00:00.123456789Z");
    failed["grouped_distributional_summaries"] = Value::Null;
    let server = MockServer::start(vec![
        MockResponse::json(200, failed),
        MockResponse::json(200, json!({ "runs": [] })),
    ])
    .await;
    let shadow = support::platform(&server)
        .get_shadow("routing", "shadow-1")
        .await
        .unwrap();
    assert_eq!(shadow.status, Some(ExperimentStatus::Failed));
    assert_eq!(
        shadow.completed_at.as_deref(),
        Some("2026-10-13T16:00:00.123456789Z")
    );
    assert_eq!(shadow.grouped_distributional_summaries, None);
    assert_eq!(shadow.runs, Some(Vec::new()));
}

#[tokio::test]
async fn lists_shadow_tests_across_pages() {
    let mut second = shadow_json();
    second["id"] = json!("shadow-2");
    let server = MockServer::start(vec![
        MockResponse::json(
            200,
            json!({ "items": [shadow_json()], "next_page_token": "page-2" }),
        ),
        MockResponse::json(200, json!({ "items": [second] })),
    ])
    .await;
    let shadows = support::platform(&server)
        .list_shadows("routing")
        .await
        .unwrap();
    let ids: Vec<Option<&str>> = shadows.iter().map(|s| s.shadow_id.as_deref()).collect();
    assert_eq!(ids, [Some("shadow-1"), Some("shadow-2")]);
    assert!(shadows.iter().all(|shadow| shadow.runs.is_none()));

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.method, "GET");
        assert_eq!(request.target.split_once('?').unwrap().0, SHADOWS);
    }
    assert_eq!(
        requests[0].query(),
        support::pairs(&[("pagereturn", "true")])
    );
    assert_eq!(
        requests[1].query(),
        support::pairs(&[("pagereturn", "true"), ("pagetoken", "page-2")])
    );
}

#[tokio::test]
async fn gets_shadow_test_metadata() {
    let server = MockServer::start(vec![MockResponse::json(200, metadata_json())]).await;
    let metadata = support::platform(&server)
        .get_shadow_metadata("routing", "shadow-1")
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "GET");
    assert_eq!(request.target, format!("{SHADOW}/metadata"));
    assert_eq!(metadata.shadow_id.as_deref(), Some("shadow-1"));
    assert_eq!(metadata.workspace_id.as_deref(), Some("routing"));
    assert_eq!(metadata.status, Some(ExperimentStatus::Started));
    assert_eq!(serde_json::to_value(&metadata).unwrap(), metadata_json());
}

#[tokio::test]
async fn starts_a_shadow_test() {
    // The platform answers `304 Not Modified` when the test already started.
    let server = MockServer::start(vec![MockResponse::new(204), MockResponse::new(304)]).await;
    let platform = support::platform(&server);
    platform.start_shadow("routing", "shadow-1").await.unwrap();
    platform.start_shadow("routing", "shadow-1").await.unwrap();
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.method, "PUT");
        assert_eq!(request.target, format!("{SHADOW}/start"));
        assert!(request.body.is_empty());
    }
}

#[tokio::test]
async fn stops_a_shadow_test() {
    let server = MockServer::start(vec![MockResponse::new(204)]).await;
    let params = ShadowStopParams {
        intent: StopIntent::Cancel,
        polling: None,
    };
    support::platform(&server)
        .stop_shadow("routing", "shadow-1", params)
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "PUT");
    assert_eq!(request.target, format!("{SHADOW}/stop"));
    assert_eq!(support::body(request), json!({ "intent": "cancel" }));
}

/// [`metadata_json`] with `status`.
fn with_status(status: &str) -> Value {
    let mut metadata = metadata_json();
    metadata["status"] = json!(status);
    metadata
}

#[tokio::test]
async fn polls_a_stop_until_the_shadow_test_is_terminal() {
    let server = MockServer::start(vec![
        MockResponse::new(204),
        MockResponse::json(200, with_status("stopping")),
        MockResponse::json(200, with_status("canceled")),
    ])
    .await;
    let params = ShadowStopParams {
        intent: StopIntent::Cancel,
        polling: Some(support::fast_polling()),
    };
    support::platform(&server)
        .stop_shadow("routing", "shadow-1", params)
        .await
        .unwrap();
    let calls: Vec<_> = server
        .requests()
        .into_iter()
        .map(|r| format!("{} {}", r.method, r.target))
        .collect();
    assert_eq!(
        calls,
        [
            format!("PUT {SHADOW}/stop"),
            format!("GET {SHADOW}/metadata"),
            format!("GET {SHADOW}/metadata"),
        ]
    );
}

#[tokio::test]
async fn polls_a_delete_until_the_shadow_test_is_gone() {
    let server = MockServer::start(vec![
        MockResponse::new(204),
        MockResponse::json(200, with_status("deleting")),
        MockResponse::new(404).body("shadow test not found"),
    ])
    .await;
    let params = ShadowDeleteParams {
        polling: Some(support::fast_polling()),
    };
    support::platform(&server)
        .delete_shadow("routing", "shadow-1", params)
        .await
        .unwrap();
    let calls: Vec<_> = server
        .requests()
        .into_iter()
        .map(|r| format!("{} {}", r.method, r.target))
        .collect();
    assert_eq!(
        calls,
        [
            format!("DELETE {SHADOW}"),
            format!("GET {SHADOW}/metadata"),
            format!("GET {SHADOW}/metadata"),
        ]
    );
}

#[tokio::test]
async fn ends_the_polling_of_a_failed_delete_with_an_error() {
    let server = MockServer::start(vec![
        MockResponse::new(204),
        MockResponse::json(200, with_status("delete-failed")),
    ])
    .await;
    let params = ShadowDeleteParams {
        polling: Some(support::fast_polling()),
    };
    let err = support::platform(&server)
        .delete_shadow("routing", "shadow-1", params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Wait { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "stopped waiting: the platform could not delete the shadow test"
    );
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn updates_only_the_fields_that_are_set() {
    // The platform answers an update with these fields only.
    let updated = json!({
        "id": "shadow-1",
        "name": "Faster routes",
        "description": "Staging and canary against production.",
        "created_at": "2026-10-13T14:00:00Z",
        "updated_at": "2026-10-13T15:00:00Z",
    });
    let server = MockServer::start(vec![MockResponse::json(200, updated)]).await;
    let params = ShadowUpdateParams {
        description: Some("Staging and canary against production.".to_owned()),
        ..Default::default()
    };
    let shadow = support::platform(&server)
        .update_shadow("routing", "shadow-1", params)
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "PATCH");
    assert_eq!(requests[0].target, SHADOW);
    assert_eq!(
        support::body(&requests[0]),
        json!({ "description": "Staging and canary against production." })
    );
    assert_eq!(shadow.name.as_deref(), Some("Faster routes"));
    assert_eq!(shadow.updated_at.as_deref(), Some("2026-10-13T15:00:00Z"));
    assert_eq!(shadow.status, None);
}

#[tokio::test]
async fn rejects_an_empty_update() {
    let server = MockServer::start(Vec::new()).await;
    let err = support::platform(&server)
        .update_shadow("routing", "shadow-1", ShadowUpdateParams::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "invalid argument: an update needs `name` or `description`"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn deletes_a_shadow_test() {
    let server = MockServer::start(vec![MockResponse::new(204)]).await;
    support::platform(&server)
        .delete_shadow("routing", "shadow-1", Default::default())
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "DELETE");
    assert_eq!(request.target, SHADOW);
}

#[tokio::test]
async fn encodes_shadow_ids_in_paths() {
    let server = MockServer::start(vec![MockResponse::json(200, metadata_json())]).await;
    let platform = support::platform(&server);
    platform
        .get_shadow_metadata("my workspace", "shadow?1")
        .await
        .unwrap();
    assert_eq!(
        server.requests()[0].target,
        "/v1/applications/my%20workspace/experiments/shadow/shadow%3F1/metadata"
    );
    for (workspace_id, shadow_id) in [
        ("routing", ".."),
        ("..", "shadow-1"),
        ("routing", ""),
        ("routing", "shadow/1"),
    ] {
        let err = platform
            .start_shadow(workspace_id, shadow_id)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{shadow_id:?}: {err}");
        let err = platform
            .delete_shadow(workspace_id, shadow_id, Default::default())
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{shadow_id:?}: {err}");
    }
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn serves_the_create_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::json(200, shadow_json())],
        "decision_platform_shadow_create",
        json!({
            "workspace_id": "routing",
            "shadow_id": "shadow-1",
            "comparisons": [
                { "baseline_instance_id": "production", "candidate_instance_ids": ["staging"] },
            ],
            "termination_events": { "maximum_runs": 100 },
            "start_events": { "time": "2026-10-14T06:00:00Z" },
        }),
    )
    .await;
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].target, SHADOWS);
    assert_eq!(
        support::body(&requests[0]),
        json!({
            "id": "shadow-1",
            "name": "shadow-1",
            "comparisons": { "production": ["staging"] },
            "termination_events": { "maximum_runs": 100 },
            "start_events": { "time": "2026-10-14T06:00:00Z" },
        })
    );
    assert_eq!(result.unwrap()["status"], "draft");
}

#[tokio::test]
async fn needs_comparisons_and_termination_events_in_the_create_tool() {
    let schema = support::tool("decision_platform_shadow_create")
        .input_schema()
        .clone();
    let required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for field in ["workspace_id", "comparisons", "termination_events"] {
        assert!(required.contains(&field), "{field}: {required:?}");
    }
    for field in ["shadow_id", "name", "description", "start_events"] {
        assert!(!required.contains(&field), "{field}: {required:?}");
    }
    let (result, requests) = support::serve_tool(
        Vec::new(),
        "decision_platform_shadow_create",
        json!({ "workspace_id": "routing", "comparisons": [] }),
    )
    .await;
    assert!(
        matches!(result, Err(ToolError::InvalidArguments(_))),
        "{result:?}"
    );
    assert!(requests.is_empty());
}

#[tokio::test]
async fn advertises_the_range_of_maximum_runs_in_the_create_tool() {
    let schema = support::tool("decision_platform_shadow_create")
        .input_schema()
        .clone();
    let maximum_runs = &schema["properties"]["termination_events"]["properties"]["maximum_runs"];
    assert_eq!(maximum_runs["minimum"], 1, "{maximum_runs}");
    assert_eq!(maximum_runs["maximum"], 300, "{maximum_runs}");
}

#[tokio::test]
async fn serves_the_get_tool() {
    let (result, requests) = support::serve_tool(
        vec![
            MockResponse::json(200, shadow_result_json()),
            MockResponse::json(200, json!({ "runs": [run_json("run-1", "production")] })),
        ],
        "decision_platform_shadow_get",
        json!({ "workspace_id": "routing", "shadow_id": "shadow-1" }),
    )
    .await;
    assert_eq!(requests[0].target, SHADOW);
    assert_eq!(requests[1].target, format!("{SHADOW}/runs?pagereturn=true"));
    let result = result.unwrap();
    assert_eq!(result["status"], "started");
    assert_eq!(result["runs"][0]["id"], "run-1");
}

#[tokio::test]
async fn serves_the_list_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::json(200, json!({ "items": [shadow_json()] }))],
        "decision_platform_shadow_list",
        json!({ "workspace_id": "routing" }),
    )
    .await;
    assert_eq!(requests[0].target, format!("{SHADOWS}?pagereturn=true"));
    assert_eq!(result.unwrap()[0]["id"], "shadow-1");
}

#[tokio::test]
async fn serves_the_metadata_get_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::json(200, metadata_json())],
        "decision_platform_shadow_metadata_get",
        json!({ "workspace_id": "routing", "shadow_id": "shadow-1" }),
    )
    .await;
    assert_eq!(requests[0].target, format!("{SHADOW}/metadata"));
    assert_eq!(result.unwrap(), metadata_json());
}

#[tokio::test]
async fn serves_the_start_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::new(204)],
        "decision_platform_shadow_start",
        json!({ "workspace_id": "routing", "shadow_id": "shadow-1" }),
    )
    .await;
    assert_eq!(requests[0].method, "PUT");
    assert_eq!(requests[0].target, format!("{SHADOW}/start"));
    assert_eq!(result.unwrap(), Value::Null);
}

#[tokio::test]
async fn serves_the_stop_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::new(204)],
        "decision_platform_shadow_stop",
        json!({ "workspace_id": "routing", "shadow_id": "shadow-1", "intent": "complete" }),
    )
    .await;
    assert_eq!(requests[0].method, "PUT");
    assert_eq!(requests[0].target, format!("{SHADOW}/stop"));
    assert_eq!(support::body(&requests[0]), json!({ "intent": "complete" }));
    assert_eq!(result.unwrap(), Value::Null);
}

#[tokio::test]
async fn needs_an_intent_in_the_stop_tool() {
    let schema = support::tool("decision_platform_shadow_stop")
        .input_schema()
        .clone();
    assert!(
        schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("intent")),
        "{schema:?}"
    );
    for args in [
        json!({ "workspace_id": "routing", "shadow_id": "shadow-1" }),
        json!({ "workspace_id": "routing", "shadow_id": "shadow-1", "intent": "pause" }),
    ] {
        let (result, requests) =
            support::serve_tool(Vec::new(), "decision_platform_shadow_stop", args).await;
        assert!(
            matches!(result, Err(ToolError::InvalidArguments(_))),
            "{result:?}"
        );
        assert!(requests.is_empty());
    }
}

#[tokio::test]
async fn serves_the_update_tool() {
    let mut updated = shadow_json();
    updated["name"] = json!("Fastest routes");
    let (result, requests) = support::serve_tool(
        vec![MockResponse::json(200, updated)],
        "decision_platform_shadow_update",
        json!({ "workspace_id": "routing", "shadow_id": "shadow-1", "name": "Fastest routes" }),
    )
    .await;
    assert_eq!(requests[0].method, "PATCH");
    assert_eq!(requests[0].target, SHADOW);
    assert_eq!(
        support::body(&requests[0]),
        json!({ "name": "Fastest routes" })
    );
    assert_eq!(result.unwrap()["name"], "Fastest routes");
}

#[tokio::test]
async fn serves_the_delete_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::new(204)],
        "decision_platform_shadow_delete",
        json!({ "workspace_id": "routing", "shadow_id": "shadow-1" }),
    )
    .await;
    assert_eq!(requests[0].method, "DELETE");
    assert_eq!(requests[0].target, SHADOW);
    assert_eq!(result.unwrap(), Value::Null);
}
