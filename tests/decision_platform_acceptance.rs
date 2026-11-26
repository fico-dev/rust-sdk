//! Tests for the platform acceptance test operations and their tools,
//! against the mock server.

mod mock_server;
mod support;

use fico_sdk::Error;
use fico_sdk::decision::platform::{
    AcceptanceCreateParams, AcceptanceDeleteParams, AcceptanceGetParams, AcceptanceUpdateParams,
};
use fico_sdk::decision::{
    AcceptanceResultsStatus, Comparison, ExperimentStatus, Metric, MetricParams, MetricTolerance,
    MetricToleranceType, MetricType, PollingOptions, StatisticType,
};
use fico_sdk::tool::ToolError;
use mock_server::{MockResponse, MockServer};
use serde_json::{Value, json};

/// Path of the experiments of the `routing` workspace.
const EXPERIMENTS: &str = "/v1/applications/routing/experiments";
/// Path of the acceptance tests of the `routing` workspace.
const TESTS: &str = "/v1/applications/routing/experiments/acceptance";

/// An input set of run inputs, as the platform returns it.
fn input_set_json() -> Value {
    json!({
        "id": "last-week",
        "name": "Last week",
        "description": "Runs of the production environment.",
        "app_id": "routing",
        "created_at": "2026-10-01T12:00:00Z",
        "input_ids": ["run-1", "run-2"],
        "updated_at": "2026-10-01T12:00:00Z",
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
        "created_at": "2026-10-01T12:00:00Z",
        "input_ids": [],
        "updated_at": "2026-10-01T12:00:00Z",
        "inputs": [
            { "id": "monday", "name": "Monday", "description": "" },
            { "id": "tuesday", "name": "Tuesday", "description": "" },
        ],
    })
}

/// A metric as the platform sends it.
fn metric_json() -> Value {
    json!({
        "field": "result.value",
        "metric_type": "direct-comparison",
        "params": {
            "operator": "le",
            "tolerance": { "type": "relative", "value": 5.0 },
        },
        "statistic": "mean",
    })
}

/// The metric of [`metric_json`].
fn metric() -> Metric {
    Metric {
        field: "result.value".to_owned(),
        metric_type: MetricType::DirectComparison,
        params: MetricParams {
            operator: Comparison::LessThanOrEqualTo,
            tolerance: MetricTolerance {
                tolerance_type: MetricToleranceType::Relative,
                value: 5.0,
            },
        },
        statistic: StatisticType::Mean,
    }
}

/// An acceptance test as the platform's create returns it, with no results
/// yet (`results_status` is empty).
fn created_json() -> Value {
    json!({
        "id": "acceptance-1",
        "name": "Version 2",
        "description": "Candidate against production.",
        "app_id": "routing",
        "experiment_id": "acceptance-1",
        "control": { "instance_id": "production", "version_id": "v1" },
        "candidate": { "instance_id": "candidate", "version_id": "v2" },
        "metrics": [metric_json()],
        "results_status": "",
        "created_at": "2026-10-20T14:00:00Z",
        "updated_at": "2026-10-20T14:00:00Z",
    })
}

/// An acceptance test as the platform's get returns it, with `status`.
fn test_json(status: &str) -> Value {
    let mut test = created_json();
    test.as_object_mut().unwrap().remove("results_status");
    test["status"] = json!(status);
    test
}

/// Statistics of one environment, as the platform sends them.
fn statistics_json(environment_id: &str, build_id: &str, mean: Value) -> Value {
    json!({
        "instance_id": environment_id,
        "version_id": build_id,
        "number_of_runs_total": 2,
        "distribution_summary_statistics": {
            "min": 10.0, "max": 12.0, "count": 2, "mean": mean, "std": 1.0,
            "shifted_geometric_mean": 10.9, "shift_parameter": 1.0,
        },
        "distribution_percentiles": {
            "p01": 10.0, "p05": 10.1, "p10": 10.2, "p25": 10.5, "p50": 11.0,
            "p75": 11.5, "p90": 11.8, "p95": 11.9, "p99": 12.0,
        },
    })
}

