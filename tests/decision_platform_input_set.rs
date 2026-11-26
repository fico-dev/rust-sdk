//! Tests for the platform input set operations and their tools, against the
//! mock server.

mod mock_server;
mod support;

use fico_sdk::Error;
use fico_sdk::decision::ManagedInput;
use fico_sdk::decision::platform::{
    InputSetCreateParams, InputSetDeleteParams, InputSetUpdateParams,
};
use mock_server::{MockResponse, MockServer};
use serde_json::{Value, json};

/// An input set of runs, as the platform returns it.
fn input_set_json() -> Value {
    json!({
        "id": "last-week",
        "name": "Last week",
        "description": "Runs of the production environment.",
        "app_id": "routing",
        "created_at": "2026-10-01T12:00:00.123456789Z",
        "input_ids": ["run-1", "run-2"],
        "updated_at": "2026-10-01T12:00:00.123456789Z",
        "inputs": [],
    })
}

/// An input set of managed inputs, as the platform returns it.
fn managed_input_set_json() -> Value {
    json!({
        "id": "weekdays",
        "name": "Weekdays",
        "description": "",
        "app_id": "routing",
        "created_at": "2026-10-01T12:00:00.123456789Z",
        "input_ids": [],
        "updated_at": "2026-10-01T12:00:00.123456789Z",
        "inputs": [
            { "id": "monday", "name": "Monday", "description": "" },
            { "id": "tuesday", "name": "Tuesday", "description": "Busy day." },
        ],
    })
}

fn managed_input(managed_input_id: &str) -> ManagedInput {
    ManagedInput {
        managed_input_id: managed_input_id.to_owned(),
        ..Default::default()
    }
}

#[tokio::test]
async fn creates_an_input_set_from_an_environment() {
    let server = MockServer::start(vec![MockResponse::json(200, input_set_json())]).await;
    let params = InputSetCreateParams {
        input_set_id: Some("last-week".to_owned()),
        name: Some("Last week".to_owned()),
        description: Some("Runs of the production environment.".to_owned()),
        environment_id: Some("production".to_owned()),
        start_time: Some("2026-09-24T00:00:00Z".to_owned()),
        end_time: Some("2026-10-01T00:00:00.5-05:00".to_owned()),
        maximum_runs: Some(10),
        ..Default::default()
    };
    let input_set = support::platform(&server)
        .create_input_set("routing", params)
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(
        requests[0].target,
        "/v1/applications/routing/experiments/inputsets"
    );
    assert_eq!(
        support::body(&requests[0]),
        json!({
            "id": "last-week",
            "name": "Last week",
            "description": "Runs of the production environment.",
            "instance_id": "production",
            "start_time": "2026-09-24T00:00:00Z",
            "end_time": "2026-10-01T00:00:00.5-05:00",
            "maximum_runs": 10,
        })
    );
    assert_eq!(input_set.input_set_id, "last-week");
    assert_eq!(input_set.workspace_id, "routing");
    assert_eq!(input_set.input_ids, ["run-1", "run-2"]);
    assert!(input_set.inputs.is_empty());
}

#[tokio::test]
async fn creates_an_input_set_from_the_default_environment() {
    for (workspace, expected) in [
        (
            json!({ "id": "routing", "default_instance": "production" }),
            "production",
        ),
        (json!({ "id": "routing" }), "latest"),
    ] {
        let server = MockServer::start(vec![
            MockResponse::json(200, workspace),
            MockResponse::json(200, input_set_json()),
        ])
        .await;
        let params = InputSetCreateParams {
            input_set_id: Some("last-week".to_owned()),
            environment_id: Some("default".to_owned()),
            ..Default::default()
        };
        support::platform(&server)
            .create_input_set("routing", params)
            .await
            .unwrap();
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            (requests[0].method.as_str(), requests[0].target.as_str()),
            ("GET", "/v1/applications/routing")
        );
        assert_eq!(support::body(&requests[1])["instance_id"], expected);
    }
}

