//! Tests for the platform switchback test operations and their tools,
//! against the mock server.

mod mock_server;
mod support;

use fico_sdk::Error;
use fico_sdk::decision::platform::{
    SwitchbackCreateParams, SwitchbackDeleteParams, SwitchbackStopParams, SwitchbackUpdateParams,
};
use fico_sdk::decision::{ExperimentStatus, RunStatus, StopIntent, TestComparisonSingle};
use fico_sdk::tool::ToolError;
use mock_server::{MockResponse, MockServer};
use serde_json::{Value, json};

/// Path of the switchback tests of the `routing` workspace.
const TESTS: &str = "/v1/applications/routing/experiments/switchback";

/// A switchback test as the platform's create returns it.
fn created_json() -> Value {
    json!({
        "id": "switchback-1",
        "name": "Rainy days",
        "description": "Candidate against production.",
        "created_at": "2026-10-13T14:00:00Z",
        "updated_at": "2026-10-13T14:00:00Z",
        "status": "draft",
        "comparison": {
            "baseline_instance_id": "production",
            "candidate_instance_id": "candidate",
        },
        "plan": {
            "start": "2026-10-14T00:00:00Z",
            "units": [
                { "duration_minutes": 60.0, "instance_id": "candidate", "index": 0 },
                { "duration_minutes": 60.0, "instance_id": "production", "index": 1 },
            ],
        },
    })
}

/// A switchback test as the platform's get returns it, with a field the SDK
/// does not model (`grouped_distributional_summaries`).
fn test_json() -> Value {
    let mut test = created_json();
    test["status"] = json!("started");
    test["started_at"] = json!("2026-10-14T00:00:00Z");
    test["grouped_distributional_summaries"] = json!([]);
    test
}

/// A switchback test as the platform's list returns it: no plan.
fn list_item_json(id: &str) -> Value {
    let mut test = created_json();
    test["id"] = json!(id);
    test.as_object_mut().unwrap().remove("plan");
    test
}

/// A run of the switchback test, with a field the SDK does not model
/// (`switchback_experiment`).
fn run_json(id: &str) -> Value {
    json!({
        "id": id,
        "user_email": "ana@example.com",
        "name": "",
        "description": "",
        "created_at": "2026-10-14T00:05:00Z",
        "application_id": "routing",
        "application_instance_id": "candidate",
        "application_version_id": "v2",
        "run_type": { "type": "standard", "definition_id": "", "reference_id": "" },
        "execution_class": "6c9500mb870s",
        "runtime": "ghcr.io/nextmv-io/runtime/default:latest",
        "status": "succeeded",
        "status_v2": "succeeded",
        "experiment_id": "switchback-1",
        "switchback_experiment": { "experiment_id": "switchback-1", "plan_unit_index": 0 },
    })
}

/// Metadata of a switchback test as the platform returns it.
fn metadata_json() -> Value {
    json!({
        "id": "switchback-1",
        "name": "Rainy days",
        "description": "Candidate against production.",
        "app_id": "routing",
        "created_at": "2026-10-13T14:00:00Z",
        "updated_at": "2026-10-13T14:00:00Z",
        "status": "draft",
    })
}

/// What the platform's update returns.
fn updated_json() -> Value {
    json!({
        "id": "switchback-1",
        "name": "Rainy days",
        "description": "Version 2 against production.",
        "created_at": "2026-10-13T14:00:00Z",
        "updated_at": "2026-10-13T15:00:00Z",
    })
}

/// Create parameters with only the required fields set.
fn params() -> SwitchbackCreateParams {
    SwitchbackCreateParams {
        switchback_id: None,
        name: None,
        description: None,
        comparison: TestComparisonSingle {
            baseline_environment_id: "production".to_owned(),
            candidate_environment_id: "candidate".to_owned(),
        },
        unit_duration_minutes: 60.0,
        units: 2,
        start: None,
    }
}

