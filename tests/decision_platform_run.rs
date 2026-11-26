//! Tests for the platform run operations and their tools, against the mock
//! server.

mod mock_server;
mod support;

use std::fs;
use std::path::PathBuf;

use fico_sdk::Error;
use fico_sdk::decision::local::Local;
use fico_sdk::decision::platform::{
    DEFAULT_CONSOLE_URL, RunCloneParams, RunCompareParams, RunCreateParams, RunDeleteParams,
    RunGetParams, RunInputGetParams, RunListParams, RunLogsGetParams, RunTrackParams,
    RunUpdateParams,
};
use fico_sdk::decision::{
    ContentFormat, Format, FormatInput, PollingOptions, RunConfiguration, RunQueuing, RunStatus,
    RunType, RunTypeConfiguration, TrackedRun, TrackedRunStatus,
};
use fico_sdk::tool::{Context, ToolError};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use mock_server::{MockResponse, MockServer, RecordedRequest};
use serde_json::{Value, json};
use support::TempDir;

/// Over the 5 MiB that the platform returns inline.
const LARGE: u64 = 6 * 1024 * 1024;

/// A run's information as `GET …/runs/run-1/metadata` returns it: a
/// succeeded run with small JSON input and output.
fn information_json() -> Value {
    json!({
        "id": "run-1",
        "user_email": "ana@example.com",
        "name": "Monday",
        "description": "Weekly plan",
        "metadata": {
            "status": "succeeded",
            "status_v2": "succeeded",
            "created_at": "2026-10-07T14:19:02Z",
            "initiated_at": "2026-10-07T14:19:03Z",
            "duration": 5021,
            "execution_duration": 4890,
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

/// `information` with its status set to `status`.
fn with_status(mut information: Value, status: &str) -> Value {
    information["metadata"]["status"] = json!(status);
    information["metadata"]["status_v2"] = json!(status);
    information
}

/// `information` with `output` set to `output`, as `GET …/runs/run-1`
/// returns it.
fn with_output(mut information: Value, output: Value) -> Value {
    information["output"] = output;
    information
}

/// Output of a routing run.
fn output_json() -> Value {
    json!({
        "solutions": [{ "routes": [{ "vehicle": "truck-1", "stops": ["a", "b"] }] }],
        "metrics": { "cost": 12.5 },
    })
}

/// A run as `GET …/runs` lists it.
fn run_json(id: &str, status: &str) -> Value {
    json!({
        "id": id,
        "user_email": "ana@example.com",
        "name": "Monday",
        "description": "",
        "created_at": "2026-10-07T14:19:02Z",
        "application_id": "routing",
        "application_instance_id": "production",
        "application_version_id": "v1",
        "run_type": { "type": "standard", "definition_id": "", "reference_id": "" },
        "execution_class": "6c9500mb870s",
        "runtime": "ghcr.io/nextmv-io/runtime/default:latest",
        "status": status,
        "status_v2": status,
    })
}

/// A tar.gz of `files`, as (path, contents), as the platform stores
/// multi-file content.
fn tar_gz(files: &[(&str, &str)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    for (path, contents) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        builder
            .append_data(&mut header, path, contents.as_bytes())
            .unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

/// A presigned URL on `bucket`, another origin than the API's.
fn presigned(bucket: &MockServer, key: &str) -> String {
    format!("{}/{key}?X-Amz-Signature=abc", bucket.url())
}

/// The method and target of each request, e.g. `GET /v1/apps?x=1`.
fn sent(requests: &[RecordedRequest]) -> Vec<String> {
    let line = |r: &RecordedRequest| format!("{} {}", r.method, r.target);
    requests.iter().map(line).collect()
}

/// Asserts that every request to the API sent the bearer token.
fn assert_authorized(requests: &[RecordedRequest]) {
    for request in requests {
        assert_eq!(
            request.header("authorization"),
            Some("Bearer secret"),
            "{}",
            request.target
        );
    }
}

#[tokio::test]
async fn gets_a_run_with_inline_output() {
    let server = MockServer::start(vec![
        MockResponse::json(200, information_json()),
        MockResponse::json(200, with_output(information_json(), output_json())),
    ])
    .await;
    let result = support::platform(&server)
        .get_run("routing", "run-1", RunGetParams::default())
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(
        sent(&requests),
        [
            "GET /v1/applications/routing/runs/run-1/metadata",
            "GET /v1/applications/routing/runs/run-1",
        ]
    );
    assert_authorized(&requests);
    assert_eq!(result.information.id, "run-1");
    assert_eq!(result.information.metadata.status, RunStatus::Succeeded);
    assert_eq!(result.information.metadata.workspace_id, "routing");
    assert_eq!(Value::Object(result.output.clone().unwrap()), output_json());
    assert_eq!(result.ensemble, None);
    // Emitted JSON is one object with the API names.
    let emitted = serde_json::to_value(&result).unwrap();
    assert_eq!(emitted["metadata"]["status_v2"], "succeeded");
    assert_eq!(emitted["output"], output_json());
}

#[tokio::test]
async fn downloads_large_json_output() {
    let bucket = MockServer::start(vec![MockResponse::json(200, output_json())]).await;
    let url = presigned(&bucket, "output/run-1-output.json");
    let mut information = information_json();
    information["metadata"]["output_size"] = json!(LARGE);
    let server = MockServer::start(vec![
        MockResponse::json(200, information.clone()),
        MockResponse::json(200, with_output(information, json!({ "url": url }))),
        MockResponse::json(200, json!({ "url": url })),
    ])
    .await;
    let result = support::platform(&server)
        .get_run("routing", "run-1", RunGetParams::default())
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(
        sent(&requests),
        [
            "GET /v1/applications/routing/runs/run-1/metadata",
            "GET /v1/applications/routing/runs/run-1?format=url",
            "GET /v1/applications/routing/runs/run-1/output?format=url",
        ]
    );
    assert_authorized(&requests);
    let downloads = bucket.requests();
    assert_eq!(
        sent(&downloads),
        ["GET /output/run-1-output.json?X-Amz-Signature=abc"]
    );
    assert_eq!(downloads[0].header("authorization"), None);
    assert_eq!(Value::Object(result.output.unwrap()), output_json());
}

#[tokio::test]
async fn unpacks_multi_file_output_into_output_dir() {
    let archive = tar_gz(&[
        ("solution.csv", "vehicle,stop\ntruck-1,a\n"),
        ("plots/map.json", "{}"),
    ]);
    let bucket = MockServer::start(vec![MockResponse::new(200).body(archive)]).await;
    let url = presigned(&bucket, "output/run-1-output.tar.gz");
    let mut information = information_json();
    information["metadata"]["format"]["output"]["type"] = json!("multi-file");
    let server = MockServer::start(vec![
        MockResponse::json(200, information.clone()),
        MockResponse::json(200, with_output(information, json!({ "url": url }))),
        MockResponse::json(200, json!({ "url": url })),
    ])
    .await;
    let dir = TempDir::new("run");
    let output_dir = dir.path().join("run-1-output");
    let params = RunGetParams {
        output_dir: Some(output_dir.clone()),
        ..Default::default()
    };
    let result = support::platform(&server)
        .get_run("routing", "run-1", params)
        .await
        .unwrap();

    assert_eq!(
        sent(&server.requests()),
        [
            "GET /v1/applications/routing/runs/run-1/metadata",
            "GET /v1/applications/routing/runs/run-1?format=url",
            "GET /v1/applications/routing/runs/run-1/output?format=url",
        ]
    );
    let downloads = bucket.requests();
    assert_eq!(downloads.len(), 1);
    assert_eq!(downloads[0].header("authorization"), None);
    assert_eq!(
        fs::read_to_string(output_dir.join("solution.csv")).unwrap(),
        "vehicle,stop\ntruck-1,a\n"
    );
    assert_eq!(
        fs::read_to_string(output_dir.join("plots/map.json")).unwrap(),
        "{}"
    );
    // The files are the output: the `{"url": …}` placeholder is gone.
    assert_eq!(result.output, None);
}

#[tokio::test]
async fn needs_output_dir_for_multi_file_output() {
    let mut information = information_json();
    information["metadata"]["format"]["output"]["type"] = json!("multi-file");
    let server = MockServer::start(vec![MockResponse::json(200, information)]).await;
    let err = support::platform(&server)
        .get_run("routing", "run-1", RunGetParams::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "invalid argument: the run's output is multi-file: set `output_dir` to download it, \
         or get the run's information, which leaves the output out"
    );
    // The metadata already says the run succeeded, so there is no `GET R`,
    // for which the platform would copy the output.
    assert_eq!(
        sent(&server.requests()),
        ["GET /v1/applications/routing/runs/run-1/metadata"]
    );
}

#[tokio::test]
async fn needs_output_dir_for_a_run_that_finished_after_its_metadata() {
    let mut information = information_json();
    information["metadata"]["format"]["output"]["type"] = json!("multi-file");
    let url = "https://bucket.example.com/output/run-1-output.tar.gz?X-Amz-Signature=abc";
    let server = MockServer::start(vec![
        MockResponse::json(200, with_status(information.clone(), "running")),
        MockResponse::json(200, with_output(information, json!({ "url": url }))),
    ])
    .await;
    let err = support::platform(&server)
        .get_run("routing", "run-1", RunGetParams::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    // Nothing is downloaded.
    assert_eq!(
        sent(&server.requests()),
        [
            "GET /v1/applications/routing/runs/run-1/metadata",
            "GET /v1/applications/routing/runs/run-1?format=url",
        ]
    );
}

#[tokio::test]
async fn downloads_the_output_of_a_run_that_finished_after_its_metadata() {
    let archive = tar_gz(&[("solution.csv", "vehicle,stop\n")]);
    let bucket = MockServer::start(vec![MockResponse::new(200).body(archive)]).await;
    let url = presigned(&bucket, "output/run-1-output.tar.gz");
    let mut information = information_json();
    information["metadata"]["format"]["output"]["type"] = json!("multi-file");
    let server = MockServer::start(vec![
        // Still running when the metadata is read.
        MockResponse::json(200, with_status(information.clone(), "running")),
        MockResponse::json(200, with_output(information, json!({ "url": url }))),
        MockResponse::json(200, json!({ "url": url })),
    ])
    .await;
    let dir = TempDir::new("run");
    let output_dir = dir.path().join("run-1-output");
    let params = RunGetParams {
        output_dir: Some(output_dir.clone()),
        ..Default::default()
    };
    let result = support::platform(&server)
        .get_run("routing", "run-1", params)
        .await
        .unwrap();
    assert_eq!(
        sent(&server.requests()),
        [
            "GET /v1/applications/routing/runs/run-1/metadata",
            "GET /v1/applications/routing/runs/run-1?format=url",
            "GET /v1/applications/routing/runs/run-1/output?format=url",
        ]
    );
    assert_eq!(bucket.requests().len(), 1);
    assert_eq!(
        fs::read_to_string(output_dir.join("solution.csv")).unwrap(),
        "vehicle,stop\n"
    );
    assert_eq!(result.information.metadata.status, RunStatus::Succeeded);
    assert_eq!(result.output, None);
}

#[tokio::test]
async fn keeps_the_error_log_link_of_a_large_failed_run() {
    // Over the platform's 5.8 MiB, it answers a failed run's `?format=url`
    // with a link to the error log, instead of the error log.
    let mut information = with_status(information_json(), "failed");
    information["metadata"]["output_size"] = json!(7 * 1024 * 1024);
    let link = json!({ "error_log_url": "https://bucket.example.com/run-1-output-failed?X-Amz-Signature=abc" });
    let server = MockServer::start(vec![
        MockResponse::json(200, information.clone()),
        MockResponse::json(200, with_output(information, link.clone())),
    ])
    .await;
    let result = support::platform(&server)
        .get_run("routing", "run-1", RunGetParams::default())
        .await
        .unwrap();
    assert_eq!(
        sent(&server.requests()),
        [
            "GET /v1/applications/routing/runs/run-1/metadata",
            "GET /v1/applications/routing/runs/run-1?format=url",
        ]
    );
    assert_eq!(result.error_log, None);
    assert_eq!(Value::Object(result.output.unwrap()), link);
}

#[tokio::test]
async fn downloads_nothing_until_the_run_succeeds() {
    let mut failed = with_status(information_json(), "failed");
    failed["error_log"] = json!({ "error": "exit status 1", "stdout": "", "stderr": "panic" });
    for (status, result) in [
        ("running", with_status(information_json(), "running")),
        ("failed", failed),
    ] {
        let mut information = with_status(information_json(), status);
        information["metadata"]["format"]["output"]["type"] = json!("multi-file");
        let server = MockServer::start(vec![
            MockResponse::json(200, information),
            MockResponse::json(200, result),
        ])
        .await;
        let dir = TempDir::new("run");
        let output_dir = dir.path().join("run-1-output");
        let params = RunGetParams {
            output_dir: Some(output_dir.clone()),
            ..Default::default()
        };
        let result = support::platform(&server)
            .get_run("routing", "run-1", params)
            .await
            .unwrap();
        assert_eq!(
            sent(&server.requests()),
            [
                "GET /v1/applications/routing/runs/run-1/metadata",
                "GET /v1/applications/routing/runs/run-1?format=url",
            ],
            "{status}"
        );
        assert!(!output_dir.exists(), "{status}");
        assert_eq!(result.output, None, "{status}");
        if status == "failed" {
            let errors = result.error_log.unwrap();
            assert_eq!(errors.stderr.as_deref(), Some("panic"));
        }
    }
}

#[tokio::test]
async fn gets_the_ensemble_of_an_ensemble_run() {
    let mut information = information_json();
    information["metadata"]["run_type"] =
        json!({ "type": "ensemble", "definition_id": "ens-1", "reference_id": "" });
    let ensemble = json!({
        "id": "run-1",
        "application_id": "routing",
        "evaluation_type": "rules",
        "definition_id": "ens-1",
        "status_v2": "succeeded",
        "error": "",
        "rules_result": { "best_run_id": "run-2" },
    });
    let server = MockServer::start(vec![
        MockResponse::json(200, information.clone()),
        MockResponse::json(200, with_output(information, output_json())),
        MockResponse::json(200, ensemble.clone()),
    ])
    .await;
    let result = support::platform(&server)
        .get_run("routing", "run-1", RunGetParams::default())
        .await
        .unwrap();
    assert_eq!(
        sent(&server.requests()),
        [
            "GET /v1/applications/routing/runs/run-1/metadata",
            "GET /v1/applications/routing/runs/run-1",
            "GET /v1/applications/routing/runs/run-1/ensemble",
        ]
    );
    assert_eq!(Value::Object(result.ensemble.unwrap()), ensemble);
    assert_eq!(Value::Object(result.output.unwrap()), output_json());
}

#[tokio::test]
async fn polls_until_the_run_is_done() {
    let server = MockServer::start(vec![
        MockResponse::json(200, with_status(information_json(), "queued")),
        MockResponse::json(200, with_status(information_json(), "running")),
        MockResponse::json(200, information_json()),
        MockResponse::json(200, with_output(information_json(), output_json())),
    ])
    .await;
    let options = support::fast_polling();
    let params = RunGetParams {
        polling: Some(options.clone()),
        ..Default::default()
    };
    let result = support::platform(&server)
        .get_run("routing", "run-1", params)
        .await
        .unwrap();
    let metadata = "GET /v1/applications/routing/runs/run-1/metadata";
    assert_eq!(
        sent(&server.requests()),
        [
            metadata,
            metadata,
            metadata,
            "GET /v1/applications/routing/runs/run-1"
        ]
    );
    assert_eq!(result.information.metadata.status, RunStatus::Succeeded);
    assert_eq!(Value::Object(result.output.unwrap()), output_json());
    // Polling only reads the options.
    assert_eq!(options, support::fast_polling());
}

#[tokio::test]
async fn stops_polling_after_max_tries() {
    let running = with_status(information_json(), "running");
    let server = MockServer::start(vec![
        MockResponse::json(200, running.clone()),
        MockResponse::json(200, running),
    ])
    .await;
    let params = RunGetParams {
        polling: Some(PollingOptions {
            max_tries: Some(2),
            ..support::fast_polling()
        }),
        ..Default::default()
    };
    let err = support::platform(&server)
        .get_run("routing", "run-1", params)
        .await
        .unwrap_err();
    let Error::Wait { reason, .. } = &err else {
        panic!("{err}");
    };
    assert_eq!(
        reason,
        "no terminal state after 2 tries, the `max_tries` limit"
    );
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn gets_run_information() {
    let server = MockServer::start(vec![MockResponse::json(200, information_json())]).await;
    let information = support::platform(&server)
        .get_run_information("routing", "run-1")
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(
        sent(&requests),
        ["GET /v1/applications/routing/runs/run-1/metadata"]
    );
    assert_authorized(&requests);
    assert_eq!(information.name, "Monday");
    assert_eq!(information.metadata.environment_id, "production");
    assert_eq!(information.metadata.output_size, 2048.0);
    // The platform sends no console URL: the SDK links the run.
    assert_eq!(
        information.console_url,
        format!(
            "{}/app/routing/run/run-1?view=details",
            DEFAULT_CONSOLE_URL.expect("a default console URL")
        )
    );
    let emitted = serde_json::to_value(&information).unwrap();
    assert_eq!(emitted["metadata"]["application_instance_id"], "production");
    assert!(emitted["metadata"].get("status").is_none());
}

#[tokio::test]
async fn gets_json_input() {
    let input = json!({ "stops": ["a", "b"], "vehicles": ["truck-1"] });
    let server = MockServer::start(vec![
        MockResponse::json(200, information_json()),
        MockResponse::json(200, input.clone()),
    ])
    .await;
    let got = support::platform(&server)
        .get_run_input("routing", "run-1", RunInputGetParams::default())
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(
        sent(&requests),
        [
            "GET /v1/applications/routing/runs/run-1/metadata",
            "GET /v1/applications/routing/runs/run-1/input",
        ]
    );
    assert_authorized(&requests);
    assert_eq!(got, Some(input));
}

#[tokio::test]
async fn downloads_large_json_input() {
    let input = json!({ "stops": ["a", "b"] });
    let bucket = MockServer::start(vec![MockResponse::json(200, input.clone())]).await;
    let url = presigned(&bucket, "input/run-1-input.json");
    let mut information = information_json();
    information["metadata"]["input_size"] = json!(LARGE);
    let server = MockServer::start(vec![
        MockResponse::json(200, information),
        MockResponse::json(200, json!({ "url": url })),
    ])
    .await;
    let got = support::platform(&server)
        .get_run_input("routing", "run-1", RunInputGetParams::default())
        .await
        .unwrap();
    assert_eq!(
        sent(&server.requests()),
        [
            "GET /v1/applications/routing/runs/run-1/metadata",
            "GET /v1/applications/routing/runs/run-1/input?format=url",
        ]
    );
    let downloads = bucket.requests();
    assert_eq!(
        sent(&downloads),
        ["GET /input/run-1-input.json?X-Amz-Signature=abc"]
    );
    assert_eq!(downloads[0].header("authorization"), None);
    assert_eq!(got, Some(input));
}

#[tokio::test]
async fn unpacks_multi_file_input_into_output_dir() {
    let archive = tar_gz(&[("stops.csv", "id\na\nb\n")]);
    let bucket = MockServer::start(vec![MockResponse::new(200).body(archive)]).await;
    let url = presigned(&bucket, "input/run-1-input.tar.gz");
    let mut information = information_json();
    information["metadata"]["format"]["input"]["type"] = json!("multi-file");
    let server = MockServer::start(vec![
        MockResponse::json(200, information),
        MockResponse::json(200, json!({ "url": url })),
    ])
    .await;
    let dir = TempDir::new("run");
    let output_dir = dir.path().join("run-1-input");
    let params = RunInputGetParams {
        output_dir: Some(output_dir.clone()),
    };
    let got = support::platform(&server)
        .get_run_input("routing", "run-1", params)
        .await
        .unwrap();
    assert_eq!(
        sent(&server.requests()),
        [
            "GET /v1/applications/routing/runs/run-1/metadata",
            "GET /v1/applications/routing/runs/run-1/input?format=url",
        ]
    );
    assert_eq!(bucket.requests()[0].header("authorization"), None);
    assert_eq!(got, None);
    assert_eq!(
        fs::read_to_string(output_dir.join("stops.csv")).unwrap(),
        "id\na\nb\n"
    );
}

#[tokio::test]
async fn needs_output_dir_for_multi_file_input() {
    let mut information = information_json();
    information["metadata"]["format"]["input"]["type"] = json!("multi-file");
    let server = MockServer::start(vec![MockResponse::json(200, information)]).await;
    let err = support::platform(&server)
        .get_run_input("routing", "run-1", RunInputGetParams::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "invalid argument: the run's input is multi-file: set `output_dir` to download it"
    );
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn gets_run_logs() {
    let server = MockServer::start(vec![MockResponse::json(
        200,
        json!({ "log": "solving\ndone\n" }),
    )])
    .await;
    let logs = support::platform(&server)
        .get_run_logs("routing", "run-1", RunLogsGetParams::default())
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(
        sent(&requests),
        ["GET /v1/applications/routing/runs/run-1/logs"]
    );
    assert_authorized(&requests);
    assert_eq!(logs.log, "solving\ndone\n");
}

#[tokio::test]
async fn polls_before_getting_run_logs() {
    let server = MockServer::start(vec![
        MockResponse::json(200, with_status(information_json(), "running")),
        MockResponse::json(200, with_status(information_json(), "failed")),
        MockResponse::json(200, json!({ "log": "panic\n" })),
    ])
    .await;
    let params = RunLogsGetParams {
        polling: Some(support::fast_polling()),
    };
    let logs = support::platform(&server)
        .get_run_logs("routing", "run-1", params)
        .await
        .unwrap();
    let metadata = "GET /v1/applications/routing/runs/run-1/metadata";
    assert_eq!(
        sent(&server.requests()),
        [
            metadata,
            metadata,
            "GET /v1/applications/routing/runs/run-1/logs"
        ]
    );
    assert_eq!(logs.log, "panic\n");
}

#[tokio::test]
async fn lists_runs_across_pages() {
    let server = MockServer::start(vec![
        MockResponse::json(
            200,
            json!({ "runs": [run_json("run-3", "running")], "next_page_token": "page-2" }),
        ),
        MockResponse::json(
            200,
            json!({ "runs": [run_json("run-2", "succeeded"), run_json("run-1", "failed")] }),
        ),
    ])
    .await;
    let runs = support::platform(&server)
        .list_runs("routing", RunListParams::default())
        .await
        .unwrap();
    let ids: Vec<&str> = runs.iter().map(|run| run.id.as_str()).collect();
    assert_eq!(ids, ["run-3", "run-2", "run-1"]);
    assert_eq!(runs[1].status, RunStatus::Succeeded);
    assert_eq!(runs[1].environment_id, "production");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_authorized(&requests);
    for request in &requests {
        assert_eq!(request.method, "GET");
        assert!(request.target.starts_with("/v1/applications/routing/runs?"));
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
async fn filters_runs_by_status() {
    let server = MockServer::start(vec![MockResponse::json(
        200,
        json!({ "runs": [
            run_json("run-3", "running"),
            run_json("run-2", "failed"),
            run_json("run-1", "failed"),
        ] }),
    )])
    .await;
    let params = RunListParams {
        status: Some(RunStatus::Failed),
        ..Default::default()
    };
    let runs = support::platform(&server)
        .list_runs("routing", params)
        .await
        .unwrap();
    let ids: Vec<&str> = runs.iter().map(|run| run.id.as_str()).collect();
    assert_eq!(ids, ["run-2", "run-1"]);
}

#[tokio::test]
async fn stops_listing_runs_at_the_limit() {
    // The first page has one failed run, so the second is needed, and the
    // second page has enough, so there is no third request.
    let server = MockServer::start(vec![
        MockResponse::json(
            200,
            json!({
                "runs": [run_json("run-4", "succeeded"), run_json("run-3", "failed")],
                "next_page_token": "page-2",
            }),
        ),
        MockResponse::json(
            200,
            json!({
                "runs": [run_json("run-2", "failed"), run_json("run-1", "failed")],
                "next_page_token": "page-3",
            }),
        ),
    ])
    .await;
    let params = RunListParams {
        status: Some(RunStatus::Failed),
        limit: Some(2),
        ..Default::default()
    };
    let runs = support::platform(&server)
        .list_runs("routing", params)
        .await
        .unwrap();
    let ids: Vec<&str> = runs.iter().map(|run| run.id.as_str()).collect();
    assert_eq!(ids, ["run-3", "run-2"]);
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn sends_the_time_range_and_the_kinds_of_runs_to_list() {
    let server = MockServer::start(vec![MockResponse::json(200, json!({ "runs": [] }))]).await;
    let params = RunListParams {
        start_time: Some("2026-10-01T00:00:00Z".to_owned()),
        end_time: Some("2026-10-02T00:00:00Z".to_owned()),
        include_experiment_runs: Some(true),
        include_shadow_runs: Some(false),
        include_ensemble_runs: Some(true),
        ..Default::default()
    };
    support::platform(&server)
        .list_runs("routing", params)
        .await
        .unwrap();
    assert_eq!(
        server.requests()[0].query(),
        support::pairs(&[
            ("start", "2026-10-01T00:00:00Z"),
            ("end", "2026-10-02T00:00:00Z"),
            ("include_exp_runs", "true"),
            ("include_shad_runs", "false"),
            ("include_ens_runs", "true"),
            ("pagereturn", "true"),
        ])
    );
}

#[tokio::test]
async fn sends_no_filters_by_default() {
    let server = MockServer::start(vec![MockResponse::json(200, json!({ "runs": [] }))]).await;
    let params = RunListParams {
        start_time: Some(String::new()),
        ..Default::default()
    };
    support::platform(&server)
        .list_runs("routing", params)
        .await
        .unwrap();
    assert_eq!(
        server.requests()[0].query(),
        support::pairs(&[("pagereturn", "true")])
    );
}

#[tokio::test]
async fn rejects_list_times_that_are_not_rfc3339() {
    let server = MockServer::start(Vec::new()).await;
    let params = RunListParams {
        end_time: Some("yesterday".to_owned()),
        ..Default::default()
    };
    let err = support::platform(&server)
        .list_runs("routing", params)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid argument: `end_time` must be an RFC 3339 timestamp, such as \
         `2026-10-01T12:00:00Z`"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn updates_a_runs_name_and_description() {
    let mut updated = information_json();
    updated["name"] = json!("Renamed");
    updated["description"] = json!("");
    let server = MockServer::start(vec![MockResponse::json(200, updated)]).await;
    let params = RunUpdateParams {
        name: Some("Renamed".to_owned()),
        description: Some(String::new()),
    };
    let information = support::platform(&server)
        .update_run("routing", "run-1", params)
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "PUT");
    assert_eq!(request.target, "/v1/applications/routing/runs/run-1");
    assert_eq!(
        support::body(request),
        json!({ "name": "Renamed", "description": "" })
    );
    assert_eq!(information.name, "Renamed");
    assert_eq!(information.description, "");
}

#[tokio::test]
async fn sends_only_the_run_fields_that_are_set() {
    let server = MockServer::start(vec![MockResponse::json(200, information_json())]).await;
    let params = RunUpdateParams {
        description: Some("Monday's plan".to_owned()),
        ..Default::default()
    };
    support::platform(&server)
        .update_run("routing", "run-1", params)
        .await
        .unwrap();
    assert_eq!(
        support::body(&server.requests()[0]),
        json!({ "description": "Monday's plan" })
    );
}

#[tokio::test]
async fn rejects_a_run_update_that_sets_nothing() {
    let server = MockServer::start(Vec::new()).await;
    let err = support::platform(&server)
        .update_run("routing", "run-1", RunUpdateParams::default())
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid argument: nothing to update: set `name` or `description`"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn serves_the_update_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(200, information_json())],
        "decision_platform_run_update",
        json!({ "workspace_id": "routing", "run_id": "run-1", "name": "Renamed" }),
    )
    .await;
    assert_eq!(sent(&requests), ["PUT /v1/applications/routing/runs/run-1"]);
    assert_eq!(support::body(&requests[0]), json!({ "name": "Renamed" }));
    assert_eq!(result["id"], "run-1");
}

#[tokio::test]
async fn rejects_a_zero_limit() {
    let server = MockServer::start(Vec::new()).await;
    let params = RunListParams {
        limit: Some(0),
        ..Default::default()
    };
    let err = support::platform(&server)
        .list_runs("routing", params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "invalid argument: `limit` must be at least 1"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn cancels_a_run() {
    let server = MockServer::start(vec![MockResponse::new(204)]).await;
    support::platform(&server)
        .cancel_run("routing", "run-1")
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(
        sent(&requests),
        ["PATCH /v1/applications/routing/runs/run-1/cancel"]
    );
    assert_authorized(&requests);
    assert!(requests[0].body.is_empty());
}

#[tokio::test]
async fn deletes_a_run() {
    let server = MockServer::start(vec![MockResponse::new(204)]).await;
    support::platform(&server)
        .delete_run("routing", "run-1", Default::default())
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(
        sent(&requests),
        ["DELETE /v1/applications/routing/runs/run-1"]
    );
    assert_authorized(&requests);
}

#[tokio::test]
async fn returns_the_platform_error_for_a_run_that_cannot_be_canceled() {
    let server = MockServer::start(vec![MockResponse::json(
        400,
        json!({ "status": "error", "error": "Run must be queued or running" }),
    )])
    .await;
    let err = support::platform(&server)
        .cancel_run("routing", "run-1")
        .await
        .unwrap_err();
    let Error::Status { status, body, .. } = &err else {
        panic!("{err}");
    };
    assert_eq!(*status, 400);
    assert!(body.contains("Run must be queued or running"), "{body}");
}

#[tokio::test]
async fn polls_a_delete_until_the_run_is_gone() {
    let server = MockServer::start(vec![
        MockResponse::new(204),
        MockResponse::json(200, information_json()),
        MockResponse::new(404).body("run not found"),
    ])
    .await;
    let params = RunDeleteParams {
        polling: Some(support::fast_polling()),
    };
    support::platform(&server)
        .delete_run("routing", "run-1", params)
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
            "DELETE /v1/applications/routing/runs/run-1",
            "GET /v1/applications/routing/runs/run-1/metadata",
            "GET /v1/applications/routing/runs/run-1/metadata",
        ]
    );
}

#[tokio::test]
async fn encodes_run_ids_in_paths() {
    let server = MockServer::start(vec![MockResponse::json(200, information_json())]).await;
    let platform = support::platform(&server);
    platform
        .get_run_information("my workspace", "run?1")
        .await
        .unwrap();
    assert_eq!(
        server.requests()[0].target,
        "/v1/applications/my%20workspace/runs/run%3F1/metadata"
    );
    for (workspace_id, run_id) in [
        ("routing", ".."),
        ("..", "run-1"),
        ("routing", ""),
        ("routing", "run/1"),
    ] {
        let err = platform
            .delete_run(workspace_id, run_id, Default::default())
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{run_id:?}: {err}");
        let err = platform.cancel_run(workspace_id, run_id).await.unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{run_id:?}: {err}");
    }
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn serves_the_get_tool_with_default_polling() {
    // The defaults wait a second before the first check, which finds the
    // run done.
    let (result, requests) = support::serve_tool_ok(
        vec![
            MockResponse::json(200, information_json()),
            MockResponse::json(200, with_output(information_json(), output_json())),
        ],
        "decision_platform_run_get",
        json!({ "workspace_id": "routing", "run_id": "run-1", "polling": {} }),
    )
    .await;
    assert_eq!(
        sent(&requests),
        [
            "GET /v1/applications/routing/runs/run-1/metadata",
            "GET /v1/applications/routing/runs/run-1",
        ]
    );
    assert_eq!(result["id"], "run-1");
    assert_eq!(result["metadata"]["status_v2"], "succeeded");
    assert_eq!(result["output"], output_json());
}

#[tokio::test]
async fn serves_the_get_tool_with_output_dir_in_local_mode() {
    let archive = tar_gz(&[("solution.csv", "vehicle,stop\n")]);
    let bucket = MockServer::start(vec![MockResponse::new(200).body(archive)]).await;
    let url = presigned(&bucket, "output/run-1-output.tar.gz");
    let mut information = information_json();
    information["metadata"]["format"]["output"]["type"] = json!("multi-file");
    let server = MockServer::start(vec![
        MockResponse::json(200, information.clone()),
        MockResponse::json(200, with_output(information, json!({ "url": url }))),
        MockResponse::json(200, json!({ "url": url })),
    ])
    .await;
    let dir = TempDir::new("run");
    let output_dir = dir.path().join("out");
    let cx = Context::new()
        .with_platform(support::platform(&server))
        .with_local(Local::with_home(dir.path().join(".fico")));
    let args = json!({ "workspace_id": "routing", "run_id": "run-1", "output_dir": output_dir });
    let result = support::call_tool(cx, "decision_platform_run_get", args)
        .await
        .unwrap();
    assert_eq!(server.requests().len(), 3);
    assert_eq!(
        fs::read_to_string(output_dir.join("solution.csv")).unwrap(),
        "vehicle,stop\n"
    );
    assert!(result.get("output").is_none(), "{result}");
}

#[tokio::test]
async fn refuses_output_dir_without_local_mode() {
    let server = MockServer::start(Vec::new()).await;
    let remote = Context::new().with_platform(support::platform(&server));
    for name in [
        "decision_platform_run_get",
        "decision_platform_run_input_get",
    ] {
        let args =
            json!({ "workspace_id": "routing", "run_id": "run-1", "output_dir": "/srv/data" });
        let err = support::call_tool(remote.clone(), name, args)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::Failed(Error::Invalid { .. })),
            "{name}: {err}"
        );
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn serves_the_information_get_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(200, information_json())],
        "decision_platform_run_information_get",
        json!({ "workspace_id": "routing", "run_id": "run-1" }),
    )
    .await;
    assert_eq!(
        sent(&requests),
        ["GET /v1/applications/routing/runs/run-1/metadata"]
    );
    assert_eq!(result["metadata"]["application_version_id"], "v1");
}

#[tokio::test]
async fn serves_the_input_get_tool() {
    let input = json!({ "stops": ["a"] });
    let (result, requests) = support::serve_tool_ok(
        vec![
            MockResponse::json(200, information_json()),
            MockResponse::json(200, input.clone()),
        ],
        "decision_platform_run_input_get",
        json!({ "workspace_id": "routing", "run_id": "run-1" }),
    )
    .await;
    assert_eq!(
        sent(&requests),
        [
            "GET /v1/applications/routing/runs/run-1/metadata",
            "GET /v1/applications/routing/runs/run-1/input",
        ]
    );
    assert_eq!(result, input);
}

#[tokio::test]
async fn serves_the_logs_get_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![
            MockResponse::json(200, information_json()),
            MockResponse::json(200, json!({ "log": "done\n" })),
        ],
        "decision_platform_run_logs_get",
        json!({
            "workspace_id": "routing",
            "run_id": "run-1",
            "polling": { "initial_delay": 0, "jitter": 0 },
        }),
    )
    .await;
    assert_eq!(
        sent(&requests),
        [
            "GET /v1/applications/routing/runs/run-1/metadata",
            "GET /v1/applications/routing/runs/run-1/logs",
        ]
    );
    assert_eq!(result, json!({ "log": "done\n" }));
}

#[tokio::test]
async fn serves_the_list_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(
            200,
            json!({ "runs": [run_json("run-2", "failed"), run_json("run-1", "succeeded")] }),
        )],
        "decision_platform_run_list",
        json!({ "workspace_id": "routing", "status": "succeeded", "limit": 5 }),
    )
    .await;
    assert_eq!(
        sent(&requests),
        ["GET /v1/applications/routing/runs?pagereturn=true"]
    );
    assert_eq!(result.as_array().unwrap().len(), 1);
    assert_eq!(result[0]["id"], "run-1");
    assert_eq!(result[0]["status_v2"], "succeeded");
}

#[tokio::test]
async fn serves_the_list_tool_with_filters() {
    let (_, requests) = support::serve_tool_ok(
        vec![MockResponse::json(200, json!({ "runs": [] }))],
        "decision_platform_run_list",
        json!({
            "workspace_id": "routing",
            "start_time": "2026-10-01T00:00:00Z",
            "include_experiment_runs": true,
        }),
    )
    .await;
    assert_eq!(
        sent(&requests),
        [
            "GET /v1/applications/routing/runs?start=2026-10-01T00%3A00%3A00Z&include_exp_runs=true&pagereturn=true"
        ]
    );
}

#[tokio::test]
async fn rejects_unknown_statuses_in_the_list_tool() {
    let server = MockServer::start(Vec::new()).await;
    let cx = Context::new().with_platform(support::platform(&server));
    for status in ["unknown", "paused"] {
        let args = json!({ "workspace_id": "routing", "status": status });
        let err = support::call_tool(cx.clone(), "decision_platform_run_list", args)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)), "{err}");
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn serves_the_cancel_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::new(204)],
        "decision_platform_run_cancel",
        json!({ "workspace_id": "routing", "run_id": "run-1" }),
    )
    .await;
    assert_eq!(
        sent(&requests),
        ["PATCH /v1/applications/routing/runs/run-1/cancel"]
    );
    assert_eq!(result, Value::Null);
}