#[tokio::test]
async fn creates_an_input_set_from_runs() {
    let server = MockServer::start(vec![MockResponse::json(200, input_set_json())]).await;
    let params = InputSetCreateParams {
        input_set_id: Some("last-week".to_owned()),
        run_ids: Some(vec!["run-1".to_owned(), "run-2".to_owned()]),
        ..Default::default()
    };
    support::platform(&server)
        .create_input_set("routing", params)
        .await
        .unwrap();
    assert_eq!(
        support::body(&server.requests()[0]),
        json!({ "id": "last-week", "name": "last-week", "run_ids": ["run-1", "run-2"] })
    );
}

#[tokio::test]
async fn creates_an_input_set_from_managed_inputs() {
    let server = MockServer::start(vec![MockResponse::json(200, managed_input_set_json())]).await;
    let params = InputSetCreateParams {
        input_set_id: Some("weekdays".to_owned()),
        name: Some("Weekdays".to_owned()),
        maximum_runs: Some(20),
        inputs: Some(vec![
            managed_input("monday"),
            ManagedInput {
                description: Some("Busy day.".to_owned()),
                ..managed_input("tuesday")
            },
        ]),
        ..Default::default()
    };
    let input_set = support::platform(&server)
        .create_input_set("routing", params)
        .await
        .unwrap();
    assert_eq!(
        support::body(&server.requests()[0]),
        json!({
            "id": "weekdays",
            "name": "Weekdays",
            "maximum_runs": 20,
            "inputs": [{ "id": "monday" }, { "id": "tuesday", "description": "Busy day." }],
        })
    );
    assert!(input_set.input_ids.is_empty());
    let names: Vec<Option<&str>> = input_set
        .inputs
        .iter()
        .map(|input| input.name.as_deref())
        .collect();
    assert_eq!(names, [Some("Monday"), Some("Tuesday")]);
}

#[tokio::test]
async fn names_a_new_input_set_after_its_id() {
    let server = MockServer::start(vec![MockResponse::json(200, input_set_json())]).await;
    let params = InputSetCreateParams {
        input_set_id: Some("last-week".to_owned()),
        name: Some(String::new()),
        environment_id: Some("prod".to_owned()),
        ..Default::default()
    };
    support::platform(&server)
        .create_input_set("routing", params)
        .await
        .unwrap();
    assert_eq!(
        support::body(&server.requests()[0]),
        json!({ "id": "last-week", "name": "last-week", "instance_id": "prod" })
    );
}

#[tokio::test]
async fn generates_an_id_when_none_is_set() {
    let server = MockServer::start(vec![MockResponse::json(200, input_set_json())]).await;
    let params = InputSetCreateParams {
        input_set_id: Some(String::new()),
        run_ids: Some(vec!["run-1".to_owned()]),
        ..Default::default()
    };
    support::platform(&server)
        .create_input_set("routing", params)
        .await
        .unwrap();
    let body = support::body(&server.requests()[0]);
    let input_set_id = body["id"].as_str().unwrap();
    let suffix = input_set_id.strip_prefix("input-set-").unwrap();
    assert_eq!(suffix.len(), 8, "{input_set_id}");
    assert!(
        suffix
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
        "{input_set_id}"
    );
    assert_eq!(body["name"], input_set_id);
}