#[tokio::test]
async fn creates_a_switchback_test() {
    let server = MockServer::start(vec![MockResponse::json(200, created_json())]).await;
    let params = SwitchbackCreateParams {
        switchback_id: Some("switchback-1".to_owned()),
        name: Some("Rainy days".to_owned()),
        description: Some("Candidate against production.".to_owned()),
        start: Some("2026-10-14T00:00:00Z".to_owned()),
        ..params()
    };
    let test = support::platform(&server)
        .create_switchback("routing", params)
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].target, TESTS);
    assert_eq!(
        support::body(&requests[0]),
        json!({
            "id": "switchback-1",
            "name": "Rainy days",
            "description": "Candidate against production.",
            "comparison": {
                "baseline_instance_id": "production",
                "candidate_instance_id": "candidate",
            },
            "generate_random_plan": {
                "unit_duration_minutes": 60.0,
                "units": 2,
                "start": "2026-10-14T00:00:00Z",
            },
        })
    );
    assert_eq!(test.switchback_id.as_deref(), Some("switchback-1"));
    assert_eq!(test.status, Some(ExperimentStatus::Draft));
    let comparison = test.comparison.unwrap();
    assert_eq!(comparison.baseline_environment_id, "production");
    assert_eq!(comparison.candidate_environment_id, "candidate");
    let plan = test.plan.unwrap();
    assert_eq!(plan.start.as_deref(), Some("2026-10-14T00:00:00Z"));
    let units = plan.units.unwrap();
    assert_eq!(units.len(), 2);
    assert_eq!(units[1].environment_id, "production");
    assert_eq!(units[1].index, 1);
    assert_eq!(units[1].duration_minutes, 60.0);
    assert_eq!(test.workspace_id, None);
    assert_eq!(test.runs, None);
}

#[tokio::test]
async fn generates_the_id_and_name_when_unset_or_empty() {
    let empty = SwitchbackCreateParams {
        switchback_id: Some(String::new()),
        name: Some(String::new()),
        start: Some(String::new()),
        ..params()
    };
    for params in [params(), empty] {
        let server = MockServer::start(vec![MockResponse::json(200, created_json())]).await;
        support::platform(&server)
            .create_switchback("routing", params)
            .await
            .unwrap();
        let body = support::body(&server.requests()[0]);
        let id = body["id"].as_str().unwrap();
        let suffix = id.strip_prefix("switchback-").unwrap();
        assert_eq!(suffix.len(), 8, "{id}");
        assert!(
            suffix
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
            "{id}"
        );
        assert_eq!(body["name"], id);
        // Optional keys are left out, and an empty `start` counts as unset.
        assert_eq!(body.get("description"), None);
        assert_eq!(
            body["generate_random_plan"],
            json!({ "unit_duration_minutes": 60.0, "units": 2 })
        );
    }
}

#[tokio::test]
async fn checks_the_plan_ranges() {
    let server = MockServer::start(vec![
        MockResponse::json(200, created_json()),
        MockResponse::json(200, created_json()),
    ])
    .await;
    let platform = support::platform(&server);
    let minutes = "invalid argument: `unit_duration_minutes` must be between 1 and 10080";
    let units = "invalid argument: `units` must be between 1 and 1000";
    let cases = [
        (0.99, 2, minutes),
        (10_080.01, 2, minutes),
        (f64::NAN, 2, minutes),
        (60.0, 0, units),
        (60.0, 1001, units),
    ];
    for (unit_duration_minutes, units, message) in cases {
        let params = SwitchbackCreateParams {
            unit_duration_minutes,
            units,
            ..params()
        };
        let err = platform
            .create_switchback("routing", params)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
        assert_eq!(err.to_string(), message);
    }
    assert!(server.requests().is_empty());

    for (unit_duration_minutes, units) in [(1.0, 1), (10_080.0, 1000)] {
        let params = SwitchbackCreateParams {
            unit_duration_minutes,
            units,
            ..params()
        };
        platform.create_switchback("routing", params).await.unwrap();
    }
    let requests = server.requests();
    assert_eq!(
        support::body(&requests[0])["generate_random_plan"],
        json!({ "unit_duration_minutes": 1.0, "units": 1 })
    );
    assert_eq!(
        support::body(&requests[1])["generate_random_plan"],
        json!({ "unit_duration_minutes": 10_080.0, "units": 1000 })
    );
}

#[tokio::test]
async fn checks_that_start_is_an_rfc3339_timestamp() {
    let server = MockServer::start(vec![MockResponse::json(200, created_json())]).await;
    let platform = support::platform(&server);
    for start in [
        "tomorrow",
        "2026-10-14",
        "2026-10-14T00:00:00",
        "2026-10-14 00:00:00Z",
        "2026-02-29T00:00:00Z",
    ] {
        let params = SwitchbackCreateParams {
            start: Some(start.to_owned()),
            ..params()
        };
        let err = platform
            .create_switchback("routing", params)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{start}: {err}");
        assert_eq!(
            err.to_string(),
            "invalid argument: `start` must be an RFC 3339 timestamp, such as \
             `2026-10-01T12:00:00Z`"
        );
    }
    assert!(server.requests().is_empty());

    // A valid timestamp goes to the platform as written.
    let params = SwitchbackCreateParams {
        start: Some("2026-10-14T02:00:00.5+02:00".to_owned()),
        ..params()
    };
    platform.create_switchback("routing", params).await.unwrap();
    let body = support::body(&server.requests()[0]);
    assert_eq!(
        body["generate_random_plan"]["start"],
        "2026-10-14T02:00:00.5+02:00"
    );
}