#[tokio::test]
async fn serves_the_delete_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::new(204)],
        "decision_platform_run_delete",
        json!({ "workspace_id": "routing", "run_id": "run-1" }),
    )
    .await;
    assert_eq!(
        sent(&requests),
        ["DELETE /v1/applications/routing/runs/run-1"]
    );
    assert_eq!(result, Value::Null);
}

/// The answer of `POST A/runs`.
fn run_created() -> MockResponse {
    MockResponse::json(202, json!({ "run_id": "run-2" }))
}

/// The answer of `POST A/runs/uploadurl`, with an upload URL on `bucket`.
fn upload_created(bucket: &MockServer) -> MockResponse {
    MockResponse::json(
        200,
        json!({
            "upload_id": "0d7ab3fa-6c2d-4b8e-9a51-2f3c4d5e6f70",
            "upload_url": presigned(bucket, "uploads/0d7ab3fa"),
        }),
    )
}

/// The files of the tar.gz `archive`, as (path, contents), in order.
fn untar(archive: &[u8]) -> Vec<(String, String)> {
    let mut archive = tar::Archive::new(GzDecoder::new(archive));
    archive
        .entries()
        .unwrap()
        .map(|entry| {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().into_owned();
            let mut contents = String::new();
            std::io::Read::read_to_string(&mut entry, &mut contents).unwrap();
            (path, contents)
        })
        .collect()
}