/// A completed acceptance test with its results, with a statistic that is
/// not a number and a pair with no candidate value.
fn completed_json() -> Value {
    let mut test = test_json("completed");
    test["results_status"] = json!("succeeded");
    test["results"] = json!({
        "passed": true,
        "metric_results": [{
            "metric": metric_json(),
            "statistics": {
                "control": statistics_json("production", "v1", json!(11.0)),
                "candidate": statistics_json("candidate", "v2", json!("nan")),
            },
            "metric_pairs": { "input-1": [10.0, 10.0], "input-2": [12.0, null] },
            "passed": true,
        }],
    });
    test
}

/// What the platform's update returns.
fn updated_json() -> Value {
    json!({
        "id": "acceptance-1",
        "name": "Version 2",
        "description": "Version 2 against production.",
        "created_at": "2026-10-20T14:00:00Z",
        "updated_at": "2026-10-20T15:00:00Z",
    })
}

/// Create parameters with only the required fields set.
fn params() -> AcceptanceCreateParams {
    AcceptanceCreateParams {
        baseline_environment_id: "production".to_owned(),
        candidate_environment_id: "candidate".to_owned(),
        input_set_id: "last-week".to_owned(),
        metrics: vec![metric()],
        ..AcceptanceCreateParams::default()
    }
}

/// The responses of a create: the input set, the batch and the test.
fn create_responses(batch_id: &str) -> Vec<MockResponse> {
    vec![
        MockResponse::json(200, input_set_json()),
        MockResponse::json(200, json!({ "id": batch_id })),
        MockResponse::json(200, created_json()),
    ]
}

#[tokio::test]
async fn creates_an_acceptance_test_on_its_own_batch() {
    let server = MockServer::start(create_responses("acceptance-1")).await;
    let params = AcceptanceCreateParams {
        acceptance_id: Some("acceptance-1".to_owned()),
        name: Some("Version 2".to_owned()),
        description: Some("Candidate against production.".to_owned()),
        ..params()
    };
    let test = support::platform(&server)
        .create_acceptance("routing", params)
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(
        requests[0].target,
        format!("{EXPERIMENTS}/inputsets/last-week")
    );
    assert_eq!(requests[1].method, "POST");
    assert_eq!(requests[1].target, format!("{EXPERIMENTS}/batch"));
    let run = |input_id: &str, environment_id: &str| json!({ "input_id": input_id, "instance_id": environment_id, "input_set_id": "last-week" });
    assert_eq!(
        support::body(&requests[1]),
        json!({
            "id": "acceptance-1",
            "name": "Version 2",
            "description": "Candidate against production.",
            "type": "batch",
            "runs": [
                run("run-1", "candidate"),
                run("run-2", "candidate"),
                run("run-1", "production"),
                run("run-2", "production"),
            ],
        })
    );
    assert_eq!(requests[2].method, "POST");
    assert_eq!(requests[2].target, TESTS);
    assert_eq!(
        support::body(&requests[2]),
        json!({
            "id": "acceptance-1",
            "name": "Version 2",
            "description": "Candidate against production.",
            "experiment_id": "acceptance-1",
            "control": { "instance_id": "production" },
            "candidate": { "instance_id": "candidate" },
            "metrics": [metric_json()],
        })
    );
    assert_eq!(test.acceptance_id, "acceptance-1");
    assert_eq!(test.workspace_id.as_deref(), Some("routing"));
    assert_eq!(test.experiment_id.as_deref(), Some("acceptance-1"));
    let control = test.control.unwrap();
    assert_eq!(control.environment_id, "production");
    assert_eq!(control.build_id.as_deref(), Some("v1"));
    assert_eq!(test.candidate.unwrap().environment_id, "candidate");
    assert_eq!(test.metrics, Some(vec![metric()]));
    assert_eq!(test.status, None);
    assert_eq!(test.results, None);
}

#[tokio::test]
async fn runs_the_managed_inputs_of_an_input_set_without_run_inputs() {
    let server = MockServer::start(vec![
        MockResponse::json(200, managed_input_set_json()),
        MockResponse::json(200, json!({ "id": "acceptance-1" })),
        MockResponse::json(200, created_json()),
    ])
    .await;
    let params = AcceptanceCreateParams {
        acceptance_id: Some("acceptance-1".to_owned()),
        input_set_id: "weekdays".to_owned(),
        ..params()
    };
    support::platform(&server)
        .create_acceptance("routing", params)
        .await
        .unwrap();

    let requests = server.requests();
    let run = |input_id: &str, environment_id: &str| json!({ "input_id": input_id, "instance_id": environment_id, "input_set_id": "weekdays" });
    assert_eq!(
        support::body(&requests[1])["runs"],
        json!([
            run("monday", "candidate"),
            run("tuesday", "candidate"),
            run("monday", "production"),
            run("tuesday", "production"),
        ])
    );
}