#[tokio::test]
async fn gets_a_switchback_test_with_all_its_runs() {
    let server = MockServer::start(vec![
        MockResponse::json(200, test_json()),
        MockResponse::json(
            200,
            json!({ "runs": [run_json("run-1")], "next_page_token": "page-2" }),
        ),
        MockResponse::json(200, json!({ "runs": [run_json("run-2")] })),
    ])
    .await;
    let test = support::platform(&server)
        .get_switchback("routing", "switchback-1")
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].target, format!("{TESTS}/switchback-1"));
    for request in &requests[1..] {
        assert_eq!(request.method, "GET");
        assert!(
            request
                .target
                .starts_with(&format!("{TESTS}/switchback-1/runs?")),
            "{}",
            request.target
        );
    }
    assert_eq!(
        requests[1].query(),
        support::pairs(&[("pagereturn", "true")])
    );
    assert_eq!(
        requests[2].query(),
        support::pairs(&[("pagereturn", "true"), ("pagetoken", "page-2")])
    );

    assert_eq!(test.status, Some(ExperimentStatus::Started));
    assert_eq!(test.started_at.as_deref(), Some("2026-10-14T00:00:00Z"));
    assert_eq!(test.completed_at, None);
    let runs = test.runs.as_ref().unwrap();
    let ids: Vec<&str> = runs.iter().map(|run| run.id.as_str()).collect();
    assert_eq!(ids, ["run-1", "run-2"]);
    assert_eq!(runs[0].environment_id, "candidate");
    assert_eq!(runs[0].status, RunStatus::Succeeded);
    assert_eq!(runs[0].experiment_id.as_deref(), Some("switchback-1"));

    // Emitted JSON uses the API names, and leaves out what the SDK does not
    // model.
    let wire = serde_json::to_value(&test).unwrap();
    let mut expected = test_json();
    expected
        .as_object_mut()
        .unwrap()
        .remove("grouped_distributional_summaries");
    let mut emitted = wire.clone();
    let emitted_runs = emitted.as_object_mut().unwrap().remove("runs").unwrap();
    assert_eq!(emitted, expected);
    assert_eq!(emitted_runs[1]["id"], "run-2");
    assert_eq!(emitted_runs[1]["application_instance_id"], "candidate");
    assert_eq!(emitted_runs[1]["status_v2"], "succeeded");
}

#[tokio::test]
async fn gets_a_switchback_test_without_runs() {
    let server = MockServer::start(vec![
        MockResponse::json(200, created_json()),
        MockResponse::json(200, json!({})),
    ])
    .await;
    let test = support::platform(&server)
        .get_switchback("routing", "switchback-1")
        .await
        .unwrap();
    assert_eq!(test.runs, Some(Vec::new()));
}

#[tokio::test]
async fn stops_when_the_switchback_test_is_missing() {
    let server = MockServer::start(vec![
        MockResponse::new(404).body("switchback experiment not found"),
    ])
    .await;
    let err = support::platform(&server)
        .get_switchback("routing", "switchback-9")
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Status { status: 404, .. }), "{err}");
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn lists_switchback_tests_across_pages() {
    let server = MockServer::start(vec![
        MockResponse::json(
            200,
            json!({ "items": [list_item_json("switchback-1")], "next_page_token": "page-2" }),
        ),
        MockResponse::json(200, json!({ "items": [list_item_json("switchback-2")] })),
    ])
    .await;
    let tests = support::platform(&server)
        .list_switchbacks("routing")
        .await
        .unwrap();
    let ids: Vec<&str> = tests
        .iter()
        .map(|test| test.switchback_id.as_deref().unwrap())
        .collect();
    assert_eq!(ids, ["switchback-1", "switchback-2"]);
    assert_eq!(tests[0].plan, None);
    assert_eq!(tests[0].runs, None);

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.method, "GET");
        assert!(request.target.starts_with(&format!("{TESTS}?")));
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
async fn gets_the_metadata_of_a_switchback_test() {
    let server = MockServer::start(vec![MockResponse::json(200, metadata_json())]).await;
    let metadata = support::platform(&server)
        .get_switchback_metadata("routing", "switchback-1")
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "GET");
    assert_eq!(request.target, format!("{TESTS}/switchback-1/metadata"));
    assert_eq!(metadata.switchback_id.as_deref(), Some("switchback-1"));
    assert_eq!(metadata.workspace_id.as_deref(), Some("routing"));
    assert_eq!(metadata.status, Some(ExperimentStatus::Draft));
    assert_eq!(serde_json::to_value(&metadata).unwrap(), metadata_json());
}