#[tokio::test]
async fn needs_one_source_and_inputs_within_the_limits() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    let run_ids = |count: usize| Some((0..count).map(|i| format!("run-{i}")).collect());
    let inputs = |count: usize| {
        Some(
            (0..count)
                .map(|i| managed_input(&format!("input-{i}")))
                .collect(),
        )
    };
    let none = "set one source: `environment_id` (with a time range), `run_ids` or `inputs`";
    let two = "set only one source: `environment_id` (with a time range), `run_ids` or `inputs`";
    let cases = [
        (InputSetCreateParams::default(), none.to_owned()),
        (
            InputSetCreateParams {
                run_ids: Some(Vec::new()),
                inputs: Some(Vec::new()),
                environment_id: Some(String::new()),
                ..Default::default()
            },
            none.to_owned(),
        ),
        (
            InputSetCreateParams {
                run_ids: run_ids(1),
                inputs: inputs(1),
                ..Default::default()
            },
            two.to_owned(),
        ),
        (
            InputSetCreateParams {
                environment_id: Some("prod".to_owned()),
                run_ids: run_ids(1),
                ..Default::default()
            },
            two.to_owned(),
        ),
        (
            InputSetCreateParams {
                end_time: Some("2026-10-20T00:00:00Z".to_owned()),
                inputs: inputs(1),
                ..Default::default()
            },
            two.to_owned(),
        ),
        (
            InputSetCreateParams {
                run_ids: run_ids(21),
                ..Default::default()
            },
            "`run_ids` has 21 items: an input set takes at most 20".to_owned(),
        ),
        (
            InputSetCreateParams {
                inputs: inputs(1_001),
                ..Default::default()
            },
            "`inputs` has 1001 items: an input set holds at most 1000".to_owned(),
        ),
        (
            InputSetCreateParams {
                environment_id: Some("prod".to_owned()),
                maximum_runs: Some(1_001),
                ..Default::default()
            },
            "`maximum_runs` is 1001: an input set holds at most 1000".to_owned(),
        ),
    ];
    for (params, message) in cases {
        let err = platform
            .create_input_set("routing", params)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
        assert_eq!(err.to_string(), format!("invalid argument: {message}"));
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn rejects_timestamps_that_are_not_rfc3339() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    let cases = [
        (
            InputSetCreateParams {
                start_time: Some("2026-09-24".to_owned()),
                ..Default::default()
            },
            "start_time",
        ),
        (
            InputSetCreateParams {
                end_time: Some("2026-10-01T00:00:00".to_owned()),
                ..Default::default()
            },
            "end_time",
        ),
    ];
    for (params, field) in cases {
        let err = platform
            .create_input_set("routing", params)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
        assert_eq!(
            err.to_string(),
            format!(
                "invalid argument: `{field}` must be an RFC 3339 timestamp, \
                 such as `2026-10-01T12:00:00Z`"
            )
        );
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn leaves_out_empty_times() {
    let server = MockServer::start(vec![MockResponse::json(200, input_set_json())]).await;
    let params = InputSetCreateParams {
        input_set_id: Some("last-week".to_owned()),
        environment_id: Some("production".to_owned()),
        start_time: Some(String::new()),
        end_time: Some(String::new()),
        ..Default::default()
    };
    support::platform(&server)
        .create_input_set("routing", params)
        .await
        .unwrap();
    assert_eq!(
        support::body(&server.requests()[0]),
        json!({ "id": "last-week", "name": "last-week", "instance_id": "production" })
    );
}

#[tokio::test]
async fn gets_an_input_set() {
    let server = MockServer::start(vec![MockResponse::json(200, managed_input_set_json())]).await;
    let input_set = support::platform(&server)
        .get_input_set("routing", "weekdays")
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "GET");
    assert_eq!(
        request.target,
        "/v1/applications/routing/experiments/inputsets/weekdays"
    );
    assert_eq!(input_set.name, "Weekdays");
    assert_eq!(input_set.created_at, "2026-10-01T12:00:00.123456789Z");
    assert_eq!(
        input_set.inputs[1].description.as_deref(),
        Some("Busy day.")
    );
    // Emitted JSON uses the API names.
    assert_eq!(
        serde_json::to_value(&input_set).unwrap(),
        managed_input_set_json()
    );
}

#[tokio::test]
async fn lists_input_sets_across_pages() {
    let server = MockServer::start(vec![
        MockResponse::json(
            200,
            json!({ "items": [input_set_json()], "next_page_token": "page-2" }),
        ),
        MockResponse::json(200, json!({ "items": [managed_input_set_json()] })),
    ])
    .await;
    let input_sets = support::platform(&server)
        .list_input_sets("routing")
        .await
        .unwrap();
    let ids: Vec<&str> = input_sets.iter().map(|s| s.input_set_id.as_str()).collect();
    assert_eq!(ids, ["last-week", "weekdays"]);

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.method, "GET");
        assert!(
            request
                .target
                .starts_with("/v1/applications/routing/experiments/inputsets?")
        );
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
async fn updates_only_the_fields_that_are_set() {
    let mut updated = input_set_json();
    updated["description"] = json!("Runs of the busiest week.");
    let server = MockServer::start(vec![
        MockResponse::json(200, input_set_json()),
        MockResponse::json(200, updated.clone()),
    ])
    .await;
    let params = InputSetUpdateParams {
        description: Some("Runs of the busiest week.".to_owned()),
        ..Default::default()
    };
    let input_set = support::platform(&server)
        .update_input_set("routing", "last-week", params)
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(
        requests[0].target,
        "/v1/applications/routing/experiments/inputsets/last-week"
    );
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(
        requests[1].target,
        "/v1/applications/routing/experiments/inputsets/last-week"
    );
    // Everything else goes back as it came.
    assert_eq!(support::body(&requests[1]), updated);
    assert_eq!(input_set.description, "Runs of the busiest week.");
    assert_eq!(input_set.name, "Last week");
}

#[tokio::test]
async fn replaces_the_managed_inputs() {
    let mut updated = managed_input_set_json();
    updated["inputs"] = json!([{ "id": "friday", "name": "Friday", "description": "" }]);
    let server = MockServer::start(vec![
        MockResponse::json(200, managed_input_set_json()),
        MockResponse::json(200, updated),
    ])
    .await;
    let params = InputSetUpdateParams {
        inputs: Some(vec![managed_input("friday")]),
        ..Default::default()
    };
    let input_set = support::platform(&server)
        .update_input_set("routing", "weekdays", params)
        .await
        .unwrap();
    let mut expected = managed_input_set_json();
    expected["inputs"] = json!([{ "id": "friday" }]);
    assert_eq!(support::body(&server.requests()[1]), expected);
    assert_eq!(input_set.inputs[0].managed_input_id, "friday");
}

#[tokio::test]
async fn clears_the_managed_inputs_with_an_empty_list() {
    let mut updated = managed_input_set_json();
    updated["inputs"] = json!([]);
    let server = MockServer::start(vec![
        MockResponse::json(200, managed_input_set_json()),
        MockResponse::json(200, updated.clone()),
    ])
    .await;
    let params = InputSetUpdateParams {
        inputs: Some(Vec::new()),
        ..Default::default()
    };
    let input_set = support::platform(&server)
        .update_input_set("routing", "weekdays", params)
        .await
        .unwrap();
    assert_eq!(support::body(&server.requests()[1]), updated);
    assert!(input_set.inputs.is_empty());
}

#[tokio::test]
async fn rejects_an_empty_update() {
    let server = MockServer::start(Vec::new()).await;
    let err = support::platform(&server)
        .update_input_set("routing", "last-week", InputSetUpdateParams::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "invalid argument: nothing to update: set `name`, `description` or `inputs`"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn deletes_an_input_set() {
    let server = MockServer::start(vec![MockResponse::new(204)]).await;
    support::platform(&server)
        .delete_input_set("routing", "last-week", Default::default())
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "DELETE");
    assert_eq!(
        request.target,
        "/v1/applications/routing/experiments/inputsets/last-week"
    );
}

#[tokio::test]
async fn polls_a_delete_until_the_input_set_is_gone() {
    let set = "/v1/applications/routing/experiments/inputsets/last-week";
    let server = MockServer::start(vec![
        MockResponse::new(204),
        MockResponse::json(200, input_set_json()),
        MockResponse::new(404).body("input set not found"),
    ])
    .await;
    let params = InputSetDeleteParams {
        polling: Some(support::fast_polling()),
    };
    support::platform(&server)
        .delete_input_set("routing", "last-week", params)
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
            format!("DELETE {set}"),
            format!("GET {set}"),
            format!("GET {set}"),
        ]
    );
}