#[tokio::test]
async fn generates_the_id_and_name_when_unset_or_empty() {
    let empty = AcceptanceCreateParams {
        acceptance_id: Some(String::new()),
        name: Some(String::new()),
        ..params()
    };
    for params in [params(), empty] {
        let server = MockServer::start(vec![
            MockResponse::json(200, input_set_json()),
            // The mock can't echo the generated ID, so the create stops at
            // the ID check, after the batch request.
            MockResponse::json(200, json!({ "id": "placeholder" })),
        ])
        .await;
        let err = support::platform(&server)
            .create_acceptance("routing", params)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err:?}");

        let requests = server.requests();
        let batch = support::body(&requests[1]);
        let acceptance_id = batch["id"].as_str().unwrap();
        let suffix = acceptance_id.strip_prefix("acceptance-").unwrap();
        assert_eq!(suffix.len(), 8, "{acceptance_id}");
        assert_eq!(batch["name"], acceptance_id);
        assert_eq!(batch.get("description"), None);
    }
}

#[tokio::test]
async fn rejects_a_create_with_a_missing_field_before_any_request() {
    let no_input_set = AcceptanceCreateParams {
        input_set_id: String::new(),
        ..params()
    };
    let no_metrics = AcceptanceCreateParams {
        metrics: Vec::new(),
        ..params()
    };
    let no_baseline = AcceptanceCreateParams {
        baseline_environment_id: String::new(),
        ..params()
    };
    let no_candidate = AcceptanceCreateParams {
        candidate_environment_id: String::new(),
        ..params()
    };
    let cases = [
        (no_input_set, "`input_set_id` must be set"),
        (no_baseline, "`baseline_environment_id` must be set"),
        (no_candidate, "`candidate_environment_id` must be set"),
        (no_metrics, "`metrics` needs at least one metric"),
    ];
    for (params, message) in cases {
        let server = MockServer::start(Vec::new()).await;
        let err = support::platform(&server)
            .create_acceptance("routing", params)
            .await
            .unwrap_err();
        let Error::Invalid { reason, .. } = &err else {
            panic!("{err:?}");
        };
        assert_eq!(reason, message);
        assert!(server.requests().is_empty());
    }
}

#[tokio::test]
async fn rejects_unknown_metric_values_before_any_request() {
    let with = |change: fn(&mut Metric)| {
        let mut metric = metric();
        change(&mut metric);
        AcceptanceCreateParams {
            metrics: vec![metric],
            ..params()
        }
    };
    let cases = [
        (with(|m| m.field = String::new()), "field"),
        (with(|m| m.metric_type = MetricType::Unknown), "metric_type"),
        (with(|m| m.statistic = StatisticType::Unknown), "statistic"),
        (
            with(|m| m.params.operator = Comparison::Unknown),
            "params.operator",
        ),
        (
            with(|m| m.params.tolerance.tolerance_type = MetricToleranceType::Unknown),
            "params.tolerance.type",
        ),
    ];
    for (params, name) in cases {
        let server = MockServer::start(Vec::new()).await;
        let err = support::platform(&server)
            .create_acceptance("routing", params)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("invalid argument: `metrics[0].{name}` must be set to a known value")
        );
        assert!(server.requests().is_empty());
    }
}

#[tokio::test]
async fn rejects_a_batch_with_another_id() {
    let server = MockServer::start(create_responses("batch-7")).await;
    let params = AcceptanceCreateParams {
        acceptance_id: Some("acceptance-1".to_owned()),
        ..params()
    };
    let err = support::platform(&server)
        .create_acceptance("routing", params)
        .await
        .unwrap_err();
    let Error::Invalid { reason, .. } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(
        reason,
        "the platform created the acceptance test's batch experiment as `batch-7`, \
         not `acceptance-1`"
    );
    assert_eq!(server.requests().len(), 2);
}