#[tokio::test]
async fn reads_statuses_it_does_not_know_as_unknown() {
    let mut metadata = metadata_json();
    metadata["status"] = json!("paused");
    let server = MockServer::start(vec![MockResponse::json(200, metadata)]).await;
    let metadata = support::platform(&server)
        .get_switchback_metadata("routing", "switchback-1")
        .await
        .unwrap();
    assert_eq!(metadata.status, Some(ExperimentStatus::Unknown));
}

#[tokio::test]
async fn starts_a_switchback_test() {
    // The platform answers `304 Not Modified` when the test already started.
    let server = MockServer::start(vec![MockResponse::new(204), MockResponse::new(304)]).await;
    let platform = support::platform(&server);
    platform
        .start_switchback("routing", "switchback-1")
        .await
        .unwrap();
    platform
        .start_switchback("routing", "switchback-1")
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.method, "PUT");
        assert_eq!(request.target, format!("{TESTS}/switchback-1/start"));
        assert!(request.body.is_empty());
    }
}

/// [`metadata_json`] with `status`.
fn with_status(status: &str) -> Value {
    let mut metadata = metadata_json();
    metadata["status"] = json!(status);
    metadata
}

#[tokio::test]
async fn polls_a_stop_and_a_delete_of_a_switchback_test() {
    let test = format!("{TESTS}/switchback-1");
    let server = MockServer::start(vec![
        MockResponse::new(204),
        MockResponse::json(200, with_status("stopping")),
        MockResponse::json(200, with_status("completed")),
        MockResponse::new(204),
        MockResponse::json(200, with_status("deleting")),
        MockResponse::new(404).body("switchback test not found"),
    ])
    .await;
    let platform = support::platform(&server);
    let params = SwitchbackStopParams {
        intent: StopIntent::Complete,
        polling: Some(support::fast_polling()),
    };
    platform
        .stop_switchback("routing", "switchback-1", params)
        .await
        .unwrap();
    let params = SwitchbackDeleteParams {
        polling: Some(support::fast_polling()),
    };
    platform
        .delete_switchback("routing", "switchback-1", params)
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
            format!("PUT {test}/stop"),
            format!("GET {test}/metadata"),
            format!("GET {test}/metadata"),
            format!("DELETE {test}"),
            format!("GET {test}/metadata"),
            format!("GET {test}/metadata"),
        ]
    );
}

#[tokio::test]
async fn ends_the_polling_of_a_failed_switchback_delete_with_an_error() {
    let server = MockServer::start(vec![
        MockResponse::new(204),
        MockResponse::json(200, with_status("delete-failed")),
    ])
    .await;
    let params = SwitchbackDeleteParams {
        polling: Some(support::fast_polling()),
    };
    let err = support::platform(&server)
        .delete_switchback("routing", "switchback-1", params)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "stopped waiting: the platform could not delete the switchback test"
    );
}

#[tokio::test]
async fn stops_a_switchback_test() {
    let server = MockServer::start(vec![MockResponse::new(204)]).await;
    let params = SwitchbackStopParams {
        intent: StopIntent::Cancel,
        polling: None,
    };
    support::platform(&server)
        .stop_switchback("routing", "switchback-1", params)
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "PUT");
    assert_eq!(request.target, format!("{TESTS}/switchback-1/stop"));
    assert_eq!(support::body(request), json!({ "intent": "cancel" }));
}