/// A JSON configuration with `content_format` for the input and the output.
fn format_json(content_format: &str) -> Value {
    json!({ "input": { "type": content_format }, "output": { "type": content_format } })
}

/// Creates a run that uploads its input first, with `params` in the
/// `routing` workspace, and returns the API's requests and the upload.
async fn create_uploaded_run(params: RunCreateParams) -> (Vec<RecordedRequest>, RecordedRequest) {
    let bucket = MockServer::start(vec![MockResponse::new(200)]).await;
    let server = MockServer::start(vec![upload_created(&bucket), run_created()]).await;
    let created = support::platform(&server)
        .create_run("routing", params)
        .await
        .unwrap();
    assert_eq!(created.run_id, "run-2");
    let requests = server.requests();
    assert_eq!(
        sent(&requests),
        [
            "POST /v1/applications/routing/runs/uploadurl",
            "POST /v1/applications/routing/runs",
        ]
    );
    assert_authorized(&requests);
    let mut uploads = bucket.requests();
    assert_eq!(uploads.len(), 1);
    let upload = uploads.remove(0);
    // The URL is presigned: no token, and no content type.
    assert_eq!(upload.method, "PUT");
    assert_eq!(upload.target, "/uploads/0d7ab3fa?X-Amz-Signature=abc");
    assert_eq!(upload.header("authorization"), None);
    assert_eq!(upload.header("content-type"), None);
    assert_eq!(
        support::body(&requests[1])["upload_id"],
        "0d7ab3fa-6c2d-4b8e-9a51-2f3c4d5e6f70"
    );
    (requests, upload)
}