/// A page of the runs of a batch experiment, with one run.
fn runs_json(run_id: &str) -> Value {
    json!({ "runs": [{
        "id": run_id,
        "user_email": "dev@example.com",
        "name": "",
        "description": "",
        "created_at": "2026-10-20T14:01:00Z",
        "application_id": "routing",
        "application_instance_id": "candidate",
        "application_version_id": "v2",
        "run_type": { "type": "", "definition_id": "", "reference_id": "" },
        "execution_class": "6c9500mb870s",
        "runtime": "ghcr.io/nextmv-io/runtime/default:latest",
        "status_v2": "succeeded",
        "experiment_id": "acceptance-1",
    }] })
}

#[tokio::test]
async fn gets_an_acceptance_test_with_its_results_and_runs() {
    let server = MockServer::start(vec![
        MockResponse::json(200, completed_json()),
        MockResponse::json(200, runs_json("candidate-run-1")),
    ])
    .await;
    let test = support::platform(&server)
        .get_acceptance("routing", "acceptance-1", AcceptanceGetParams::default())
        .await
        .unwrap();

    assert_eq!(
        calls(&server),
        [
            format!("GET {TESTS}/acceptance-1"),
            format!("GET {EXPERIMENTS}/batch/acceptance-1/runs?pagereturn=true"),
        ]
    );
    let runs = test.runs.as_deref().unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].id, "candidate-run-1");
    assert_eq!(test.status, Some(ExperimentStatus::Completed));
    let results = test.results.unwrap();
    assert!(results.passed);
    assert_eq!(results.error, None);
    let result = &results.metric_results.unwrap()[0];
    assert!(result.passed);
    assert_eq!(result.metric, metric());
    let control = &result.statistics.control;
    assert_eq!(control.environment_id, "production");
    assert_eq!(control.build_id, "v1");
    assert_eq!(control.number_of_runs_total, 2);
    assert_eq!(control.distribution_summary_statistics.mean, 11.0);
    assert_eq!(control.distribution_percentiles.p50, 11.0);
    let candidate = &result.statistics.candidate;
    assert_eq!(candidate.environment_id, "candidate");
    assert!(candidate.distribution_summary_statistics.mean.is_nan());
    let pairs = result.metric_pairs.as_ref().unwrap();
    assert_eq!(pairs["input-1"], [Some(10.0), Some(10.0)]);
    assert_eq!(pairs["input-2"], [Some(12.0), None]);
    assert_eq!(
        test.results_status,
        Some(AcceptanceResultsStatus::Succeeded)
    );
}

#[tokio::test]
async fn reads_an_empty_or_new_results_status() {
    let mut new = created_json();
    new["results_status"] = json!("archived");
    let server = MockServer::start(vec![MockResponse::json(
        200,
        json!({ "items": [created_json(), new] }),
    )])
    .await;
    let tests = support::platform(&server)
        .list_acceptances("routing")
        .await
        .unwrap();
    // The platform sends `""` for results it hasn't computed.
    assert_eq!(tests[0].results_status, None);
    assert_eq!(
        tests[1].results_status,
        Some(AcceptanceResultsStatus::Unknown)
    );
    let written = serde_json::to_value(&tests[0]).unwrap();
    assert_eq!(written.get("results_status"), None);
}

#[tokio::test]
async fn polls_an_acceptance_test_until_it_completes() {
    let server = MockServer::start(vec![
        MockResponse::json(200, test_json("started")),
        MockResponse::json(200, test_json("started")),
        MockResponse::json(200, completed_json()),
        MockResponse::json(200, json!({ "runs": [] })),
    ])
    .await;
    let params = AcceptanceGetParams {
        polling: Some(support::fast_polling()),
    };
    let test = support::platform(&server)
        .get_acceptance("routing", "acceptance-1", params)
        .await
        .unwrap();

    // The runs come once, after the last poll.
    let mut expected = vec![format!("GET {TESTS}/acceptance-1"); 3];
    expected.push(format!(
        "GET {EXPERIMENTS}/batch/acceptance-1/runs?pagereturn=true"
    ));
    assert_eq!(calls(&server), expected);
    assert_eq!(test.status, Some(ExperimentStatus::Completed));
    assert!(test.results.unwrap().passed);
}