#[tokio::test]
async fn updates_only_the_fields_that_are_set() {
    let server = MockServer::start(vec![MockResponse::json(200, updated_json())]).await;
    let params = SwitchbackUpdateParams {
        description: Some("Version 2 against production.".to_owned()),
        ..Default::default()
    };
    let test = support::platform(&server)
        .update_switchback("routing", "switchback-1", params)
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "PATCH");
    assert_eq!(requests[0].target, format!("{TESTS}/switchback-1"));
    assert_eq!(
        support::body(&requests[0]),
        json!({ "description": "Version 2 against production." })
    );
    assert_eq!(test.name.as_deref(), Some("Rainy days"));
    assert_eq!(
        test.description.as_deref(),
        Some("Version 2 against production.")
    );
    assert_eq!(test.updated_at.as_deref(), Some("2026-10-13T15:00:00Z"));
    assert_eq!(test.status, None);
}

#[tokio::test]
async fn rejects_an_empty_update() {
    let server = MockServer::start(Vec::new()).await;
    let err = support::platform(&server)
        .update_switchback("routing", "switchback-1", SwitchbackUpdateParams::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "invalid argument: nothing to update: set `name` or `description`"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn deletes_a_switchback_test() {
    let server = MockServer::start(vec![MockResponse::new(204)]).await;
    support::platform(&server)
        .delete_switchback("routing", "switchback-1", Default::default())
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "DELETE");
    assert_eq!(request.target, format!("{TESTS}/switchback-1"));
}

#[tokio::test]
async fn encodes_switchback_ids_in_paths() {
    let server = MockServer::start(vec![MockResponse::json(200, metadata_json())]).await;
    let platform = support::platform(&server);
    platform
        .get_switchback_metadata("my workspace", "switchback?1")
        .await
        .unwrap();
    assert_eq!(
        server.requests()[0].target,
        "/v1/applications/my%20workspace/experiments/switchback/switchback%3F1/metadata"
    );
    for (workspace_id, switchback_id) in [
        ("routing", ".."),
        ("..", "switchback-1"),
        ("routing", ""),
        ("routing", "switchback/1"),
    ] {
        let err = platform
            .delete_switchback(workspace_id, switchback_id, Default::default())
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Invalid { .. }),
            "{switchback_id:?}: {err}"
        );
    }
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn serves_the_create_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::json(200, created_json())],
        "decision_platform_switchback_create",
        json!({
            "workspace_id": "routing",
            "switchback_id": "switchback-1",
            "comparison": {
                "baseline_instance_id": "production",
                "candidate_instance_id": "candidate",
            },
            "unit_duration_minutes": 60,
            "units": 2,
            "start": "2026-10-14T00:00:00Z",
        }),
    )
    .await;
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].target, TESTS);
    assert_eq!(
        support::body(&requests[0]),
        json!({
            "id": "switchback-1",
            "name": "switchback-1",
            "comparison": {
                "baseline_instance_id": "production",
                "candidate_instance_id": "candidate",
            },
            "generate_random_plan": {
                "unit_duration_minutes": 60.0,
                "units": 2,
                "start": "2026-10-14T00:00:00Z",
            },
        })
    );
    let result = result.unwrap();
    assert_eq!(result["id"], "switchback-1");
    assert_eq!(result["plan"]["units"][0]["instance_id"], "candidate");
}

#[tokio::test]
async fn serves_the_get_tool() {
    let (result, requests) = support::serve_tool(
        vec![
            MockResponse::json(200, test_json()),
            MockResponse::json(200, json!({ "runs": [run_json("run-1")] })),
        ],
        "decision_platform_switchback_get",
        json!({ "workspace_id": "routing", "switchback_id": "switchback-1" }),
    )
    .await;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].target, format!("{TESTS}/switchback-1"));
    assert_eq!(
        requests[1].target,
        format!("{TESTS}/switchback-1/runs?pagereturn=true")
    );
    let result = result.unwrap();
    assert_eq!(result["status"], "started");
    assert_eq!(result["runs"][0]["id"], "run-1");
}

#[tokio::test]
async fn serves_the_list_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::json(
            200,
            json!({ "items": [list_item_json("switchback-1")] }),
        )],
        "decision_platform_switchback_list",
        json!({ "workspace_id": "routing" }),
    )
    .await;
    assert_eq!(requests[0].target, format!("{TESTS}?pagereturn=true"));
    assert_eq!(result.unwrap()[0]["id"], "switchback-1");
}

#[tokio::test]
async fn serves_the_metadata_get_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::json(200, metadata_json())],
        "decision_platform_switchback_metadata_get",
        json!({ "workspace_id": "routing", "switchback_id": "switchback-1" }),
    )
    .await;
    assert_eq!(requests[0].target, format!("{TESTS}/switchback-1/metadata"));
    assert_eq!(result.unwrap(), metadata_json());
}