#[tokio::test]
async fn creates_a_run_with_inline_json_input() {
    let server = MockServer::start(vec![run_created()]).await;
    let params = RunCreateParams {
        input: Some(json!({ "stops": [{ "id": "A" }] })),
        environment_id: Some("production".to_owned()),
        name: Some("Monday".to_owned()),
        description: Some("Weekly plan".to_owned()),
        options: [("solve.duration".to_owned(), "10s".to_owned())].into(),
        ..Default::default()
    };
    let created = support::platform(&server)
        .create_run("routing", params)
        .await
        .unwrap();
    assert_eq!(created.run_id, "run-2");

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(
        requests[0].target,
        "/v1/applications/routing/runs?instance_id=production"
    );
    assert_eq!(requests[0].header("content-type"), Some("application/json"));
    assert_authorized(&requests);
    // A configuration is always sent, with the inferred format.
    assert_eq!(
        support::body(&requests[0]),
        json!({
            "input": { "stops": [{ "id": "A" }] },
            "name": "Monday",
            "description": "Weekly plan",
            "options": { "solve.duration": "10s" },
            "configuration": { "format": format_json("json") },
        })
    );
}

#[tokio::test]
async fn leaves_out_the_environment_when_none_is_set() {
    let server = MockServer::start(vec![run_created()]).await;
    let params = RunCreateParams {
        input: Some(json!({})),
        environment_id: Some(String::new()),
        name: Some(String::new()),
        ..Default::default()
    };
    support::platform(&server)
        .create_run("routing", params)
        .await
        .unwrap();
    let requests = server.requests();
    // The platform then uses the workspace's default environment.
    assert_eq!(requests[0].target, "/v1/applications/routing/runs");
    assert_eq!(
        support::body(&requests[0]),
        json!({
            "input": {},
            "name": "",
            "configuration": { "format": format_json("json") },
        })
    );
}

/// The answer of `GET A`: the workspace `routing`, with `default` as its
/// default environment, if any.
fn workspace(default: Option<&str>) -> MockResponse {
    let mut workspace = json!({ "id": "routing", "name": "Routing" });
    if let Some(default) = default {
        workspace["default_instance"] = default.into();
    }
    MockResponse::json(200, workspace)
}

/// The requests of a run created on `environment_id`, with the answers
/// `first` before the run's.
async fn create_on(environment_id: &str, first: Vec<MockResponse>) -> Vec<String> {
    let mut responses = first;
    responses.push(run_created());
    let server = MockServer::start(responses).await;
    let params = RunCreateParams {
        input: Some(json!({})),
        environment_id: Some(environment_id.to_owned()),
        ..Default::default()
    };
    support::platform(&server)
        .create_run("routing", params)
        .await
        .unwrap();
    sent(&server.requests())
}

#[tokio::test]
async fn runs_default_on_the_workspaces_default_environment() {
    let sent = create_on("default", vec![workspace(Some("production"))]).await;
    assert_eq!(
        sent,
        [
            "GET /v1/applications/routing",
            "POST /v1/applications/routing/runs?instance_id=production",
        ]
    );
}

#[tokio::test]
async fn runs_default_on_the_platforms_fallback_without_a_default_environment() {
    // No environment: the platform's own fallback, its reserved `devint`,
    // since a workspace without a default has usually never been pushed,
    // and has no `latest` yet.
    for default in [None, Some("")] {
        let sent = create_on("default", vec![workspace(default)]).await;
        assert_eq!(
            sent,
            [
                "GET /v1/applications/routing",
                "POST /v1/applications/routing/runs",
            ]
        );
    }
}

#[tokio::test]
async fn sends_another_environment_without_reading_the_workspace() {
    // `latest` too: only `default` is resolved.
    for environment_id in ["staging", "latest", "Default"] {
        let sent = create_on(environment_id, Vec::new()).await;
        assert_eq!(
            sent,
            [format!(
                "POST /v1/applications/routing/runs?instance_id={environment_id}"
            )]
        );
    }
}

#[tokio::test]
async fn stops_when_the_workspace_of_default_cannot_be_read() {
    let server = MockServer::start(vec![MockResponse::json(
        404,
        json!({ "error": "not found" }),
    )])
    .await;
    let params = RunCreateParams {
        input: Some(json!({})),
        environment_id: Some("default".to_owned()),
        ..Default::default()
    };
    let err = support::platform(&server)
        .create_run("routing", params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Status { status: 404, .. }), "{err:?}");
    assert_eq!(sent(&server.requests()), ["GET /v1/applications/routing"]);
}