#[tokio::test]
async fn reads_the_runs_of_the_batch_the_test_names() {
    let mut test = completed_json();
    test["experiment_id"] = json!("batch-7");
    let server = MockServer::start(vec![
        MockResponse::json(200, test),
        MockResponse::json(200, json!({ "runs": [] })),
    ])
    .await;
    support::platform(&server)
        .get_acceptance("routing", "acceptance-1", AcceptanceGetParams::default())
        .await
        .unwrap();
    assert_eq!(
        calls(&server)[1],
        format!("GET {EXPERIMENTS}/batch/batch-7/runs?pagereturn=true")
    );
}

#[tokio::test]
async fn lists_acceptance_tests_without_runs() {
    let server = MockServer::start(vec![MockResponse::json(
        200,
        json!({ "items": [created_json()] }),
    )])
    .await;
    let tests = support::platform(&server)
        .list_acceptances("routing")
        .await
        .unwrap();
    assert_eq!(tests[0].runs, None);
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn stops_polling_after_max_tries() {
    let server = MockServer::start(vec![
        MockResponse::json(200, test_json("started")),
        MockResponse::json(200, test_json("started")),
    ])
    .await;
    let params = AcceptanceGetParams {
        polling: Some(PollingOptions {
            max_tries: Some(2),
            ..support::fast_polling()
        }),
    };
    let err = support::platform(&server)
        .get_acceptance("routing", "acceptance-1", params)
        .await
        .unwrap_err();
    let Error::Wait { reason, .. } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(
        reason,
        "no terminal state after 2 tries, the `max_tries` limit"
    );
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn lists_acceptance_tests_across_pages() {
    let mut second = test_json("started");
    second["id"] = json!("acceptance-2");
    let server = MockServer::start(vec![
        MockResponse::json(
            200,
            json!({ "items": [test_json("completed")], "next_page_token": "page-2" }),
        ),
        MockResponse::json(200, json!({ "items": [second] })),
    ])
    .await;
    let tests = support::platform(&server)
        .list_acceptances("routing")
        .await
        .unwrap();
    let ids: Vec<&str> = tests
        .iter()
        .map(|test| test.acceptance_id.as_str())
        .collect();
    assert_eq!(ids, ["acceptance-1", "acceptance-2"]);

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
async fn updates_only_the_fields_set() {
    let cases = [
        (
            AcceptanceUpdateParams {
                description: Some("Version 2 against production.".to_owned()),
                ..AcceptanceUpdateParams::default()
            },
            json!({ "description": "Version 2 against production." }),
        ),
        (
            AcceptanceUpdateParams {
                name: Some("Version 2".to_owned()),
                description: Some(String::new()),
            },
            json!({ "name": "Version 2", "description": "" }),
        ),
    ];
    for (params, expected) in cases {
        let server = MockServer::start(vec![MockResponse::json(200, updated_json())]).await;
        let test = support::platform(&server)
            .update_acceptance("routing", "acceptance-1", params)
            .await
            .unwrap();

        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "PATCH");
        assert_eq!(requests[0].target, format!("{TESTS}/acceptance-1"));
        assert_eq!(support::body(&requests[0]), expected);
        assert_eq!(test.description, "Version 2 against production.");
        assert_eq!(test.updated_at, "2026-10-20T15:00:00Z");
        assert_eq!(test.metrics, None);
    }
}

#[tokio::test]
async fn rejects_an_update_that_changes_nothing() {
    let empty = AcceptanceUpdateParams {
        name: Some(String::new()),
        description: Some(String::new()),
    };
    for params in [AcceptanceUpdateParams::default(), empty] {
        let server = MockServer::start(Vec::new()).await;
        let err = support::platform(&server)
            .update_acceptance("routing", "acceptance-1", params)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
        assert!(server.requests().is_empty());
    }
}

/// The method and target of each request `server` got.
fn calls(server: &MockServer) -> Vec<String> {
    server
        .requests()
        .into_iter()
        .map(|r| format!("{} {}", r.method, r.target))
        .collect()
}

/// A list of acceptance tests whose batch experiments are `batch_ids`, by
/// test, from `acceptance-1`.
fn list_json(batch_ids: &[&str]) -> Value {
    let items: Vec<Value> = batch_ids
        .iter()
        .enumerate()
        .map(|(i, batch_id)| {
            let mut test = created_json();
            test["id"] = json!(format!("acceptance-{}", i + 1));
            test["experiment_id"] = json!(batch_id);
            test
        })
        .collect();
    json!({ "items": items })
}

#[tokio::test]
async fn deletes_an_acceptance_test_then_its_batch() {
    let server = MockServer::start(vec![
        MockResponse::json(200, list_json(&["acceptance-1", "acceptance-2"])),
        MockResponse::new(204),
        MockResponse::new(204),
    ])
    .await;
    support::platform(&server)
        .delete_acceptance("routing", "acceptance-1", Default::default())
        .await
        .unwrap();
    assert_eq!(
        calls(&server),
        [
            format!("GET {TESTS}?pagereturn=true"),
            format!("DELETE {TESTS}/acceptance-1"),
            format!("DELETE {EXPERIMENTS}/batch/acceptance-1"),
        ]
    );
}

#[tokio::test]
async fn keeps_a_batch_that_another_test_shares() {
    let server = MockServer::start(vec![
        MockResponse::json(200, list_json(&["shared", "shared"])),
        MockResponse::new(204),
    ])
    .await;
    support::platform(&server)
        .delete_acceptance("routing", "acceptance-1", Default::default())
        .await
        .unwrap();
    assert_eq!(
        calls(&server),
        [
            format!("GET {TESTS}?pagereturn=true"),
            format!("DELETE {TESTS}/acceptance-1"),
        ]
    );
}

#[tokio::test]
async fn counts_a_missing_batch_as_deleted() {
    let server = MockServer::start(vec![
        MockResponse::json(200, list_json(&["acceptance-1"])),
        MockResponse::new(204),
        MockResponse::new(404).body("batch experiment not found"),
    ])
    .await;
    support::platform(&server)
        .delete_acceptance("routing", "acceptance-1", Default::default())
        .await
        .unwrap();
    assert_eq!(server.requests().len(), 3);
}

#[tokio::test]
async fn returns_the_error_of_a_batch_delete_after_the_test_is_gone() {
    let server = MockServer::start(vec![
        MockResponse::json(200, list_json(&["acceptance-1"])),
        MockResponse::new(204),
        MockResponse::new(400).body("experiment is still referenced"),
    ])
    .await;
    let err = support::platform(&server)
        .delete_acceptance("routing", "acceptance-1", Default::default())
        .await
        .unwrap_err();
    let Error::Status { status, url, .. } = &err else {
        panic!("{err}");
    };
    assert_eq!(*status, 400);
    assert!(url.ends_with("/experiments/batch/acceptance-1"), "{url}");
}

#[tokio::test]
async fn passes_on_the_404_of_a_test_that_is_not_listed() {
    let server = MockServer::start(vec![
        MockResponse::json(200, list_json(&["acceptance-1"])),
        MockResponse::new(404).body("acceptance test not found"),
    ])
    .await;
    let err = support::platform(&server)
        .delete_acceptance("routing", "acceptance-9", Default::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Status { status: 404, .. }), "{err}");
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn polls_until_the_batch_is_gone() {
    let server = MockServer::start(vec![
        MockResponse::json(200, list_json(&["acceptance-1"])),
        MockResponse::new(204),
        MockResponse::new(204),
        MockResponse::json(200, json!({ "id": "acceptance-1", "status": "deleting" })),
        MockResponse::new(404).body("batch experiment not found"),
    ])
    .await;
    let params = AcceptanceDeleteParams {
        polling: Some(support::fast_polling()),
    };
    support::platform(&server)
        .delete_acceptance("routing", "acceptance-1", params)
        .await
        .unwrap();
    let calls = calls(&server);
    assert_eq!(calls[3], format!("GET {EXPERIMENTS}/batch/acceptance-1"));
    assert_eq!(calls.len(), 5);
}

#[tokio::test]
async fn ends_the_polling_of_a_failed_batch_delete_with_an_error() {
    let server = MockServer::start(vec![
        MockResponse::json(200, list_json(&["acceptance-1"])),
        MockResponse::new(204),
        MockResponse::new(204),
        MockResponse::json(
            200,
            json!({ "id": "acceptance-1", "status": "delete-failed" }),
        ),
    ])
    .await;
    let params = AcceptanceDeleteParams {
        polling: Some(support::fast_polling()),
    };
    let err = support::platform(&server)
        .delete_acceptance("routing", "acceptance-1", params)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "stopped waiting: the platform could not delete the batch experiment"
    );
}

/// Arguments of the create tool.
fn create_args() -> Value {
    json!({
        "workspace_id": "routing",
        "acceptance_id": "acceptance-1",
        "baseline_environment_id": "production",
        "candidate_environment_id": "candidate",
        "input_set_id": "last-week",
        "metrics": [metric_json()],
    })
}

#[tokio::test]
async fn serves_the_create_tool() {
    let (result, requests) = support::serve_tool(
        create_responses("acceptance-1"),
        "decision_platform_acceptance_create",
        create_args(),
    )
    .await;
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].target, format!("{EXPERIMENTS}/batch"));
    assert_eq!(
        support::body(&requests[2])["metrics"],
        json!([metric_json()])
    );
    let result = result.unwrap();
    assert_eq!(result["id"], "acceptance-1");
    assert_eq!(result["control"]["instance_id"], "production");
}