#[tokio::test]
async fn encodes_input_set_ids_in_paths() {
    let server = MockServer::start(vec![MockResponse::json(200, input_set_json())]).await;
    let platform = support::platform(&server);
    platform
        .get_input_set("my workspace", "last?week")
        .await
        .unwrap();
    assert_eq!(
        server.requests()[0].target,
        "/v1/applications/my%20workspace/experiments/inputsets/last%3Fweek"
    );
    for (workspace_id, input_set_id) in [
        ("routing", ".."),
        ("..", "last-week"),
        ("routing", ""),
        ("routing", "last/week"),
    ] {
        let err = platform
            .delete_input_set(workspace_id, input_set_id, Default::default())
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Invalid { .. }),
            "{input_set_id:?}: {err}"
        );
    }
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn serves_the_create_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(200, managed_input_set_json())],
        "decision_platform_input_set_create",
        json!({
            "workspace_id": "routing",
            "input_set_id": "weekdays",
            "inputs": [{ "id": "monday" }, { "id": "tuesday", "name": "Tuesday" }],
        }),
    )
    .await;
    assert_eq!(requests[0].method, "POST");
    assert_eq!(
        requests[0].target,
        "/v1/applications/routing/experiments/inputsets"
    );
    assert_eq!(
        support::body(&requests[0]),
        json!({
            "id": "weekdays",
            "name": "weekdays",
            "inputs": [{ "id": "monday" }, { "id": "tuesday", "name": "Tuesday" }],
        })
    );
    assert_eq!(result["app_id"], "routing");
}