#[tokio::test]
async fn reads_no_workspace_for_an_ensemble_run_on_default() {
    let server = MockServer::start(vec![run_created()]).await;
    let params = RunCreateParams {
        input: Some(json!({})),
        environment_id: Some("default".to_owned()),
        configuration: Some(RunConfiguration {
            run_type: Some(RunTypeConfiguration {
                definition_id: Some("ensemble-1".to_owned()),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    support::platform(&server)
        .create_run("routing", params)
        .await
        .unwrap();
    // An ensemble run takes no environment, so there is nothing to resolve.
    assert_eq!(
        sent(&server.requests()),
        ["POST /v1/applications/routing/runs"]
    );
}

#[tokio::test]
async fn clones_a_run_on_default() {
    let on = |environment: &str| format!("POST /v1/applications/routing/runs{environment}");
    for (default, expected) in [
        (Some("production"), on("?instance_id=production")),
        (None, on("")),
    ] {
        let server = MockServer::start(vec![
            MockResponse::json(200, cloned_json()),
            workspace(default),
            run_created(),
        ])
        .await;
        let params = RunCloneParams {
            input: Some(json!({})),
            environment_id: Some("default".to_owned()),
            ..Default::default()
        };
        support::platform(&server)
            .clone_run("routing", "run-1", params)
            .await
            .unwrap();
        assert_eq!(
            sent(&server.requests()),
            [
                "GET /v1/applications/routing/runs/run-1/metadata".to_owned(),
                "GET /v1/applications/routing".to_owned(),
                expected,
            ]
        );
    }
}

#[tokio::test]
async fn tracks_a_run_on_default_without_a_default_environment() {
    let params = RunTrackParams {
        tracked_run: TrackedRun {
            input: Some(json!({ "stops": ["a"] })),
            output: Some(json!({ "solutions": [] })),
            ..Default::default()
        },
        environment_id: Some("default".to_owned()),
        ..Default::default()
    };
    let (requests, _) = track_after(params, 2, vec![workspace(None)]).await;
    // No environment: the platform's fallback, which takes a tracked run
    // before the workspace's first push.
    assert_eq!(requests[2].target, "/v1/applications/routing/runs");
}

#[tokio::test]
async fn uploads_large_input_first() {
    let large = "x".repeat(LARGE as usize);
    let params = RunCreateParams {
        input: Some(json!({ "blob": large })),
        ..Default::default()
    };
    let (requests, upload) = create_uploaded_run(params).await;
    assert_eq!(
        serde_json::from_slice::<Value>(&upload.body).unwrap(),
        json!({ "blob": large })
    );
    assert_eq!(
        support::body(&requests[1]),
        json!({
            "upload_id": "0d7ab3fa-6c2d-4b8e-9a51-2f3c4d5e6f70",
            "configuration": { "format": format_json("json") },
        })
    );
}

#[tokio::test]
async fn uploads_an_input_dir_as_a_tar_gz() {
    let dir = TempDir::new("run");
    fs::write(dir.path().join("orders.csv"), "id\n1\n").unwrap();
    fs::create_dir(dir.path().join("depots")).unwrap();
    fs::write(dir.path().join("depots/north.json"), "{}").unwrap();
    let params = RunCreateParams {
        input_dir: Some(dir.path().to_owned()),
        environment_id: Some("production".to_owned()),
        ..Default::default()
    };
    let (requests, upload) = create_uploaded_run_with_query(params).await;
    assert_eq!(
        untar(&upload.body),
        [
            ("depots/north.json".to_owned(), "{}".to_owned()),
            ("orders.csv".to_owned(), "id\n1\n".to_owned()),
        ]
    );
    assert_eq!(
        support::body(&requests[1])["configuration"],
        json!({ "format": format_json("multi-file") })
    );
}

/// As [`create_uploaded_run`], for a run in the `production` environment.
async fn create_uploaded_run_with_query(
    params: RunCreateParams,
) -> (Vec<RecordedRequest>, RecordedRequest) {
    let bucket = MockServer::start(vec![MockResponse::new(200)]).await;
    let server = MockServer::start(vec![upload_created(&bucket), run_created()]).await;
    support::platform(&server)
        .create_run("routing", params)
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(
        sent(&requests),
        [
            "POST /v1/applications/routing/runs/uploadurl",
            "POST /v1/applications/routing/runs?instance_id=production",
        ]
    );
    let upload = bucket.requests().remove(0);
    assert_eq!(upload.header("authorization"), None);
    (requests, upload)
}

#[tokio::test]
async fn uploads_a_json_input_file_as_it_is() {
    let dir = TempDir::new("run");
    let file = dir.path().join("input.json");
    // Not compact, so a rewrite would show.
    fs::write(&file, "{ \"stops\": [] }\n").unwrap();
    let params = RunCreateParams {
        input_file: Some(file),
        ..Default::default()
    };
    let (requests, upload) = create_uploaded_run(params).await;
    assert_eq!(upload.body, b"{ \"stops\": [] }\n");
    assert_eq!(
        support::body(&requests[1])["configuration"],
        json!({ "format": format_json("json") })
    );
}

#[tokio::test]
async fn uploads_a_tar_gz_input_file_as_it_is() {
    let dir = TempDir::new("run");
    let file = dir.path().join("input.tar.gz");
    let archive = tar_gz(&[("orders.csv", "id\n1\n")]);
    fs::write(&file, &archive).unwrap();
    let params = RunCreateParams {
        input_file: Some(file),
        ..Default::default()
    };
    let (requests, upload) = create_uploaded_run(params).await;
    assert_eq!(upload.body, archive);
    assert_eq!(
        support::body(&requests[1])["configuration"],
        json!({ "format": format_json("multi-file") })
    );
}

#[tokio::test]
async fn rejects_an_input_file_that_is_neither_json_nor_a_tar_gz() {
    let server = MockServer::start(Vec::new()).await;
    let dir = TempDir::new("run");
    let file = dir.path().join("input.txt");
    fs::write(&file, "plain text").unwrap();
    let params = RunCreateParams {
        input_file: Some(file),
        ..Default::default()
    };
    let err = support::platform(&server)
        .create_run("routing", params)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Invalid { reason, .. } if reason.contains("input_file")),
        "{err}"
    );
    let params = RunCreateParams {
        input_file: Some(dir.path().join("missing.json")),
        ..Default::default()
    };
    let err = support::platform(&server)
        .create_run("routing", params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Io { .. }), "{err}");
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn rejects_an_input_dir_that_is_not_a_directory() {
    let server = MockServer::start(Vec::new()).await;
    let dir = TempDir::new("run");
    let file = dir.path().join("input.json");
    fs::write(&file, "{}").unwrap();
    let params = RunCreateParams {
        input_dir: Some(file),
        ..Default::default()
    };
    let err = support::platform(&server)
        .create_run("routing", params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn runs_on_a_managed_input_in_its_format() {
    let managed_input = json!({
        "id": "monday",
        "name": "Monday",
        "app_id": "routing",
        "upload_id": "0d7ab3fa-6c2d-4b8e-9a51-2f3c4d5e6f70",
        "format": format_json("multi-file"),
        "created_at": "2026-10-01T12:00:00Z",
        "updated_at": "2026-10-01T12:00:00Z",
    });
    let server =
        MockServer::start(vec![MockResponse::json(200, managed_input), run_created()]).await;
    let params = RunCreateParams {
        managed_input_id: Some("monday".to_owned()),
        environment_id: Some("production".to_owned()),
        ..Default::default()
    };
    support::platform(&server)
        .create_run("routing", params)
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(
        sent(&requests),
        [
            "GET /v1/applications/routing/inputs/monday",
            "POST /v1/applications/routing/runs?instance_id=production",
        ]
    );
    assert_authorized(&requests);
    assert_eq!(
        support::body(&requests[1]),
        json!({
            "input_id": "monday",
            "configuration": { "format": format_json("multi-file") },
        })
    );
}

#[tokio::test]
async fn keeps_the_format_that_the_configuration_sets() {
    let server = MockServer::start(vec![run_created()]).await;
    let params = RunCreateParams {
        managed_input_id: Some("monday".to_owned()),
        configuration: Some(RunConfiguration {
            format: Some(Format::from(ContentFormat::MultiFile)),
            ..Default::default()
        }),
        ..Default::default()
    };
    support::platform(&server)
        .create_run("routing", params)
        .await
        .unwrap();
    let requests = server.requests();
    // No need to read the managed input.
    assert_eq!(sent(&requests), ["POST /v1/applications/routing/runs"]);
    assert_eq!(
        support::body(&requests[0]),
        json!({
            "input_id": "monday",
            "configuration": { "format": { "input": { "type": "multi-file" } } },
        })
    );
}

#[tokio::test]
async fn runs_on_an_upload() {
    let server = MockServer::start(vec![run_created()]).await;
    let params = RunCreateParams {
        upload_id: Some("0d7ab3fa-6c2d-4b8e-9a51-2f3c4d5e6f70".to_owned()),
        configuration: Some(RunConfiguration {
            execution_class: Some("6c9500mb870s".to_owned()),
            secrets_collection_id: Some("secrets-1".to_owned()),
            queuing: Some(RunQueuing {
                priority: Some(3),
                disabled: Some(true),
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    support::platform(&server)
        .create_run("routing", params)
        .await
        .unwrap();
    assert_eq!(
        support::body(&server.requests()[0]),
        json!({
            "upload_id": "0d7ab3fa-6c2d-4b8e-9a51-2f3c4d5e6f70",
            // No format: the platform takes the build's, and rejects one
            // that differs from it.
            "configuration": {
                "execution_class": "6c9500mb870s",
                "secrets_collection_id": "secrets-1",
                "queuing": { "priority": 3, "disabled": true },
            },
        })
    );
}

#[tokio::test]
async fn leaves_out_the_environment_of_an_ensemble_run() {
    for run_type in [Some(RunType::Ensemble), None] {
        let server = MockServer::start(vec![run_created()]).await;
        let params = RunCreateParams {
            input: Some(json!({})),
            environment_id: Some("production".to_owned()),
            configuration: Some(RunConfiguration {
                run_type: Some(RunTypeConfiguration {
                    run_type,
                    definition_id: Some("fleet".to_owned()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        support::platform(&server)
            .create_run("routing", params)
            .await
            .unwrap();
        let requests = server.requests();
        assert_eq!(
            requests[0].target, "/v1/applications/routing/runs",
            "{run_type:?}"
        );
        // A definition ID alone makes an ensemble run.
        assert_eq!(
            support::body(&requests[0])["configuration"],
            json!({
                "format": format_json("json"),
                "run_type": { "type": "ensemble", "definition_id": "fleet" },
            })
        );
    }
}

#[tokio::test]
async fn sets_the_integration_execution_class() {
    for execution_class in [None, Some(""), Some("integration")] {
        let server = MockServer::start(vec![run_created()]).await;
        let params = RunCreateParams {
            input: Some(json!({})),
            configuration: Some(RunConfiguration {
                execution_class: execution_class.map(str::to_owned),
                integration_id: Some("dbx".to_owned()),
                ..Default::default()
            }),
            ..Default::default()
        };
        support::platform(&server)
            .create_run("routing", params)
            .await
            .unwrap();
        let configuration = &support::body(&server.requests()[0])["configuration"];
        assert_eq!(configuration["execution_class"], "integration");
        assert_eq!(configuration["integration_id"], "dbx");
    }
}

#[tokio::test]
async fn rejects_invalid_configurations() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    let run_type = |run_type, definition_id: Option<&str>| RunConfiguration {
        run_type: Some(RunTypeConfiguration {
            run_type,
            definition_id: definition_id.map(str::to_owned),
            ..Default::default()
        }),
        ..Default::default()
    };
    let priority = |priority| RunConfiguration {
        queuing: Some(RunQueuing {
            priority: Some(priority),
            ..Default::default()
        }),
        ..Default::default()
    };
    for (configuration, field) in [
        (priority(0), "priority"),
        (priority(10), "priority"),
        (
            RunConfiguration {
                execution_class: Some("6c9500mb870s".to_owned()),
                integration_id: Some("dbx".to_owned()),
                ..Default::default()
            },
            "integration_id",
        ),
        (run_type(Some(RunType::Ensemble), None), "definition_id"),
        (run_type(Some(RunType::Ensemble), Some("")), "definition_id"),
        (
            run_type(Some(RunType::Standard), Some("fleet")),
            "definition_id",
        ),
        (run_type(Some(RunType::Unknown), None), "run_type"),
        (
            RunConfiguration {
                format: Some(Format {
                    format_input: FormatInput {
                        input_type: ContentFormat::Unknown,
                    },
                    format_output: None,
                }),
                ..Default::default()
            },
            "format",
        ),
    ] {
        let params = RunCreateParams {
            input: Some(json!({})),
            configuration: Some(configuration.clone()),
            ..Default::default()
        };
        let err = platform.create_run("routing", params).await.unwrap_err();
        assert!(
            matches!(&err, Error::Invalid { reason, .. } if reason.contains(field)),
            "{configuration:?}: {err}"
        );
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn needs_exactly_one_input() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    let dir = TempDir::new("run");
    for params in [
        RunCreateParams::default(),
        // Empty values are unset.
        RunCreateParams {
            input_dir: Some(PathBuf::new()),
            managed_input_id: Some(String::new()),
            upload_id: Some(String::new()),
            ..Default::default()
        },
        RunCreateParams {
            input: Some(json!({})),
            upload_id: Some("upload-1".to_owned()),
            ..Default::default()
        },
        RunCreateParams {
            input_dir: Some(dir.path().to_owned()),
            managed_input_id: Some("monday".to_owned()),
            ..Default::default()
        },
    ] {
        let err = platform.create_run("routing", params).await.unwrap_err();
        assert!(
            matches!(&err, Error::Invalid { reason, .. } if reason.contains("exactly one")),
            "{err}"
        );
    }
    for input in [json!([1, 2]), json!("text"), json!(null)] {
        let params = RunCreateParams {
            input: Some(input),
            ..Default::default()
        };
        let err = platform.create_run("routing", params).await.unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn serves_the_create_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![run_created()],
        "decision_platform_run_create",
        json!({
            "workspace_id": "routing",
            "input": { "stops": [] },
            "environment_id": "production",
            "options": { "solve.duration": "10s" },
            "configuration": { "queuing": { "priority": 1 } },
        }),
    )
    .await;
    assert_eq!(result, json!({ "run_id": "run-2" }));
    assert_eq!(
        sent(&requests),
        ["POST /v1/applications/routing/runs?instance_id=production"]
    );
    assert_eq!(
        support::body(&requests[0]),
        json!({
            "input": { "stops": [] },
            "options": { "solve.duration": "10s" },
            "configuration": {
                "format": format_json("json"),
                "queuing": { "priority": 1 },
            },
        })
    );
}

#[tokio::test]
async fn serves_the_create_tool_with_input_dir_in_local_mode() {
    let dir = TempDir::new("run");
    let input_dir = dir.path().join("input");
    fs::create_dir(&input_dir).unwrap();
    fs::write(input_dir.join("orders.csv"), "id\n").unwrap();
    let bucket = MockServer::start(vec![MockResponse::new(200)]).await;
    let server = MockServer::start(vec![upload_created(&bucket), run_created()]).await;
    let cx = Context::new()
        .with_platform(support::platform(&server))
        .with_local(Local::with_home(dir.path().join(".fico")));
    let args = json!({ "workspace_id": "routing", "input_dir": input_dir });
    let result = support::call_tool(cx, "decision_platform_run_create", args)
        .await
        .unwrap();
    assert_eq!(result, json!({ "run_id": "run-2" }));
    assert_eq!(server.requests().len(), 2);
    assert_eq!(
        untar(&bucket.requests()[0].body),
        [("orders.csv".to_owned(), "id\n".to_owned())]
    );
}

#[tokio::test]
async fn refuses_input_paths_without_local_mode() {
    let server = MockServer::start(Vec::new()).await;
    let remote = Context::new().with_platform(support::platform(&server));
    for field in ["input_dir", "input_file"] {
        let args = json!({ "workspace_id": "routing", field: "/srv/data" });
        let err = support::call_tool(remote.clone(), "decision_platform_run_create", args)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::Failed(Error::Invalid { .. })),
            "{field}: {err}"
        );
    }
    assert!(server.requests().is_empty());
}

/// The information of the run `run_id` named `name`, with `metrics` and the
/// active options `options`.
fn compared_information(run_id: &str, name: &str, metrics: Value, options: Value) -> Value {
    let mut information = information_json();
    information["id"] = json!(run_id);
    information["name"] = json!(name);
    information["console_url"] = json!(format!("https://console.example.com/{run_id}"));
    information["metadata"]["metrics"] = metrics;
    information["metadata"]["options"] = json!({ "active_options": options });
    information["metadata"]["tracking"] = json!({ "cloned_run_id": "run-0", "input_id": "" });
    information
}

/// The parameters to compare the runs `run_ids`.
fn compared(run_ids: &[&str]) -> RunCompareParams {
    RunCompareParams {
        run_ids: run_ids.iter().map(|&run_id| run_id.to_owned()).collect(),
    }
}

#[tokio::test]
async fn compares_runs_field_by_field() {
    let first = compared_information(
        "run-1",
        "Monday",
        // Wrapped, as JSON output carries them.
        json!({ "metrics": { "cost": 12.3456789, "vehicles": 3 } }),
        json!({ "solve.duration": "10s" }),
    );
    let mut second = compared_information(
        "run-2",
        "Tuesday",
        json!({ "cost": 12.345681, "vehicles": 3.0 }),
        json!({ "solve.duration": "30s" }),
    );
    second["metadata"]["status_v2"] = json!("failed");
    let server = MockServer::start(vec![
        MockResponse::json(200, first),
        MockResponse::json(200, second),
    ])
    .await;
    let comparison = support::platform(&server)
        .compare_runs("routing", compared(&["run-1", "run-2"]))
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(
        sent(&requests),
        [
            "GET /v1/applications/routing/runs/run-1/metadata",
            "GET /v1/applications/routing/runs/run-2/metadata",
        ]
    );
    assert_authorized(&requests);
    let emitted = serde_json::to_value(&comparison).unwrap();
    assert_eq!(emitted["run_ids"], json!(["run-1", "run-2"]));
    let information = &emitted["information"];
    assert_eq!(
        information["name"],
        json!({ "has_differences": true, "values": { "run-1": "Monday", "run-2": "Tuesday" } })
    );
    assert_eq!(information["user_email"]["has_differences"], false);
    assert!(information.get("metadata").is_none() && information.get("console_url").is_none());
    let metadata = &emitted["metadata"];
    assert_eq!(
        metadata["status"],
        json!({ "has_differences": true, "values": { "run-1": "succeeded", "run-2": "failed" } })
    );
    assert_eq!(metadata["content_format"]["values"]["run-1"], "json");
    assert_eq!(metadata["run_type"]["values"]["run-2"], "standard");
    assert_eq!(metadata["cloned_run_id"]["values"]["run-1"], "run-0");
    assert_eq!(
        metadata["application_instance_id"]["has_differences"],
        false
    );
    for flattened in ["status_v2", "format", "tracking", "metrics", "options"] {
        assert!(metadata.get(flattened).is_none(), "{flattened}");
    }
    // Rounded to 5 digits, unwrapped, and numbers compared by value.
    assert_eq!(
        emitted["metrics"],
        json!({
            "cost": { "has_differences": false, "values": { "run-1": 12.34568, "run-2": 12.34568 } },
            "vehicles": { "has_differences": false, "values": { "run-1": 3, "run-2": 3.0 } },
        })
    );
    assert_eq!(
        emitted["options"]["solve.duration"],
        json!({ "has_differences": true, "values": { "run-1": "10s", "run-2": "30s" } })
    );
}

#[tokio::test]
async fn compares_runs_without_metrics_or_options() {
    let server = MockServer::start(vec![MockResponse::json(200, information_json())]).await;
    let comparison = support::platform(&server)
        .compare_runs("routing", compared(&["run-1"]))
        .await
        .unwrap();
    assert_eq!(comparison.run_ids, ["run-1"]);
    assert_eq!(comparison.metrics, None);
    assert_eq!(comparison.options, None);
    assert!(!comparison.metadata["status"].has_differences);
}

#[tokio::test]
async fn checks_the_runs_to_compare() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    let many: Vec<String> = (0..21).map(|n| format!("run-{n}")).collect();
    for params in [
        compared(&[]),
        RunCompareParams { run_ids: many },
        compared(&["run-1", "run-2", "run-1"]),
    ] {
        let err = platform.compare_runs("routing", params).await.unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn serves_the_compare_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![
            MockResponse::json(200, information_json()),
            MockResponse::json(200, with_status(information_json(), "failed")),
        ],
        "decision_platform_run_compare",
        json!({ "workspace_id": "routing", "run_ids": ["run-1", "run-2"] }),
    )
    .await;
    assert_eq!(requests.len(), 2);
    assert_eq!(result["run_ids"], json!(["run-1", "run-2"]));
    assert_eq!(result["metadata"]["status"]["has_differences"], true);
    // `run_ids` is required.
    let server = MockServer::start(Vec::new()).await;
    let cx = Context::new().with_platform(support::platform(&server));
    let err = support::call_tool(
        cx,
        "decision_platform_run_compare",
        json!({ "workspace_id": "routing" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArguments(_)), "{err}");
}

/// The information of a run to clone: `information_json` with options,
/// queuing, secrets and its environment's execution class.
fn cloned_json() -> Value {
    let mut information = information_json();
    let metadata = &mut information["metadata"];
    metadata["options"] = json!({
        "active_options": { "solve.duration": "10s", "solve.iterations": "50" },
        "request_options": { "solve.duration": "10s" },
        "options_summary": [
            { "name": "solve.duration", "source": "run", "value": "10s" },
            { "name": "solve.iterations", "source": "environment", "value": "50" }
        ],
    });
    metadata["queuing_priority"] = json!(3);
    metadata["queuing_disabled"] = json!(false);
    metadata["secrets_collection_id"] = json!("secrets-1");
    metadata["tracking"] = json!({ "cloned_run_id": "", "input_id": "" });
    information
}

#[tokio::test]
async fn clones_a_run_with_its_input_and_configuration() {
    let input = json!({ "stops": [{ "id": "A" }] });
    let server = MockServer::start(vec![
        MockResponse::json(200, cloned_json()),
        MockResponse::json(200, input.clone()),
        run_created(),
    ])
    .await;
    let created = support::platform(&server)
        .clone_run("routing", "run-1", RunCloneParams::default())
        .await
        .unwrap();
    assert_eq!(created.run_id, "run-2");
    let requests = server.requests();
    assert_eq!(
        sent(&requests),
        [
            "GET /v1/applications/routing/runs/run-1/metadata",
            "GET /v1/applications/routing/runs/run-1/input",
            "POST /v1/applications/routing/runs?instance_id=production",
        ]
    );
    assert_authorized(&requests);
    assert_eq!(
        support::body(&requests[2]),
        json!({
            "input": input,
            "name": "Monday clone",
            "description": "Clone of Weekly plan",
            "options": { "solve.duration": "10s", "solve.iterations": "50" },
            "configuration": {
                "execution_class": "6c9500mb870s",
                "format": format_json("json"),
                "run_type": { "type": "standard" },
                "secrets_collection_id": "secrets-1",
                "queuing": { "priority": 3, "disabled": false },
            },
            "tracking": { "cloned_run_id": "run-1" },
        })
    );
}

#[tokio::test]
async fn clones_a_run_with_what_params_set() {
    let server =
        MockServer::start(vec![MockResponse::json(200, cloned_json()), run_created()]).await;
    let params = RunCloneParams {
        input: Some(json!({ "stops": [] })),
        environment_id: Some("staging".to_owned()),
        name: Some("Retry".to_owned()),
        description: Some("With no stops".to_owned()),
        options: [("solve.duration".to_owned(), "30s".to_owned())].into(),
        configuration: Some(RunConfiguration {
            execution_class: Some("8c16gb12h".to_owned()),
            // Only the queue setting: the priority stays the cloned run's.
            queuing: Some(RunQueuing {
                disabled: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    support::platform(&server)
        .clone_run("routing", "run-1", params)
        .await
        .unwrap();
    let requests = server.requests();
    // A new input: the cloned run's is not read.
    assert_eq!(
        sent(&requests),
        [
            "GET /v1/applications/routing/runs/run-1/metadata",
            "POST /v1/applications/routing/runs?instance_id=staging",
        ]
    );
    assert_eq!(
        support::body(&requests[1]),
        json!({
            "input": { "stops": [] },
            "name": "Retry",
            "description": "With no stops",
            "options": { "solve.duration": "30s" },
            "configuration": {
                "execution_class": "8c16gb12h",
                "format": format_json("json"),
                "run_type": { "type": "standard" },
                "secrets_collection_id": "secrets-1",
                "queuing": { "priority": 3, "disabled": true },
            },
            "tracking": { "cloned_run_id": "run-1" },
        })
    );
}

#[tokio::test]
async fn clones_a_run_on_its_managed_input() {
    let mut cloned = cloned_json();
    cloned["metadata"]["tracking"]["input_id"] = json!("monday");
    let server = MockServer::start(vec![MockResponse::json(200, cloned), run_created()]).await;
    support::platform(&server)
        .clone_run("routing", "run-1", RunCloneParams::default())
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "{:?}", sent(&requests));
    let sent_body = support::body(&requests[1]);
    assert_eq!(sent_body["input_id"], "monday");
    assert!(sent_body.get("input").is_none() && sent_body.get("upload_id").is_none());
    // The managed input's format is the cloned run's.
    assert_eq!(sent_body["configuration"]["format"], format_json("json"));
}

#[tokio::test]
async fn replaces_the_managed_input_of_the_cloned_run() {
    let mut cloned = cloned_json();
    cloned["metadata"]["tracking"]["input_id"] = json!("monday");
    let server = MockServer::start(vec![MockResponse::json(200, cloned), run_created()]).await;
    let params = RunCloneParams {
        upload_id: Some("upload-9".to_owned()),
        ..Default::default()
    };
    support::platform(&server)
        .clone_run("routing", "run-1", params)
        .await
        .unwrap();
    let requests = server.requests();
    // An upload ID: neither the cloned run's input nor its managed input.
    assert_eq!(requests.len(), 2, "{:?}", sent(&requests));
    let sent_body = support::body(&requests[1]);
    assert_eq!(sent_body["upload_id"], "upload-9");
    assert!(sent_body.get("input_id").is_none());
    // The cloned run's format, since the SDK does not see an upload.
    assert_eq!(sent_body["configuration"]["format"], format_json("json"));
}

/// Clones the run `cloned`, whose input the bucket serves as `content`
/// from a presigned URL, and returns the API's requests and the upload.
async fn clone_downloaded_input(
    cloned: Value,
    content: Vec<u8>,
) -> (Vec<RecordedRequest>, RecordedRequest) {
    let bucket = MockServer::start(vec![MockResponse::new(200).body(content)]).await;
    let uploads = MockServer::start(vec![MockResponse::new(200)]).await;
    let url = presigned(&bucket, "input/run-1-input");
    let server = MockServer::start(vec![
        MockResponse::json(200, cloned),
        MockResponse::json(200, json!({ "url": url })),
        upload_created(&uploads),
        run_created(),
    ])
    .await;
    support::platform(&server)
        .clone_run("routing", "run-1", RunCloneParams::default())
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(
        sent(&requests),
        [
            "GET /v1/applications/routing/runs/run-1/metadata",
            "GET /v1/applications/routing/runs/run-1/input?format=url",
            "POST /v1/applications/routing/runs/uploadurl",
            "POST /v1/applications/routing/runs?instance_id=production",
        ]
    );
    assert_authorized(&requests);
    let download = &bucket.requests()[0];
    assert_eq!(download.header("authorization"), None);
    let mut uploads = uploads.requests();
    assert_eq!(uploads.len(), 1);
    let upload = uploads.remove(0);
    assert_eq!(upload.method, "PUT");
    assert_eq!(upload.header("authorization"), None);
    assert_eq!(
        support::body(&requests[3])["upload_id"],
        "0d7ab3fa-6c2d-4b8e-9a51-2f3c4d5e6f70"
    );
    (requests, upload)
}

#[tokio::test]
async fn uploads_the_multi_file_input_of_the_cloned_run_again() {
    let archive = tar_gz(&[("stops.csv", "id\na\n")]);
    let mut cloned = cloned_json();
    cloned["metadata"]["format"] = format_json("multi-file");
    let (requests, upload) = clone_downloaded_input(cloned, archive.clone()).await;
    // The archive, as it is.
    assert_eq!(upload.body, archive);
    assert_eq!(
        support::body(&requests[3])["configuration"]["format"],
        format_json("multi-file")
    );
}

#[tokio::test]
async fn uploads_the_large_json_input_of_the_cloned_run_again() {
    let input = json!({ "blob": "x".repeat(LARGE as usize) }).to_string();
    let mut cloned = cloned_json();
    cloned["metadata"]["input_size"] = json!(LARGE);
    let (requests, upload) = clone_downloaded_input(cloned, input.clone().into_bytes()).await;
    assert_eq!(upload.body, input.into_bytes());
    assert_eq!(
        support::body(&requests[3])["configuration"]["format"],
        format_json("json")
    );
}

#[tokio::test]
async fn takes_the_format_of_a_new_input_in_a_clone() {
    // A JSON run, cloned with a directory of input files.
    let dir = TempDir::new("run");
    fs::write(dir.path().join("stops.csv"), "id\na\n").unwrap();
    let uploads = MockServer::start(vec![MockResponse::new(200)]).await;
    let server = MockServer::start(vec![
        MockResponse::json(200, cloned_json()),
        upload_created(&uploads),
        run_created(),
    ])
    .await;
    let params = RunCloneParams {
        input_dir: Some(dir.path().to_owned()),
        ..Default::default()
    };
    support::platform(&server)
        .clone_run("routing", "run-1", params)
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(requests.len(), 3, "{:?}", sent(&requests));
    assert_eq!(
        support::body(&requests[2])["configuration"]["format"],
        format_json("multi-file")
    );

    // A JSON run, cloned on a multi-file managed input, whose format is
    // read first.
    let managed_input = json!({
        "id": "monday",
        "name": "Monday",
        "app_id": "routing",
        "format": format_json("multi-file"),
        "created_at": "2026-10-01T12:00:00Z",
        "updated_at": "2026-10-01T12:00:00Z",
    });
    let server = MockServer::start(vec![
        MockResponse::json(200, cloned_json()),
        MockResponse::json(200, managed_input),
        run_created(),
    ])
    .await;
    let params = RunCloneParams {
        managed_input_id: Some("monday".to_owned()),
        ..Default::default()
    };
    support::platform(&server)
        .clone_run("routing", "run-1", params)
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(
        sent(&requests),
        [
            "GET /v1/applications/routing/runs/run-1/metadata",
            "GET /v1/applications/routing/inputs/monday",
            "POST /v1/applications/routing/runs?instance_id=production",
        ]
    );
    let sent_body = support::body(&requests[2]);
    assert_eq!(sent_body["input_id"], "monday");
    assert_eq!(
        sent_body["configuration"]["format"],
        format_json("multi-file")
    );
}

#[tokio::test]
async fn clones_into_an_ensemble_run() {
    let server = MockServer::start(vec![
        MockResponse::json(200, cloned_json()),
        MockResponse::json(200, json!({ "stops": [] })),
        run_created(),
    ])
    .await;
    let params = RunCloneParams {
        environment_id: Some("staging".to_owned()),
        configuration: Some(RunConfiguration {
            // A definition with no run type is an ensemble.
            run_type: Some(RunTypeConfiguration {
                definition_id: Some("fleet".to_owned()),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    support::platform(&server)
        .clone_run("routing", "run-1", params)
        .await
        .unwrap();
    let requests = server.requests();
    // Ensemble runs take no environment.
    assert_eq!(requests[2].target, "/v1/applications/routing/runs");
    assert_eq!(
        support::body(&requests[2])["configuration"]["run_type"],
        json!({ "type": "ensemble", "definition_id": "fleet" })
    );
}

#[tokio::test]
async fn clones_a_tracked_run_as_a_standard_run() {
    let mut cloned = cloned_json();
    let metadata = &mut cloned["metadata"];
    metadata["run_type"] = json!({ "type": "external", "definition_id": "", "reference_id": "" });
    metadata["execution_class"] = json!("external");
    metadata["queuing_priority"] = json!(0);
    metadata["integration"] = json!({ "id": "dbx", "provider": "databricks", "run_id": "7" });
    let server = MockServer::start(vec![
        MockResponse::json(200, cloned),
        MockResponse::json(200, json!({ "stops": [] })),
        run_created(),
    ])
    .await;
    support::platform(&server)
        .clone_run("routing", "run-1", RunCloneParams::default())
        .await
        .unwrap();
    let configuration = &support::body(&server.requests()[2])["configuration"];
    // The platform's run type, execution class and priority 0 are not
    // kept, so the environment's apply. The integration is, with the
    // execution class it needs.
    assert_eq!(
        configuration,
        &json!({
            "execution_class": "integration",
            "format": format_json("json"),
            "secrets_collection_id": "secrets-1",
            "queuing": { "disabled": false },
            "integration_id": "dbx",
        })
    );
}

#[tokio::test]
async fn checks_a_clone_before_any_request() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    let dir = TempDir::new("run");
    for params in [
        RunCloneParams {
            input: Some(json!({})),
            managed_input_id: Some("monday".to_owned()),
            ..Default::default()
        },
        RunCloneParams {
            input_dir: Some(dir.path().to_owned()),
            upload_id: Some("upload-1".to_owned()),
            ..Default::default()
        },
        RunCloneParams {
            input: Some(json!([1])),
            ..Default::default()
        },
        RunCloneParams {
            configuration: Some(RunConfiguration {
                queuing: Some(RunQueuing {
                    priority: Some(10),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
    ] {
        let err = platform
            .clone_run("routing", "run-1", params)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn serves_the_clone_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(200, cloned_json()), run_created()],
        "decision_platform_run_clone",
        json!({
            "workspace_id": "routing",
            "run_id": "run-1",
            "input": { "stops": [] },
            "configuration": { "queuing": { "priority": 1 } },
        }),
    )
    .await;
    assert_eq!(result, json!({ "run_id": "run-2" }));
    assert_eq!(requests.len(), 2);
    let configuration = &support::body(&requests[1])["configuration"];
    assert_eq!(
        configuration["queuing"],
        json!({ "priority": 1, "disabled": false })
    );
}

#[tokio::test]
async fn refuses_clone_input_paths_without_local_mode() {
    let server = MockServer::start(Vec::new()).await;
    let remote = Context::new().with_platform(support::platform(&server));
    for field in ["input_dir", "input_file"] {
        let args = json!({ "workspace_id": "routing", "run_id": "run-1", field: "/srv/data" });
        let err = support::call_tool(remote.clone(), "decision_platform_run_clone", args)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::Failed(Error::Invalid { .. })),
            "{field}: {err}"
        );
    }
    assert!(server.requests().is_empty());
}

/// The answers of `POST A/runs/uploadurl` for `count` uploads to `bucket`:
/// `upload-0`, `upload-1`, …, each at its own key.
fn upload_urls(bucket: &MockServer, count: usize) -> Vec<MockResponse> {
    let url = |n| {
        MockResponse::json(
            200,
            json!({ "upload_id": format!("upload-{n}"), "upload_url": presigned(bucket, &format!("uploads/{n}")) }),
        )
    };
    (0..count).map(url).collect()
}

/// Tracks the run `params` sets in the `routing` workspace, with `count`
/// uploads, and returns the API's requests and the uploads' bodies, in
/// order.
async fn track(params: RunTrackParams, count: usize) -> (Vec<RecordedRequest>, Vec<Vec<u8>>) {
    track_after(params, count, Vec::new()).await
}

/// As [`track`], with `first`, the answers to the requests before the
/// uploads, such as the workspace's for `default`. Their requests are not
/// returned.
async fn track_after(
    params: RunTrackParams,
    count: usize,
    first: Vec<MockResponse>,
) -> (Vec<RecordedRequest>, Vec<Vec<u8>>) {
    let skip = first.len();
    let bucket = MockServer::start(vec![MockResponse::new(200); count]).await;
    let mut responses = first;
    responses.extend(upload_urls(&bucket, count));
    responses.push(run_created());
    let server = MockServer::start(responses).await;
    let created = support::platform(&server)
        .track_run("routing", params)
        .await
        .unwrap();
    assert_eq!(created.run_id, "run-2");
    let requests = server.requests().split_off(skip);
    assert_eq!(requests.len(), count + 1, "{:?}", sent(&requests));
    let upload_urls = &requests[..count];
    assert!(
        upload_urls
            .iter()
            .all(|r| r.method == "POST" && r.target == "/v1/applications/routing/runs/uploadurl")
    );
    assert_authorized(&requests);
    let uploads = bucket.requests();
    assert_eq!(uploads.len(), count);
    for (n, upload) in uploads.iter().enumerate() {
        assert_eq!(upload.method, "PUT");
        assert_eq!(upload.target, format!("/uploads/{n}?X-Amz-Signature=abc"));
        assert_eq!(upload.header("authorization"), None);
    }
    let bodies = uploads.into_iter().map(|upload| upload.body).collect();
    (requests, bodies)
}

#[tokio::test]
async fn tracks_a_json_run() {
    let params = RunTrackParams {
        tracked_run: TrackedRun {
            status: TrackedRunStatus::Failed,
            input: Some(json!({ "stops": ["a"] })),
            output: Some(json!({ "solutions": [], "metrics": { "cost": 1 } })),
            duration: Some(5_000),
            error: Some("infeasible".to_owned()),
            logs: Some("solving\nno solution\n".to_owned()),
            name: Some("Monday".to_owned()),
            description: Some("Ran on my laptop".to_owned()),
            ..Default::default()
        },
        environment_id: Some("production".to_owned()),
        ..Default::default()
    };
    let (requests, uploads) = track(params, 3).await;
    let json = |bytes: &[u8]| serde_json::from_slice::<Value>(bytes).unwrap();
    assert_eq!(json(&uploads[0]), json!({ "stops": ["a"] }));
    assert_eq!(
        json(&uploads[1]),
        json!({ "solutions": [], "metrics": { "cost": 1 } })
    );
    assert_eq!(uploads[2], b"solving\nno solution\n");
    let created = &requests[3];
    assert_eq!(
        created.target,
        "/v1/applications/routing/runs?instance_id=production"
    );
    assert_eq!(
        support::body(created),
        json!({
            "upload_id": "upload-0",
            "name": "Monday",
            "description": "Ran on my laptop",
            "configuration": {
                "format": format_json("json"),
                "run_type": { "type": "external" },
            },
            "result": {
                "output_upload_id": "upload-1",
                "error_upload_id": "upload-2",
                "status": "failed",
                "error_message": "infeasible",
                "execution_duration": 5000,
            },
        })
    );
}

#[tokio::test]
async fn tracks_a_multi_file_run_with_metrics_and_assets() {
    let dir = TempDir::new("run");
    let (inputs, outputs) = (dir.path().join("inputs"), dir.path().join("outputs"));
    fs::create_dir_all(&inputs).unwrap();
    fs::create_dir_all(&outputs).unwrap();
    fs::write(inputs.join("stops.csv"), "id\na\n").unwrap();
    fs::write(outputs.join("routes.csv"), "vehicle,stop\n").unwrap();
    let params = RunTrackParams {
        tracked_run: TrackedRun {
            input_dir_path: Some(inputs),
            output_dir_path: Some(outputs),
            metrics: Some(json!({ "cost": 12.5 }).as_object().unwrap().clone()),
            assets: Some(vec![json!({ "name": "map", "content_type": "json" })]),
            ..Default::default()
        },
        ..Default::default()
    };
    let (requests, uploads) = track(params, 4).await;
    assert_eq!(
        untar(&uploads[0]),
        [("stops.csv".to_owned(), "id\na\n".to_owned())]
    );
    assert_eq!(
        untar(&uploads[1]),
        [("routes.csv".to_owned(), "vehicle,stop\n".to_owned())]
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&uploads[2]).unwrap(),
        json!({ "cost": 12.5 })
    );
    // The assets are uploaded wrapped in an object, under `assets`.
    assert_eq!(
        serde_json::from_slice::<Value>(&uploads[3]).unwrap(),
        json!({ "assets": [{ "name": "map", "content_type": "json" }] })
    );
    let created = &requests[4];
    // No environment: `latest`.
    assert_eq!(
        created.target,
        "/v1/applications/routing/runs?instance_id=latest"
    );
    assert_eq!(
        support::body(created),
        json!({
            "upload_id": "upload-0",
            "configuration": {
                "format": format_json("multi-file"),
                "run_type": { "type": "external" },
            },
            "result": {
                "output_upload_id": "upload-1",
                "status": "succeeded",
                "metrics_upload_id": "upload-2",
                "assets_upload_id": "upload-3",
            },
        })
    );
}

#[tokio::test]
async fn tracks_a_run_from_files() {
    let dir = TempDir::new("run");
    let (input, output, logs) = (
        dir.path().join("input.json"),
        dir.path().join("output.json"),
        dir.path().join("logs.txt"),
    );
    fs::write(&input, "{ \"stops\": [\"a\"] }\n").unwrap();
    fs::write(&output, "[1, 2]").unwrap();
    fs::write(&logs, "solving\n").unwrap();
    let params = RunTrackParams {
        tracked_run: TrackedRun {
            input_file: Some(input),
            output_file: Some(output),
            logs_file: Some(logs),
            ..Default::default()
        },
        environment_id: Some("default".to_owned()),
        ..Default::default()
    };
    // `default`: the workspace's default environment, read first.
    let (requests, uploads) = track_after(params, 3, vec![workspace(Some("production"))]).await;
    // Sent as they are.
    assert_eq!(uploads[0], b"{ \"stops\": [\"a\"] }\n");
    assert_eq!(uploads[1], b"[1, 2]");
    assert_eq!(uploads[2], b"solving\n");
    assert_eq!(
        requests[3].target,
        "/v1/applications/routing/runs?instance_id=production"
    );
}

#[tokio::test]
async fn checks_a_tracked_run_before_any_request() {
    let dir = TempDir::new("run");
    let (inputs, file, text, list) = (
        dir.path().join("inputs"),
        dir.path().join("input.json"),
        dir.path().join("logs.txt"),
        dir.path().join("list.json"),
    );
    fs::create_dir_all(&inputs).unwrap();
    fs::write(&file, "{}").unwrap();
    fs::write(&text, "not json").unwrap();
    fs::write(&list, "[1]").unwrap();
    let json_run = |output: Option<Value>| TrackedRun {
        input: Some(json!({})),
        output,
        ..Default::default()
    };
    let output = Some(json!({ "routes": [] }));
    let cases = [
        // Inputs: none, or more than one.
        (
            TrackedRun {
                output: output.clone(),
                ..Default::default()
            },
            None,
            "exactly one",
        ),
        (
            TrackedRun {
                input_file: Some(file.clone()),
                ..json_run(output.clone())
            },
            None,
            "exactly one",
        ),
        // Outputs and logs: more than one, or none.
        (
            TrackedRun {
                output_file: Some(file.clone()),
                ..json_run(output.clone())
            },
            None,
            "at most one",
        ),
        (
            TrackedRun {
                logs: Some("a".to_owned()),
                logs_file: Some(text.clone()),
                ..json_run(output.clone())
            },
            None,
            "at most one",
        ),
        (json_run(None), None, "output or its logs"),
        // An empty output is none.
        (json_run(Some(json!({}))), None, "output or its logs"),
        // The status and the error.
        (
            TrackedRun {
                error: Some("infeasible".to_owned()),
                ..json_run(output.clone())
            },
            None,
            "only for runs that failed",
        ),
        // What the sources hold.
        (
            TrackedRun {
                input: Some(json!([1])),
                ..json_run(output.clone())
            },
            None,
            "JSON object",
        ),
        (
            TrackedRun {
                input: None,
                input_file: Some(text.clone()),
                ..json_run(output.clone())
            },
            None,
            "JSON file",
        ),
        (
            TrackedRun {
                input: None,
                input_file: Some(list.clone()),
                ..json_run(output.clone())
            },
            None,
            "JSON object",
        ),
        (
            TrackedRun {
                input: None,
                input_file: Some(inputs.clone()),
                ..json_run(output.clone())
            },
            None,
            "must be a file",
        ),
        (
            TrackedRun {
                input: None,
                input_dir_path: Some(file.clone()),
                output_dir_path: Some(inputs.clone()),
                ..Default::default()
            },
            None,
            "must be a directory",
        ),
        // The content format and the sources.
        (
            json_run(output.clone()),
            Some(ContentFormat::MultiFile),
            "input_dir_path",
        ),
        (
            TrackedRun {
                input: None,
                input_dir_path: Some(inputs.clone()),
                output: output.clone(),
                ..Default::default()
            },
            None,
            "output_dir_path",
        ),
        (
            TrackedRun {
                input: None,
                input_dir_path: Some(inputs.clone()),
                logs: Some("a".to_owned()),
                ..Default::default()
            },
            Some(ContentFormat::Json),
            "content_format",
        ),
    ];
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    for (tracked_run, content_format, rule) in cases {
        let params = RunTrackParams {
            tracked_run: tracked_run.clone(),
            content_format,
            ..Default::default()
        };
        let err = platform.track_run("routing", params).await.unwrap_err();
        assert!(
            matches!(&err, Error::Invalid { reason, .. } if reason.contains(rule)),
            "{tracked_run:?}: {err}"
        );
    }
    let missing = RunTrackParams {
        tracked_run: TrackedRun {
            input: None,
            input_file: Some(dir.path().join("missing.json")),
            ..json_run(output)
        },
        ..Default::default()
    };
    let err = platform.track_run("routing", missing).await.unwrap_err();
    assert!(matches!(err, Error::Io { .. }), "{err}");
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn serves_the_track_tool() {
    let bucket = MockServer::start(vec![MockResponse::new(200); 2]).await;
    let mut responses = upload_urls(&bucket, 2);
    responses.push(run_created());
    let server = MockServer::start(responses).await;
    let cx = Context::new().with_platform(support::platform(&server));
    let args = json!({
        "workspace_id": "routing",
        "tracked_run": { "status": "succeeded", "input": { "stops": [] }, "output": { "routes": [] } },
        "environment_id": "staging",
    });
    let result = support::call_tool(cx.clone(), "decision_platform_run_track", args)
        .await
        .unwrap();
    assert_eq!(result, json!({ "run_id": "run-2" }));
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[2].target,
        "/v1/applications/routing/runs?instance_id=staging"
    );
    assert_eq!(support::body(&requests[2])["result"]["status"], "succeeded");
    // `tracked_run` is required.
    let err = support::call_tool(
        cx,
        "decision_platform_run_track",
        json!({ "workspace_id": "routing" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArguments(_)), "{err}");
}

#[tokio::test]
async fn refuses_tracked_run_paths_without_local_mode() {
    let server = MockServer::start(Vec::new()).await;
    let remote = Context::new().with_platform(support::platform(&server));
    for field in [
        "input_file",
        "output_file",
        "logs_file",
        "input_dir_path",
        "output_dir_path",
    ] {
        let args = json!({
            "workspace_id": "routing",
            "tracked_run": { "status": "succeeded", field: "/srv/data" },
        });
        let err = support::call_tool(remote.clone(), "decision_platform_run_track", args)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::Failed(Error::Invalid { .. })),
            "{field}: {err}"
        );
    }
    assert!(server.requests().is_empty());
}