#[tokio::test]
async fn rejects_bad_arguments_in_the_create_tool() {
    let mut no_input_set = create_args();
    no_input_set.as_object_mut().unwrap().remove("input_set_id");
    let mut unknown_operator = create_args();
    unknown_operator["metrics"][0]["params"]["operator"] = json!("unknown");
    let cases = [
        (no_input_set, "missing field `input_set_id`"),
        (unknown_operator, "unknown variant `unknown`"),
    ];
    for (args, message) in cases {
        let (result, requests) =
            support::serve_tool(Vec::new(), "decision_platform_acceptance_create", args).await;
        let err = result.unwrap_err();
        assert!(
            matches!(&err, ToolError::InvalidArguments(m) if m.contains(message)),
            "{err}"
        );
        assert!(requests.is_empty());
    }
}

#[tokio::test]
async fn serves_the_get_tool_with_default_polling() {
    let (result, requests) = support::serve_tool(
        vec![
            MockResponse::json(200, completed_json()),
            MockResponse::json(200, runs_json("candidate-run-1")),
        ],
        "decision_platform_acceptance_get",
        json!({
            "workspace_id": "routing",
            "acceptance_id": "acceptance-1",
            "polling": { "initial_delay": 0, "jitter": 0 },
        }),
    )
    .await;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].target, format!("{TESTS}/acceptance-1"));
    let result = result.unwrap();
    assert_eq!(result["status"], "completed");
    let candidate = &result["results"]["metric_results"][0]["statistics"]["candidate"];
    assert_eq!(candidate["distribution_summary_statistics"]["mean"], "nan");
    assert_eq!(result["runs"][0]["id"], "candidate-run-1");
}