#[tokio::test]
async fn serves_the_get_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(200, input_set_json())],
        "decision_platform_input_set_get",
        json!({ "workspace_id": "routing", "input_set_id": "last-week" }),
    )
    .await;
    assert_eq!(
        requests[0].target,
        "/v1/applications/routing/experiments/inputsets/last-week"
    );
    assert_eq!(result["input_ids"], json!(["run-1", "run-2"]));
}

#[tokio::test]
async fn serves_the_list_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(
            200,
            json!({ "items": [input_set_json()] }),
        )],
        "decision_platform_input_set_list",
        json!({ "workspace_id": "routing" }),
    )
    .await;
    assert_eq!(
        requests[0].target,
        "/v1/applications/routing/experiments/inputsets?pagereturn=true"
    );
    assert_eq!(result[0]["id"], "last-week");
}

#[tokio::test]
async fn serves_the_update_tool() {
    let mut updated = input_set_json();
    updated["name"] = json!("Busiest week");
    let (result, requests) = support::serve_tool_ok(
        vec![
            MockResponse::json(200, input_set_json()),
            MockResponse::json(200, updated.clone()),
        ],
        "decision_platform_input_set_update",
        json!({ "workspace_id": "routing", "input_set_id": "last-week", "name": "Busiest week" }),
    )
    .await;
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(
        requests[1].target,
        "/v1/applications/routing/experiments/inputsets/last-week"
    );
    assert_eq!(support::body(&requests[1]), updated);
    assert_eq!(result["name"], "Busiest week");
}

#[tokio::test]
async fn serves_the_delete_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::new(204)],
        "decision_platform_input_set_delete",
        json!({ "workspace_id": "routing", "input_set_id": "last-week" }),
    )
    .await;
    assert_eq!(requests[0].method, "DELETE");
    assert_eq!(
        requests[0].target,
        "/v1/applications/routing/experiments/inputsets/last-week"
    );
    assert_eq!(result, Value::Null);
}