#[tokio::test]
async fn serves_the_start_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::new(204)],
        "decision_platform_switchback_start",
        json!({ "workspace_id": "routing", "switchback_id": "switchback-1" }),
    )
    .await;
    assert_eq!(requests[0].method, "PUT");
    assert_eq!(requests[0].target, format!("{TESTS}/switchback-1/start"));
    assert_eq!(result.unwrap(), Value::Null);
}

#[tokio::test]
async fn serves_the_stop_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::new(204)],
        "decision_platform_switchback_stop",
        json!({ "workspace_id": "routing", "switchback_id": "switchback-1", "intent": "complete" }),
    )
    .await;
    assert_eq!(requests[0].method, "PUT");
    assert_eq!(requests[0].target, format!("{TESTS}/switchback-1/stop"));
    assert_eq!(support::body(&requests[0]), json!({ "intent": "complete" }));
    assert_eq!(result.unwrap(), Value::Null);
}

#[tokio::test]
async fn serves_the_update_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::json(200, updated_json())],
        "decision_platform_switchback_update",
        json!({ "workspace_id": "routing", "switchback_id": "switchback-1", "name": "Rainy days" }),
    )
    .await;
    assert_eq!(requests[0].method, "PATCH");
    assert_eq!(requests[0].target, format!("{TESTS}/switchback-1"));
    assert_eq!(support::body(&requests[0]), json!({ "name": "Rainy days" }));
    assert_eq!(result.unwrap()["name"], "Rainy days");
}

#[tokio::test]
async fn serves_the_delete_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::new(204)],
        "decision_platform_switchback_delete",
        json!({ "workspace_id": "routing", "switchback_id": "switchback-1" }),
    )
    .await;
    assert_eq!(requests[0].method, "DELETE");
    assert_eq!(requests[0].target, format!("{TESTS}/switchback-1"));
    assert_eq!(result.unwrap(), Value::Null);
}

/// The names in the `required` list of `schema`.
fn required(schema: &Value) -> Vec<&str> {
    let mut names: Vec<&str> = schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    names.sort_unstable();
    names
}

#[test]
fn requires_the_comparison_the_plan_and_the_intent() {
    let create = Value::Object(
        support::tool("decision_platform_switchback_create")
            .input_schema()
            .clone(),
    );
    assert_eq!(
        required(&create),
        [
            "comparison",
            "unit_duration_minutes",
            "units",
            "workspace_id"
        ]
    );
    let comparison = &create["properties"]["comparison"];
    assert_eq!(
        required(comparison),
        ["baseline_instance_id", "candidate_instance_id"]
    );
    let units = &create["properties"]["units"];
    assert_eq!(
        (&units["minimum"], &units["maximum"]),
        (&json!(1), &json!(1000))
    );
    let minutes = &create["properties"]["unit_duration_minutes"];
    assert_eq!(
        (&minutes["minimum"], &minutes["maximum"]),
        (&json!(1), &json!(10080))
    );

    let stop = Value::Object(
        support::tool("decision_platform_switchback_stop")
            .input_schema()
            .clone(),
    );
    assert_eq!(required(&stop), ["intent", "switchback_id", "workspace_id"]);
}

#[tokio::test]
async fn rejects_tool_arguments_before_any_request() {
    let cases = [
        (
            "decision_platform_switchback_stop",
            json!({ "workspace_id": "routing", "switchback_id": "switchback-1" }),
        ),
        (
            "decision_platform_switchback_stop",
            json!({ "workspace_id": "routing", "switchback_id": "switchback-1", "intent": "pause" }),
        ),
        (
            "decision_platform_switchback_create",
            json!({ "workspace_id": "routing", "unit_duration_minutes": 60, "units": 2 }),
        ),
    ];
    for (name, args) in cases {
        let (result, requests) = support::serve_tool(Vec::new(), name, args.clone()).await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidArguments(_)),
            "{args}: {err}"
        );
        assert!(requests.is_empty());
    }

    let (result, requests) = support::serve_tool(
        Vec::new(),
        "decision_platform_switchback_create",
        json!({
            "workspace_id": "routing",
            "comparison": {
                "baseline_instance_id": "production",
                "candidate_instance_id": "candidate",
            },
            "unit_duration_minutes": 60,
            "units": 1001,
        }),
    )
    .await;
    let err = result.unwrap_err();
    assert!(
        matches!(err, ToolError::Failed(Error::Invalid { .. })),
        "{err}"
    );
    assert!(requests.is_empty());
}