#[tokio::test]
async fn serves_the_list_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::json(
            200,
            json!({ "items": [test_json("completed")] }),
        )],
        "decision_platform_acceptance_list",
        json!({ "workspace_id": "routing" }),
    )
    .await;
    assert_eq!(requests[0].target, format!("{TESTS}?pagereturn=true"));
    assert_eq!(result.unwrap()[0]["id"], "acceptance-1");
}

#[tokio::test]
async fn serves_the_update_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::json(200, updated_json())],
        "decision_platform_acceptance_update",
        json!({
            "workspace_id": "routing",
            "acceptance_id": "acceptance-1",
            "description": "Version 2 against production.",
        }),
    )
    .await;
    assert_eq!(requests[0].method, "PATCH");
    assert_eq!(
        support::body(&requests[0]),
        json!({ "description": "Version 2 against production." })
    );
    assert_eq!(
        result.unwrap()["description"],
        "Version 2 against production."
    );
}

#[tokio::test]
async fn serves_the_delete_tool() {
    let (result, requests) = support::serve_tool(
        vec![
            MockResponse::json(200, list_json(&["acceptance-1"])),
            MockResponse::new(204),
            MockResponse::new(204),
        ],
        "decision_platform_acceptance_delete",
        json!({ "workspace_id": "routing", "acceptance_id": "acceptance-1" }),
    )
    .await;
    assert_eq!(requests[1].method, "DELETE");
    assert_eq!(requests[1].target, format!("{TESTS}/acceptance-1"));
    assert_eq!(
        requests[2].target,
        format!("{EXPERIMENTS}/batch/acceptance-1")
    );
    assert_eq!(result.unwrap(), Value::Null);
}
