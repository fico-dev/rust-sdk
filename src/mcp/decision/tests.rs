//! Tests of the curated tools, against the mock server for the platform
//! and temporary directories for local mode.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;

use super::*;
use crate::RetryPolicy;
use crate::decision::platform::DEFAULT_CONSOLE_URL;
use crate::http::mock_server::{MockResponse, MockServer, RecordedRequest};
use crate::mcp::cache::{self, MAX_INLINE_BYTES};
use crate::mcp::surface::Risk;
use crate::mcp::tests::{assert_error, client, full_context};
use crate::mcp::{Mode, SurfaceOptions};
use crate::tool::ToolError;

/// The curated tools, with their risk level, whether they are
/// local-only, and the smallest arguments they take.
fn listed() -> Vec<(&'static str, Risk, bool, Value)> {
    let ws = json!({ "workspace_id": "w" });
    let run = json!({ "workspace_id": "w", "run_id": "r" });
    let src = json!({ "src": "/srv/workspace" });
    vec![
        ("decision_workspace_get", Risk::Read, false, ws.clone()),
        (
            "decision_list",
            Risk::Read,
            false,
            json!({ "kind": "workspace" }),
        ),
        (
            "decision_get",
            Risk::Read,
            false,
            json!({ "kind": "build", "workspace_id": "w", "id": "b" }),
        ),
        ("decision_run_get", Risk::Read, false, run.clone()),
        (
            "decision_run_compare",
            Risk::Read,
            false,
            json!({ "run_ids": ["a", "b"], "workspace_id": "w" }),
        ),
        (
            "decision_run_create",
            Risk::Write,
            false,
            json!({ "workspace_id": "w", "input": {} }),
        ),
        (
            "decision_acceptance_create",
            Risk::Write,
            false,
            json!({
                "workspace_id": "w",
                "candidate_environment_id": "c",
                "baseline_environment_id": "b",
                "input_set_id": "i",
                "metrics": [],
            }),
        ),
        (
            "decision_scenario_create",
            Risk::Write,
            false,
            json!({ "workspace_id": "w", "input_set_id": "i", "scenarios": [] }),
        ),
        ("decision_run_cancel", Risk::Destructive, false, run),
        (
            "decision_delete",
            Risk::Destructive,
            false,
            json!({ "kind": "workspace", "workspace_id": "w" }),
        ),
        ("decision_guide", Risk::Read, false, json!({})),
        ("decision_manifest_validate", Risk::Read, true, src.clone()),
        (
            "decision_local_run_create",
            Risk::Write,
            true,
            json!({ "src": "/srv/workspace", "input": {} }),
        ),
        (
            "decision_local_run_get",
            Risk::Read,
            true,
            json!({ "src": "/srv/workspace", "run_id": "r" }),
        ),
        ("decision_local_run_list", Risk::Read, true, src),
        (
            "decision_local_workspace_sync",
            Risk::Write,
            true,
            json!({ "src": "/srv/workspace", "workspace_id": "w" }),
        ),
    ]
}

fn tool(name: &str) -> Tool {
    tools()
        .into_iter()
        .find(|tool| tool.name() == name)
        .unwrap()
}

#[test]
fn declares_the_listed_tools_with_their_flags() {
    let names: Vec<&str> = tools().iter().map(Tool::name).collect();
    let listed = listed();
    let listed_names: Vec<&str> = listed.iter().map(|(name, ..)| *name).collect();
    assert_eq!(names, listed_names);
    for (name, risk, local_only, _) in listed {
        let tool = tool(name);
        assert_eq!(Risk::of(&tool, false), risk, "{name}");
        assert_eq!(Risk::of(&tool, true), risk, "{name}");
        assert_eq!(tool.is_local_only(), local_only, "{name}");
        assert!(
            tool.title().is_some_and(|title| !title.is_empty()),
            "{name}"
        );
    }
}

#[test]
fn has_closed_schemas_with_documented_properties() {
    for tool in tools() {
        let name = tool.name();
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object", "{name}");
        assert_eq!(schema["additionalProperties"], false, "{name}");
        for key in ["oneOf", "anyOf", "allOf", "$ref"] {
            assert!(!schema.contains_key(key), "{name}: {key}");
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        for (property, value) in properties.into_iter().flatten() {
            let description = value["description"].as_str().unwrap_or_default();
            assert!(!description.is_empty(), "{name}.{property}");
            assert!(!description.contains("[`"), "{name}.{property}");
        }
    }
}

#[tokio::test]
async fn needs_the_context_it_uses() {
    for (name, _, _, args) in listed() {
        let result = tool(name).call(Context::new(), args).await;
        let invalid = matches!(result, Err(ToolError::Failed(Error::Invalid { .. })));
        // The guide needs neither the platform nor local mode.
        assert_eq!(invalid, name != "decision_guide", "{name}: {result:?}");
    }
}

#[tokio::test]
async fn needs_local_mode_for_paths() {
    let remote = full_context().without_local();
    let cases = [
        (
            "decision_run_compare",
            "src",
            json!({ "run_ids": ["local-a"] }),
        ),
        (
            "decision_run_create",
            "input_file",
            json!({ "workspace_id": "w" }),
        ),
        (
            "decision_run_create",
            "input_dir",
            json!({ "workspace_id": "w" }),
        ),
    ];
    let mut checked = BTreeSet::new();
    for (name, field, mut args) in cases {
        args[field] = json!("/srv/path");
        let result = tool(name).call(remote.clone(), args).await;
        let reason = match result {
            Err(ToolError::Failed(Error::Invalid { reason })) => reason,
            other => panic!("{name}.{field}: {other:?}"),
        };
        assert!(reason.contains("local mode"), "{name}.{field}: {reason}");
        checked.insert(format!("{name}.{field}"));
    }
    // Every path field of a tool that remote mode serves is checked.
    for tool in tools().into_iter().filter(|tool| !tool.is_local_only()) {
        let properties = tool
            .input_schema()
            .get("properties")
            .and_then(Value::as_object);
        let paths = properties
            .into_iter()
            .flat_map(|properties| properties.keys())
            .filter(|key| *key == "src" || key.ends_with("_file") || key.ends_with("_dir"));
        for path in paths {
            let field = format!("{}.{path}", tool.name());
            assert!(checked.contains(&field), "{field} is not checked");
        }
    }
}

// The mock platform and its fixtures.

/// A platform that sends its requests to `server`, without retries.
fn platform(server: &MockServer) -> Platform {
    let client = crate::Client::builder(server.url())
        .bearer_token("secret")
        .retry(RetryPolicy::none())
        .build()
        .unwrap();
    Platform::new(client)
}

/// A new empty directory, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> TempDir {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let name = format!(
            "fico-sdk-mcp-decision-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir.canonicalize().unwrap())
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A context with the platform at `server`, and local mode in the FICO
/// home `home` when it is set.
fn context(server: &MockServer, home: Option<&Path>) -> Context {
    let cx = Context::new().with_platform(platform(server));
    match home {
        Some(home) => cx.with_local(Local::with_home(home)),
        None => cx,
    }
}

/// Calls the curated tool `name` with `args` in `cx`.
async fn call(cx: Context, name: &str, args: Value) -> Result<Value, ToolError> {
    tool(name).call(cx, args).await
}

/// The reason of a failed call: the message of its `Error::Invalid`.
#[track_caller]
fn reason(result: Result<Value, ToolError>) -> String {
    match result {
        Err(ToolError::Failed(Error::Invalid { reason })) => reason,
        other => panic!("expected an invalid call, got {other:?}"),
    }
}

/// The method and target of each request.
fn sent(requests: &[RecordedRequest]) -> Vec<String> {
    let line = |r: &RecordedRequest| format!("{} {}", r.method, r.target);
    requests.iter().map(line).collect()
}

fn body(request: &RecordedRequest) -> Value {
    serde_json::from_slice(&request.body).unwrap()
}

/// A run's information, as `GET …/runs/{run_id}/metadata` returns it, in
/// `status`, with JSON input and output.
fn information_json(run_id: &str, status: &str) -> Value {
    json!({
        "id": run_id,
        "user_email": "ana@example.com",
        "name": "Monday",
        "description": "Weekly plan",
        "metadata": {
            "status": status,
            "status_v2": status,
            "created_at": "2026-10-07T14:19:02Z",
            "duration": 5021,
            "input_size": 1024,
            "output_size": 2048,
            "error": "",
            "application_id": "routing",
            "application_instance_id": "production",
            "application_version_id": "v1",
            "metrics": { "metrics": { "cost": 12.5 } },
            "options": { "active_options": { "solve.duration": "10s" } },
            "format": { "input": { "type": "json" }, "output": { "type": "json" } },
        },
    })
}

/// A succeeded run with `output`, as `GET …/runs/{run_id}` returns it.
fn with_output(run_id: &str, output: Value) -> Value {
    let mut run = information_json(run_id, "succeeded");
    run["output"] = output;
    run
}

/// The environment `environment_id` of `routing`, running `build_id`.
fn environment_json(environment_id: &str, name: &str, build_id: &str) -> Value {
    json!({
        "id": environment_id,
        "application_id": "routing",
        "version_id": build_id,
        "name": name,
        "description": "",
        "configuration": { "options": { "solve.duration": "10s" } },
        "locked": false,
        "created_at": "2026-10-01T12:00:00Z",
        "updated_at": "2026-10-01T12:00:00Z",
    })
}

/// The build `build_id` of `routing`.
fn build_json(build_id: &str, name: &str, created_at: &str) -> Value {
    json!({
        "id": build_id,
        "application_id": "routing",
        "name": name,
        "description": "",
        "created_at": created_at,
        "updated_at": created_at,
    })
}

/// A run as `GET …/runs` lists it.
fn run_item_json(run_id: &str, status: &str) -> Value {
    json!({
        "id": run_id,
        "user_email": "ana@example.com",
        "name": "",
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

/// A page of `items`, as the platform's paginated lists return it.
fn page(items: Vec<Value>) -> MockResponse {
    MockResponse::json(200, json!({ "items": items }))
}

/// A `404` with the platform's message.
fn not_found(message: &str) -> MockResponse {
    MockResponse::json(404, json!({ "status": "Not found", "error": message }))
}

// The platform tools.

#[tokio::test]
async fn gets_a_workspace_with_its_environments_builds_runs_and_tests() {
    let server = MockServer::start(vec![
        MockResponse::json(
            200,
            json!({ "id": "routing", "name": "Routing", "default_instance_id": "production" }),
        ),
        page(vec![environment_json("production", "Production", "v2")]),
        page(vec![
            build_json("v1", "First", "2026-10-01T00:00:00Z"),
            build_json("v2", "Second", "2026-10-02T00:00:00Z"),
        ]),
        MockResponse::json(
            200,
            json!({ "runs": [run_item_json("run-2", "running"), run_item_json("run-1", "failed")] }),
        ),
        page(vec![json!({
            "id": "acceptance-1", "name": "v2 vs v1", "description": "", "status": "started",
            "created_at": "2026-10-02T00:00:00Z", "updated_at": "2026-10-02T00:00:00Z",
        })]),
        page(vec![]),
        page(vec![]),
        page(vec![]),
    ])
    .await;
    let args = json!({ "workspace_id": "routing" });
    let result = call(context(&server, None), "decision_workspace_get", args)
        .await
        .unwrap();
    assert_eq!(
        sent(&server.requests()),
        [
            "GET /v1/applications/routing",
            "GET /v1/applications/routing/instances?pagereturn=true",
            "GET /v1/applications/routing/versions?pagereturn=true",
            "GET /v1/applications/routing/runs?pagereturn=true",
            "GET /v1/applications/routing/experiments/acceptance?pagereturn=true",
            "GET /v1/applications/routing/experiments/batch?type=scenario&pagereturn=true",
            "GET /v1/applications/routing/experiments/shadow?pagereturn=true",
            "GET /v1/applications/routing/experiments/switchback?pagereturn=true",
        ]
    );
    assert_eq!(result["workspace_id"], "routing");
    assert_eq!(result["name"], "Routing");
    // Names next to IDs: the build each environment runs, and the
    // environment each run ran in.
    let environment = &result["environments"][0];
    assert_eq!(environment["environment_id"], "production");
    assert_eq!(environment["build_name"], "Second");
    assert_eq!(environment["options"], json!({ "solve.duration": "10s" }));
    assert_eq!(result["builds"]["count"], 2);
    assert_eq!(result["builds"]["latest"][0]["build_id"], "v2");
    let runs = &result["recent_runs"];
    assert_eq!(runs["by_status"], json!({ "failed": 1, "running": 1 }));
    assert_eq!(runs["latest"][0]["environment_name"], "Production");
    assert_eq!(
        result["running_tests"],
        json!([{ "kind": "acceptance", "id": "acceptance-1", "name": "v2 vs v1", "status": "started" }])
    );
}

#[tokio::test]
async fn counts_only_started_scenario_tests_as_running() {
    let scenario = |id: &str, status: &str| {
        json!({
            "id": id, "name": id, "status": status,
            "created_at": "2026-10-20T14:00:00Z", "updated_at": "2026-10-20T14:00:00Z",
        })
    };
    let server = MockServer::start(vec![
        MockResponse::json(200, json!({ "id": "routing", "name": "Routing" })),
        page(vec![]),
        page(vec![]),
        MockResponse::json(200, json!({ "runs": [] })),
        page(vec![]),
        // A list reports a failed or canceled test as `unknown`.
        page(vec![
            scenario("s-running", "started"),
            scenario("s-failed", "unknown"),
        ]),
        page(vec![]),
        page(vec![]),
    ])
    .await;
    let args = json!({ "workspace_id": "routing" });
    let result = call(context(&server, None), "decision_workspace_get", args)
        .await
        .unwrap();
    assert_eq!(
        result["running_tests"],
        json!([{ "kind": "scenario", "id": "s-running", "name": "s-running", "status": "started" }])
    );
}

#[tokio::test]
async fn filters_scenario_tests_only_by_the_statuses_a_list_reports() {
    let server = MockServer::start(vec![]).await;
    for status in ["failed", "canceled", "draft"] {
        let args = json!({ "kind": "scenario", "workspace_id": "routing", "status": status });
        let reason = reason(call(context(&server, None), "decision_list", args).await);
        assert!(
            reason.contains("only `started` and `completed`"),
            "{status}: {reason}"
        );
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn lists_what_exists_for_a_workspace_that_does_not() {
    let server = MockServer::start(vec![
        not_found("app with identifier routng not found"),
        not_found("app with identifier routng not found"),
        page(vec![
            json!({ "id": "routing", "name": "Routing" }),
            json!({ "id": "pricing" }),
        ]),
    ])
    .await;
    let args = json!({ "workspace_id": "routng" });
    let reason = reason(call(context(&server, None), "decision_workspace_get", args).await);
    assert_eq!(
        reason,
        "workspace `routng` does not exist. Workspaces: `routing` (Routing), `pricing`."
    );
}

#[tokio::test]
async fn lists_resources_up_to_the_limit_and_says_there_are_more() {
    let builds: Vec<Value> = (1..=4)
        .map(|n| build_json(&format!("v{n}"), "", &format!("2026-10-0{n}T00:00:00Z")))
        .collect();
    let server = MockServer::start(vec![page(builds)]).await;
    let args = json!({ "kind": "build", "workspace_id": "routing", "limit": 2 });
    let result = call(context(&server, None), "decision_list", args)
        .await
        .unwrap();
    // Newest first.
    let ids: Vec<&str> = result["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["build_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["v4", "v3"]);
    assert_eq!(result["count"], 2);
    assert_eq!(result["more"], true);
    assert!(result["hint"].as_str().unwrap().contains("larger `limit`"));
}

#[tokio::test]
async fn lists_twenty_resources_by_default() {
    let builds: Vec<Value> = (0..25)
        .map(|n| {
            build_json(
                &format!("v{n:02}"),
                "",
                &format!("2026-10-01T00:00:{n:02}Z"),
            )
        })
        .collect();
    let server = MockServer::start(vec![page(builds.clone()), page(builds[..5].to_vec())]).await;
    let args = json!({ "kind": "build", "workspace_id": "routing" });
    let result = call(context(&server, None), "decision_list", args)
        .await
        .unwrap();
    assert_eq!(result["count"], 20);
    assert_eq!(result["more"], true);
    let args = json!({ "kind": "build", "workspace_id": "routing" });
    let result = call(context(&server, None), "decision_list", args)
        .await
        .unwrap();
    assert_eq!(result["count"], 5);
    assert!(result.get("more").is_none());
}

#[tokio::test]
async fn lists_runs_by_status_with_their_environment_names() {
    let server = MockServer::start(vec![
        MockResponse::json(200, json!({ "runs": [run_item_json("run-1", "failed")] })),
        page(vec![environment_json("production", "Production", "v1")]),
    ])
    .await;
    let args = json!({
        "kind": "run", "workspace_id": "routing", "status": "failed", "limit": 5,
    });
    let result = call(context(&server, None), "decision_list", args)
        .await
        .unwrap();
    assert_eq!(
        result["items"],
        json!([{
            "run_id": "run-1",
            "status": "failed",
            "created_at": "2026-10-07T14:19:02Z",
            "environment_id": "production",
            "environment_name": "Production",
            "build_id": "v1",
        }])
    );
    let args = json!({ "kind": "run", "workspace_id": "routing", "status": "broken" });
    let reason = reason(call(context(&server, None), "decision_list", args).await);
    assert!(reason.contains("`broken` is not a run status"), "{reason}");
}

#[tokio::test]
async fn needs_a_workspace_to_list_its_resources() {
    let server = MockServer::start(vec![]).await;
    for workspace_id in [Value::Null, json!(""), json!("   ")] {
        let mut args = json!({ "kind": "environment" });
        if !workspace_id.is_null() {
            args["workspace_id"] = workspace_id;
        }
        let reason = reason(call(context(&server, None), "decision_list", args).await);
        assert!(reason.contains("`workspace_id` is required"), "{reason}");
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn gets_an_environment_with_its_build_name() {
    let server = MockServer::start(vec![
        MockResponse::json(200, environment_json("production", "Production", "v2")),
        MockResponse::json(200, build_json("v2", "Second", "2026-10-02T00:00:00Z")),
    ])
    .await;
    let args = json!({ "kind": "environment", "workspace_id": "routing", "id": "production" });
    let result = call(context(&server, None), "decision_get", args)
        .await
        .unwrap();
    assert_eq!(
        result,
        json!({
            "environment_id": "production",
            "name": "Production",
            "build_id": "v2",
            "build_name": "Second",
            "options": { "solve.duration": "10s" },
        })
    );
}

#[tokio::test]
async fn shows_that_an_environment_sets_no_options() {
    let mut environment = environment_json("staging", "Staging", "v1");
    environment["configuration"] = json!({ "execution_class": "6c9500mb870s" });
    let server = MockServer::start(vec![
        MockResponse::json(200, environment),
        MockResponse::json(200, build_json("v1", "First", "2026-10-01T00:00:00Z")),
    ])
    .await;
    let args = json!({ "kind": "environment", "workspace_id": "routing", "id": "staging" });
    let result = call(context(&server, None), "decision_get", args)
        .await
        .unwrap();
    assert_eq!(result["options"], json!({}));
}

#[tokio::test]
async fn lists_the_environments_when_one_does_not_exist() {
    let server = MockServer::start(vec![
        not_found("app instance with identifier routing not found"),
        MockResponse::json(200, json!({ "id": "routing" })),
        not_found("app instance with identifier routing not found"),
        page(vec![
            environment_json("production", "Production", "v1"),
            environment_json("staging", "Staging", "v1"),
        ]),
    ])
    .await;
    let args = json!({ "kind": "environment", "workspace_id": "routing", "id": "prod" });
    let reason = reason(call(context(&server, None), "decision_get", args).await);
    assert_eq!(
        reason,
        "environment `prod` does not exist in workspace `routing`. Its environments: \
         `production` (Production), `staging` (Staging)."
    );
}

#[tokio::test]
async fn gets_a_secrets_collection_without_its_values() {
    let server = MockServer::start(vec![MockResponse::json(
        200,
        json!({
            "id": "keys", "application_id": "routing", "name": "Keys", "description": "",
            "created_at": "2026-10-01T00:00:00Z", "updated_at": "2026-10-01T00:00:00Z",
            "secrets": [{ "type": "env", "location": "API_KEY", "value": "hunter2" }],
        }),
    )])
    .await;
    let args = json!({ "kind": "secrets", "workspace_id": "routing", "id": "keys" });
    let result = call(context(&server, None), "decision_get", args)
        .await
        .unwrap();
    assert_eq!(
        result["secrets"],
        json!([{ "type": "env", "location": "API_KEY" }])
    );
    assert!(!result.to_string().contains("hunter2"));
}

/// An acceptance test of `routing` in `status`.
fn acceptance_json(status: &str) -> Value {
    json!({
        "id": "acceptance-1",
        "name": "v2 vs v1",
        "description": "",
        "app_id": "routing",
        "control": { "instance_id": "production", "version_id": "v1" },
        "candidate": { "instance_id": "staging", "version_id": "v2" },
        "metrics": [{
            "field": "cost", "metric_type": "direct-comparison", "statistic": "mean",
            "params": { "operator": "le", "tolerance": { "type": "relative", "value": 5.0 } },
        }],
        "status": status,
        "created_at": "2026-10-20T14:00:00Z",
        "updated_at": "2026-10-20T14:00:00Z",
    })
}

/// The statistics of one environment of an acceptance test.
fn statistics_json(environment_id: &str, mean: f64) -> Value {
    json!({
        "instance_id": environment_id,
        "version_id": "v1",
        "number_of_runs_total": 2,
        "distribution_summary_statistics": {
            "min": 1.0, "max": 2.0, "count": 2, "mean": mean, "std": 0.5,
            "shifted_geometric_mean": 1.4, "shift_parameter": 1.0,
        },
        "distribution_percentiles": {
            "p01": 1.0, "p05": 1.0, "p10": 1.0, "p25": 1.2, "p50": 1.5,
            "p75": 1.8, "p90": 1.9, "p95": 2.0, "p99": 2.0,
        },
    })
}

#[tokio::test]
async fn gets_an_acceptance_test_with_its_results() {
    let mut completed = acceptance_json("completed");
    completed["results"] = json!({
        "passed": true,
        "metric_results": [{
            "metric": completed["metrics"][0],
            "statistics": {
                "control": statistics_json("production", 12.0),
                "candidate": statistics_json("staging", 11.5),
            },
            "passed": true,
        }],
    });
    let server = MockServer::start(vec![
        MockResponse::json(200, completed),
        page(vec![
            environment_json("production", "Production", "v1"),
            environment_json("staging", "Staging", "v2"),
        ]),
    ])
    .await;
    let args = json!({ "kind": "acceptance", "workspace_id": "routing", "id": "acceptance-1" });
    let result = call(context(&server, None), "decision_get", args)
        .await
        .unwrap();
    // The view leaves the test's runs out, so they are not read.
    assert_eq!(
        sent(&server.requests()),
        [
            "GET /v1/applications/routing/experiments/acceptance/acceptance-1",
            "GET /v1/applications/routing/instances?pagereturn=true",
        ]
    );
    assert_eq!(result.get("runs"), None);
    assert_eq!(result["passed"], true);
    assert_eq!(result["candidate"]["environment_name"], "Staging");
    assert_eq!(result["baseline"]["environment_name"], "Production");
    assert_eq!(
        result["metrics"],
        json!([{
            "field": "cost",
            "metric_type": "direct-comparison",
            "statistic": "mean",
            "operator": "le",
            "tolerance": { "type": "relative", "value": 5.0 },
            "candidate": 11.5,
            "baseline": 12.0,
            "runs": { "candidate": 2, "baseline": 2 },
            "passed": true,
        }])
    );
    assert!(result.get("hint").is_none());
}

#[tokio::test]
async fn returns_a_running_test_with_a_hint_when_the_wait_runs_out() {
    let server = MockServer::start(vec![
        MockResponse::json(200, acceptance_json("started")),
        MockResponse::json(200, acceptance_json("started")),
        MockResponse::json(200, acceptance_json("started")),
        page(vec![]),
    ])
    .await;
    let args = json!({
        "kind": "acceptance", "workspace_id": "routing", "id": "acceptance-1", "wait_seconds": 1,
    });
    let result = call(context(&server, None), "decision_get", args)
        .await
        .unwrap();
    assert_eq!(result["status"], "started");
    let hint = result["hint"].as_str().unwrap();
    assert!(hint.contains("has not finished after 1 s"), "{hint}");
    assert!(hint.contains("`wait_seconds`"), "{hint}");
}

#[tokio::test]
async fn gets_a_run_with_its_metrics_and_environment_name() {
    let server = MockServer::start(vec![
        MockResponse::json(200, information_json("run-1", "succeeded")),
        MockResponse::json(200, environment_json("production", "Production", "v1")),
    ])
    .await;
    let args = json!({ "workspace_id": "routing", "run_id": "run-1" });
    let result = call(context(&server, None), "decision_run_get", args)
        .await
        .unwrap();
    assert_eq!(
        sent(&server.requests()),
        [
            "GET /v1/applications/routing/runs/run-1/metadata",
            "GET /v1/applications/routing/instances/production",
        ]
    );
    assert_eq!(
        result,
        json!({
            "run_id": "run-1",
            "name": "Monday",
            "description": "Weekly plan",
            "status": "succeeded",
            "workspace_id": "routing",
            "environment_id": "production",
            "environment_name": "Production",
            "build_id": "v1",
            "created_at": "2026-10-07T14:19:02Z",
            "duration_ms": 5021.0,
            "metrics": { "cost": 12.5 },
            "options": { "solve.duration": "10s" },
            "input_format": "json",
            "output_format": "json",
            "input_bytes": 1024,
            "output_bytes": 2048,
            "console_url": format!("{}/app/routing/run/run-1?view=details", DEFAULT_CONSOLE_URL.unwrap()),
            "hint": "The run succeeded. Call `decision_run_get` with `include: [\"output\"]` for \
                     its output.",
        })
    );
}

#[tokio::test]
async fn includes_small_parts_inline() {
    let server = MockServer::start(vec![
        MockResponse::json(200, information_json("run-1", "succeeded")),
        MockResponse::json(200, environment_json("production", "Production", "v1")),
        // `get_run` reads the metadata again, then the run with its output.
        MockResponse::json(200, information_json("run-1", "succeeded")),
        MockResponse::json(200, with_output("run-1", json!({ "solution": [1, 2] }))),
        MockResponse::json(200, json!({ "log": "solved\n" })),
    ])
    .await;
    let args = json!({
        "workspace_id": "routing", "run_id": "run-1", "include": ["output", "logs"],
    });
    let result = call(context(&server, None), "decision_run_get", args)
        .await
        .unwrap();
    assert_eq!(result["output"], json!({ "solution": [1, 2] }));
    assert_eq!(result["logs"], "solved\n");
    assert!(result.get("hint").is_none());
}

#[tokio::test]
async fn caches_large_parts_in_local_mode_and_reads_them_again_without_a_download() {
    let home = TempDir::new();
    let large = "x".repeat(MAX_INLINE_BYTES as usize + 1);
    let server = MockServer::start(vec![
        MockResponse::json(200, information_json("run-1", "failed")),
        MockResponse::json(200, environment_json("production", "Production", "v1")),
        MockResponse::json(200, json!({ "log": large })),
        // The second call: only the run and its environment.
        MockResponse::json(200, information_json("run-1", "failed")),
        MockResponse::json(200, environment_json("production", "Production", "v1")),
    ])
    .await;
    let cx = context(&server, Some(home.path()));
    let args = json!({ "workspace_id": "routing", "run_id": "run-1", "include": ["logs"] });
    let result = call(cx.clone(), "decision_run_get", args.clone())
        .await
        .unwrap();
    let file = home.path().join("decision/cache/runs/run-1/logs/logs.log");
    assert_eq!(result["logs_file"], json!(file));
    assert_eq!(result["logs_bytes"], large.len());
    assert!(result.get("logs").is_none());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), large);
    // A failed run's output is not there; its logs are what to read.
    assert!(result.get("hint").is_none());

    let again = call(cx, "decision_run_get", args).await.unwrap();
    assert_eq!(again["logs_file"], json!(file));
    let requests = server.requests();
    assert_eq!(requests.len(), 5);
    assert!(
        !sent(&requests[3..])
            .iter()
            .any(|line| line.contains("/logs")),
        "{:?}",
        sent(&requests)
    );
}

/// A tar.gz of `files`, as (path, contents), as the platform stores
/// multi-file content.
fn tar_gz(files: &[(&str, &str)]) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::GzEncoder;

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

#[tokio::test]
async fn downloads_multi_file_output_into_the_cache_once() {
    let home = TempDir::new();
    let archive = tar_gz(&[("solution.csv", "vehicle,stop\ntruck-1,a\n")]);
    let bucket = MockServer::start(vec![MockResponse::new(200).body(archive)]).await;
    let url = format!("{}/output.tar.gz?X-Amz-Signature=abc", bucket.url());
    let mut information = information_json("run-1", "succeeded");
    information["metadata"]["format"]["output"]["type"] = json!("multi-file");
    let mut run = information.clone();
    run["output"] = json!({ "url": url });
    let server = MockServer::start(vec![
        MockResponse::json(200, information.clone()),
        MockResponse::json(200, environment_json("production", "Production", "v1")),
        // `get_run` into the cache.
        MockResponse::json(200, information.clone()),
        MockResponse::json(200, run),
        MockResponse::json(200, json!({ "url": url })),
        // The second call: only the run and its environment.
        MockResponse::json(200, information),
        MockResponse::json(200, environment_json("production", "Production", "v1")),
    ])
    .await;
    let cx = context(&server, Some(home.path()));
    let args = json!({ "workspace_id": "routing", "run_id": "run-1", "include": ["output"] });
    let result = call(cx.clone(), "decision_run_get", args.clone())
        .await
        .unwrap();
    let dir = home
        .path()
        .join("decision/cache/runs/run-1/outputs/solutions");
    assert_eq!(result["output_dir"], json!(dir));
    assert_eq!(
        std::fs::read_to_string(dir.join("solution.csv")).unwrap(),
        "vehicle,stop\ntruck-1,a\n"
    );
    let again = call(cx, "decision_run_get", args).await.unwrap();
    assert_eq!(again["output_dir"], json!(dir));
    assert_eq!(server.requests().len(), 7);
    assert_eq!(bucket.requests().len(), 1);
    // Remote mode can't download it.
    let server = MockServer::start(vec![
        MockResponse::json(200, {
            let mut information = information_json("run-1", "succeeded");
            information["metadata"]["format"]["output"]["type"] = json!("multi-file");
            information
        }),
        MockResponse::json(200, environment_json("production", "Production", "v1")),
    ])
    .await;
    let args = json!({ "workspace_id": "routing", "run_id": "run-1", "include": ["output"] });
    let result = call(context(&server, None), "decision_run_get", args)
        .await
        .unwrap();
    let reason = result["output_missing"].as_str().unwrap();
    assert!(reason.contains("only local mode"), "{reason}");
}

#[tokio::test]
async fn links_large_parts_in_remote_mode_without_downloading_them() {
    let mut information = information_json("run-1", "succeeded");
    information["metadata"]["output_size"] = json!(MAX_INLINE_BYTES + 1);
    let server = MockServer::start(vec![
        MockResponse::json(200, information),
        MockResponse::json(200, environment_json("production", "Production", "v1")),
    ])
    .await;
    let args = json!({ "workspace_id": "routing", "run_id": "run-1", "include": ["output"] });
    let result = call(context(&server, None), "decision_run_get", args)
        .await
        .unwrap();
    assert_eq!(
        result["output_resource_link"],
        "fico://decision/workspaces/routing/runs/run-1/output"
    );
    assert_eq!(result["output_bytes"], MAX_INLINE_BYTES + 1);
    // The output was not downloaded.
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn says_why_a_part_is_missing() {
    let server = MockServer::start(vec![
        MockResponse::json(200, information_json("run-1", "running")),
        MockResponse::json(200, environment_json("production", "Production", "v1")),
    ])
    .await;
    let args = json!({
        "workspace_id": "routing", "run_id": "run-1", "include": ["output", "logs"],
    });
    let result = call(context(&server, None), "decision_run_get", args)
        .await
        .unwrap();
    for part in ["output_missing", "logs_missing"] {
        let reason = result[part].as_str().unwrap();
        assert!(reason.contains("has not ended yet"), "{part}: {reason}");
    }
    let hint = result["hint"].as_str().unwrap();
    assert!(hint.contains("still running"), "{hint}");
}

#[tokio::test]
async fn waits_for_a_run_and_returns_its_status_when_the_wait_runs_out() {
    // A check at once, one after the initial delay, and one after the
    // first wait, past the second's limit.
    let mut responses: Vec<MockResponse> = (0..3)
        .map(|_| MockResponse::json(200, information_json("run-1", "running")))
        .collect();
    responses.push(MockResponse::json(
        200,
        environment_json("production", "Production", "v1"),
    ));
    let server = MockServer::start(responses).await;
    let args = json!({ "workspace_id": "routing", "run_id": "run-1", "wait_seconds": 1 });
    let result = call(context(&server, None), "decision_run_get", args)
        .await
        .unwrap();
    assert_eq!(result["status"], "running");
    assert_eq!(result["environment_name"], "Production");
    let hint = result["hint"].as_str().unwrap();
    assert!(hint.contains("still running after 1 s"), "{hint}");
    assert!(hint.contains("`decision_run_get`"), "{hint}");
}

#[tokio::test]
async fn lists_the_latest_runs_when_a_run_does_not_exist() {
    let server = MockServer::start(vec![
        MockResponse::json(
            400,
            json!({ "status": "Bad Request", "error": "invalid value for appRun: run-9" }),
        ),
        MockResponse::json(200, json!({ "id": "routing" })),
        MockResponse::json(
            400,
            json!({ "status": "Bad Request", "error": "invalid value for appRun: run-9" }),
        ),
        MockResponse::json(
            200,
            json!({ "runs": [run_item_json("run-1", "succeeded")] }),
        ),
    ])
    .await;
    let args = json!({ "workspace_id": "routing", "run_id": "run-9" });
    let reason = reason(call(context(&server, None), "decision_run_get", args).await);
    assert_eq!(
        reason,
        "run `run-9` does not exist in workspace `routing`. Its latest runs: `run-1` \
         (succeeded). For more, use `decision_list` with kind `run`."
    );
}

#[tokio::test]
async fn compares_platform_runs() {
    let mut second = information_json("run-2", "succeeded");
    second["metadata"]["metrics"] = json!({ "metrics": { "cost": 10.0 } });
    let server = MockServer::start(vec![
        MockResponse::json(200, information_json("run-1", "succeeded")),
        MockResponse::json(200, second),
    ])
    .await;
    let args = json!({ "workspace_id": "routing", "run_ids": ["run-1", "run-2"] });
    let result = call(context(&server, None), "decision_run_compare", args)
        .await
        .unwrap();
    assert_eq!(result["run_ids"], json!(["run-1", "run-2"]));
    assert_eq!(
        result["metrics"]["cost"],
        json!({ "values": { "run-1": 12.5, "run-2": 10.0 }, "differ": true })
    );
    assert_eq!(result["other_differences"]["id"]["run-2"], "run-2");
}

#[tokio::test]
async fn needs_src_to_compare_local_runs() {
    let server = MockServer::start(vec![]).await;
    let home = TempDir::new();
    let args = json!({ "run_ids": ["local-a", "run-2"], "workspace_id": "routing" });
    let reason = reason(
        call(
            context(&server, Some(home.path())),
            "decision_run_compare",
            args,
        )
        .await,
    );
    assert!(reason.contains("`src` is required"), "{reason}");
}

#[tokio::test]
async fn starts_a_run_waits_for_it_and_returns_it() {
    let server = MockServer::start(vec![
        MockResponse::json(200, json!({ "run_id": "run-1" })),
        MockResponse::json(200, information_json("run-1", "succeeded")),
        MockResponse::json(200, environment_json("production", "Production", "v1")),
    ])
    .await;
    let args = json!({
        "workspace_id": "routing",
        "environment_id": "production",
        "input": { "orders": 3 },
        "options": { "solve.duration": "5s" },
        // Blank optional strings count as absent.
        "managed_input_id": "",
        "from_run_id": "  ",
    });
    let result = call(context(&server, None), "decision_run_create", args)
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(
        sent(&requests[..2]),
        [
            "POST /v1/applications/routing/runs?instance_id=production",
            "GET /v1/applications/routing/runs/run-1/metadata",
        ]
    );
    assert_eq!(body(&requests[0])["input"], json!({ "orders": 3 }));
    assert_eq!(
        body(&requests[0])["options"],
        json!({ "solve.duration": "5s" })
    );
    assert_eq!(result["run_id"], "run-1");
    assert_eq!(result["status"], "succeeded");
    assert_eq!(result["environment_name"], "Production");
}

#[tokio::test]
async fn starts_a_run_on_the_default_environment() {
    let server = MockServer::start(vec![
        MockResponse::json(
            200,
            json!({ "id": "routing", "default_instance": "production" }),
        ),
        MockResponse::json(200, json!({ "run_id": "run-1" })),
        MockResponse::json(200, information_json("run-1", "queued")),
        MockResponse::json(200, environment_json("production", "Production", "v1")),
    ])
    .await;
    let args = json!({
        "workspace_id": "routing",
        "environment_id": "default",
        "input": { "orders": 3 },
        "wait_seconds": 0,
    });
    call(context(&server, None), "decision_run_create", args)
        .await
        .unwrap();
    assert_eq!(
        sent(&server.requests()[..2]),
        [
            "GET /v1/applications/routing",
            "POST /v1/applications/routing/runs?instance_id=production",
        ]
    );
}

#[tokio::test]
async fn runs_again_from_a_run() {
    let server = MockServer::start(vec![
        MockResponse::json(200, information_json("run-1", "succeeded")),
        MockResponse::json(200, json!({ "orders": 3 })),
        MockResponse::json(200, json!({ "run_id": "run-2" })),
        MockResponse::json(200, information_json("run-2", "queued")),
        MockResponse::json(200, environment_json("production", "Production", "v1")),
    ])
    .await;
    let args = json!({ "workspace_id": "routing", "from_run_id": "run-1", "wait_seconds": 0 });
    let result = call(context(&server, None), "decision_run_create", args)
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(requests[2].method, "POST");
    assert_eq!(
        body(&requests[2])["tracking"],
        json!({ "cloned_run_id": "run-1" })
    );
    assert_eq!(result["run_id"], "run-2");
    let hint = result["hint"].as_str().unwrap();
    assert!(hint.contains("still queued"), "{hint}");
}

#[tokio::test]
async fn needs_exactly_one_input_to_start_a_run() {
    let server = MockServer::start(vec![]).await;
    for args in [
        json!({ "workspace_id": "routing" }),
        json!({ "workspace_id": "routing", "input": {}, "managed_input_id": "m" }),
        json!({ "workspace_id": "routing", "input": "", "managed_input_id": " " }),
    ] {
        let reason = reason(call(context(&server, None), "decision_run_create", args).await);
        assert!(reason.contains("set exactly one of"), "{reason}");
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn lists_the_environments_when_a_run_names_one_that_does_not_exist() {
    let server = MockServer::start(vec![
        not_found("app instance with identifier routing not found"),
        MockResponse::json(200, json!({ "id": "routing" })),
        not_found("app instance with identifier routing not found"),
        page(vec![environment_json("production", "Production", "v1")]),
    ])
    .await;
    let args = json!({ "workspace_id": "routing", "environment_id": "prod", "input": {} });
    let reason = reason(call(context(&server, None), "decision_run_create", args).await);
    assert!(
        reason.ends_with("Its environments: `production` (Production)."),
        "{reason}"
    );
}

#[tokio::test]
async fn makes_an_input_set_of_runs_for_an_acceptance_test() {
    let input_set = json!({
        "id": "inpset-1", "name": "inpset-1", "description": "", "app_id": "routing",
        "input_ids": ["run-1", "run-2"], "inputs": [],
        "created_at": "2026-10-20T13:00:00Z", "updated_at": "2026-10-20T13:00:00Z",
    });
    let server = MockServer::start(vec![
        // `create_input_set`, then `create_acceptance`'s input set and batch.
        MockResponse::json(200, input_set.clone()),
        MockResponse::json(200, input_set),
        // The mock can't echo the test's generated ID, so the create stops
        // at the batch, as in `decision_platform_acceptance.rs`.
        MockResponse::json(200, json!({ "id": "acceptance-1" })),
    ])
    .await;
    let args = json!({
        "workspace_id": "routing",
        "candidate_environment_id": "staging",
        "baseline_environment_id": "production",
        "input_set_id": "",
        "run_ids": ["run-1", "run-2"],
        "metrics": [{
            "field": "cost", "metric_type": "direct-comparison", "statistic": "mean",
            "params": { "operator": "le", "tolerance": { "type": "relative", "value": 5.0 } },
        }],
        "name": "v2 vs v1",
        "wait_seconds": 0,
    });
    let reason = reason(call(context(&server, None), "decision_acceptance_create", args).await);
    assert!(reason.contains("batch experiment"), "{reason}");
    let requests = server.requests();
    assert_eq!(
        sent(&requests),
        [
            "POST /v1/applications/routing/experiments/inputsets",
            "GET /v1/applications/routing/experiments/inputsets/inpset-1",
            "POST /v1/applications/routing/experiments/batch",
        ]
    );
    assert_eq!(body(&requests[0])["run_ids"], json!(["run-1", "run-2"]));
    let batch = body(&requests[2]);
    assert_eq!(batch["name"], "v2 vs v1");
    let environments: Vec<&Value> = batch["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|run| &run["instance_id"])
        .collect();
    assert_eq!(
        environments,
        ["staging", "staging", "production", "production"]
    );
}

#[tokio::test]
async fn needs_a_metric_before_making_an_input_set() {
    let server = MockServer::start(vec![]).await;
    let args = json!({
        "workspace_id": "routing",
        "candidate_environment_id": "staging",
        "baseline_environment_id": "production",
        "run_ids": ["run-1"],
        "metrics": [],
    });
    let reason = reason(call(context(&server, None), "decision_acceptance_create", args).await);
    assert_eq!(reason, "`metrics` needs at least one metric");
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn names_the_input_set_it_made_when_the_test_fails() {
    let input_set = json!({
        "id": "inpset-1", "name": "inpset-1", "description": "", "app_id": "routing",
        "input_ids": ["run-1"], "inputs": [],
        "created_at": "2026-10-20T13:00:00Z", "updated_at": "2026-10-20T13:00:00Z",
    });
    let args = json!({
        "workspace_id": "routing",
        "candidate_environment_id": "staging",
        "baseline_environment_id": "production",
        "run_ids": ["run-1"],
        "metrics": [{
            "field": "cost", "metric_type": "direct-comparison", "statistic": "mean",
            "params": { "operator": "le", "tolerance": { "type": "relative", "value": 5.0 } },
        }],
    });
    let note = "The input set `inpset-1` was made from `run_ids` before the test failed: pass \
                it as `input_set_id` to try again";
    // A failure the platform reports.
    let server = MockServer::start(vec![
        MockResponse::json(200, input_set.clone()),
        MockResponse::json(200, input_set.clone()),
        MockResponse::json(
            400,
            json!({ "status": "Bad Request", "error": "the batch could not be created" }),
        ),
    ])
    .await;
    let failed = reason(
        call(
            context(&server, None),
            "decision_acceptance_create",
            args.clone(),
        )
        .await,
    );
    assert!(
        failed.contains("the batch could not be created"),
        "{failed}"
    );
    assert!(failed.ends_with(note), "{failed}");
    // An environment that does not exist: the reason that lists the real
    // ones, once, without a second prefix.
    let server = MockServer::start(vec![
        MockResponse::json(200, input_set.clone()),
        MockResponse::json(200, input_set),
        not_found("app instance with identifier routing not found"),
        MockResponse::json(200, json!({ "id": "routing" })),
        not_found("app instance with identifier routing not found"),
        page(vec![environment_json("production", "Production", "v1")]),
    ])
    .await;
    let missing = reason(call(context(&server, None), "decision_acceptance_create", args).await);
    assert_eq!(
        missing,
        format!(
            "environment `staging` does not exist in workspace `routing`. Its environments: \
             `production` (Production). {note}"
        )
    );
}

#[tokio::test]
async fn needs_an_input_set_or_runs_for_an_acceptance_test() {
    let server = MockServer::start(vec![]).await;
    let args = json!({
        "workspace_id": "routing",
        "candidate_environment_id": "staging",
        "baseline_environment_id": "production",
        "input_set_id": "last-week",
        "run_ids": ["run-1"],
        "metrics": [{
            "field": "cost", "metric_type": "direct-comparison", "statistic": "mean",
            "params": { "operator": "le", "tolerance": { "type": "relative", "value": 5.0 } },
        }],
    });
    let reason = reason(call(context(&server, None), "decision_acceptance_create", args).await);
    assert!(
        reason.contains("exactly one of `input_set_id` and `run_ids`"),
        "{reason}"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn starts_a_scenario_test_of_option_values() {
    let input_set = json!({
        "id": "last-week", "name": "Last week", "description": "", "app_id": "routing",
        "input_ids": ["run-1"], "inputs": [],
        "created_at": "2026-10-01T12:00:00Z", "updated_at": "2026-10-01T12:00:00Z",
    });
    let metadata = json!({
        "id": "scenario-1", "name": "Durations", "app_id": "routing", "status": "started",
        "created_at": "2026-10-20T14:00:00Z", "updated_at": "2026-10-20T14:00:00Z",
        "number_of_requested_runs": 2,
    });
    let mut test = metadata.clone();
    test["input_set_id"] = json!("");
    let server = MockServer::start(vec![
        MockResponse::json(200, environment_json("staging", "Staging", "v1")),
        MockResponse::json(200, input_set),
        MockResponse::json(202, json!({ "id": "scenario-1" })),
        MockResponse::json(200, metadata),
        MockResponse::json(200, test),
        MockResponse::json(200, json!({ "runs": [] })),
    ])
    .await;
    let args = json!({
        "workspace_id": "routing",
        "input_set_id": "last-week",
        "scenarios": [{ "environment_id": "staging", "options": { "solve.duration": ["5s", "10s"] } }],
        "name": "Durations",
        "wait_seconds": 0,
    });
    let result = call(context(&server, None), "decision_scenario_create", args)
        .await
        .unwrap();
    let requests = server.requests();
    let created = body(&requests[2]);
    assert_eq!(created["type"], "scenario");
    assert_eq!(
        created["option_sets"],
        json!({
            "scenario-1_0": { "solve.duration": "5s" },
            "scenario-1_1": { "solve.duration": "10s" },
        })
    );
    assert_eq!(result["scenario_id"], "scenario-1");
    assert_eq!(result["status"], "started");
    assert!(result["hint"].as_str().unwrap().contains("kind `scenario`"));
}

#[tokio::test]
async fn cancels_a_run_and_returns_its_status() {
    let server = MockServer::start(vec![
        MockResponse::new(200),
        MockResponse::json(200, information_json("run-1", "canceled")),
    ])
    .await;
    let args = json!({ "workspace_id": "routing", "run_id": "run-1" });
    let result = call(context(&server, None), "decision_run_cancel", args)
        .await
        .unwrap();
    assert_eq!(
        sent(&server.requests()[..1]),
        ["PATCH /v1/applications/routing/runs/run-1/cancel"]
    );
    assert_eq!(
        result,
        json!({ "run_id": "run-1", "canceled": true, "status": "canceled" })
    );
}

#[tokio::test]
async fn deletes_a_resource_by_kind() {
    let server = MockServer::start(vec![MockResponse::new(200), MockResponse::new(200)]).await;
    let args = json!({ "kind": "input_set", "workspace_id": "routing", "id": "last-week" });
    let result = call(context(&server, None), "decision_delete", args)
        .await
        .unwrap();
    assert_eq!(
        result,
        json!({ "deleted": true, "kind": "input_set", "workspace_id": "routing", "id": "last-week" })
    );
    let args = json!({ "kind": "workspace", "workspace_id": "routing", "id": "" });
    let result = call(context(&server, None), "decision_delete", args)
        .await
        .unwrap();
    assert_eq!(
        result,
        json!({ "deleted": true, "kind": "workspace", "workspace_id": "routing" })
    );
    assert_eq!(
        sent(&server.requests()),
        [
            "DELETE /v1/applications/routing/experiments/inputsets/last-week",
            "DELETE /v1/applications/routing",
        ]
    );
    let args = json!({ "kind": "build", "workspace_id": "routing" });
    let reason = reason(call(context(&server, None), "decision_delete", args).await);
    assert_eq!(reason, "`id` is required to delete a build");
}

#[tokio::test]
async fn returns_the_guide() {
    let result = call(Context::new(), "decision_guide", json!({}))
        .await
        .unwrap();
    let guide = result["guide"].as_str().unwrap();
    assert!(guide.starts_with("# Decision workspace workflow"));
    // Deploying chains the operation tools, which the guide names, and every
    // `decision_*` tool it names exists.
    for operation in [
        "decision_platform_workspace_push",
        "decision_platform_build_create",
        "decision_platform_environment_create",
        "decision_platform_environment_update",
    ] {
        assert!(guide.contains(operation), "{operation}");
    }
    let catalog: BTreeSet<&str> = crate::decision::tools().iter().map(Tool::name).collect();
    let curated: BTreeSet<&str> = tools().iter().map(Tool::name).collect();
    for word in guide.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        if word.starts_with("decision_") && word.len() > "decision_".len() {
            assert!(
                curated.contains(word) || catalog.contains(word),
                "the guide names `{word}`, which is no tool"
            );
        }
    }
    // FICO names, and nothing out of scope.
    for word in [
        "app.yaml\n",
        "instance",
        "Nextmv Cloud",
        "statistics",
        "batch",
    ] {
        assert!(!guide.contains(word), "{word:?}");
    }
}

// The local tools.

/// A workspace whose entrypoint is the shell script `main`, which solves a
/// JSON input: it reads `vehicles`, logs to stderr, and prints its output
/// with metrics.
#[cfg(unix)]
struct Workspace {
    root: TempDir,
    src: PathBuf,
}

#[cfg(unix)]
impl Workspace {
    fn new(script: &str) -> Workspace {
        use std::os::unix::fs::PermissionsExt;

        let root = TempDir::new();
        let src = root.path().join("routing");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("workspace.yaml"), "type: binary\nfiles: [main]\n").unwrap();
        let main = src.join("main");
        std::fs::write(&main, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&main, std::fs::Permissions::from_mode(0o755)).unwrap();
        Workspace { root, src }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join(".fico")
    }

    fn context(&self) -> Context {
        Context::new().with_local(Local::with_home(self.home()))
    }
}

/// A workspace that prints a solution, its metrics and logs.
#[cfg(unix)]
const SOLVER: &str = r#"input="$(cat)"
vehicles=$(printf '%s' "$input" | tr -cd '0-9')
echo "solving for $vehicles vehicles" >&2
printf '{"solutions": [{"vehicles": %s}], "metrics": {"cost": %s.5}}' "$vehicles" "$vehicles""#;

#[cfg(unix)]
#[tokio::test]
async fn runs_a_workspace_locally_and_gets_its_parts() {
    let workspace = Workspace::new(SOLVER);
    let args = json!({
        "src": workspace.src,
        "input": { "vehicles": 2 },
        "include": ["output", "logs", "input"],
        "wait_seconds": 30,
    });
    let result = call(workspace.context(), "decision_local_run_create", args)
        .await
        .unwrap();
    assert_eq!(result["status"], "succeeded", "{result}");
    assert_eq!(result["workspace_registered"], true);
    assert_eq!(result["metrics"], json!({ "cost": 2.5 }));
    assert_eq!(result["output"]["solutions"], json!([{ "vehicles": 2 }]));
    assert_eq!(result["logs"], "solving for 2 vehicles\n");
    assert_eq!(result["input"], json!({ "vehicles": 2 }));
    let run_id = result["run_id"].as_str().unwrap().to_owned();
    assert!(run_id.starts_with("local-"), "{run_id}");

    let args = json!({ "src": workspace.src, "run_id": run_id });
    let got = call(workspace.context(), "decision_local_run_get", args)
        .await
        .unwrap();
    assert_eq!(got["status"], "succeeded");
    assert!(got.get("output").is_none());
    assert!(got["hint"].as_str().unwrap().contains("include"));

    let args = json!({ "src": workspace.src, "status": "succeeded" });
    let list = call(workspace.context(), "decision_local_run_list", args)
        .await
        .unwrap();
    assert_eq!(list["count"], 1);
    assert_eq!(list["items"][0]["run_id"], json!(run_id));

    // Run again from the run, with other options.
    let args = json!({
        "src": workspace.src, "from_run_id": run_id, "options": { "x": "1" }, "wait_seconds": 30,
    });
    let again = call(workspace.context(), "decision_local_run_create", args)
        .await
        .unwrap();
    assert_eq!(again["status"], "succeeded");
    assert_eq!(again["cloned_from_run_id"], json!(run_id));

    // Compare them.
    let run_ids = json!([run_id, again["run_id"]]);
    let args = json!({ "src": workspace.src, "run_ids": run_ids });
    let comparison = call(workspace.context(), "decision_run_compare", args)
        .await
        .unwrap();
    assert_eq!(comparison["run_ids"], run_ids);
    assert_eq!(comparison["metrics"]["cost"]["differ"], false);
    let clone_id = again["run_id"].as_str().unwrap();
    assert_eq!(
        comparison["options"]["x"]["values"],
        json!({ clone_id: "1" })
    );
    assert_eq!(
        comparison["other_differences"]["cloned_run_id"][clone_id],
        json!(run_id)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn returns_large_local_parts_as_their_files() {
    let workspace = Workspace::new(
        r#"cat > /dev/null
head -c 50000 /dev/zero | tr '\0' 'x' >&2
printf '{"solutions": []}'"#,
    );
    let args = json!({
        "src": workspace.src, "input": {}, "include": ["logs"], "wait_seconds": 30,
    });
    let result = call(workspace.context(), "decision_local_run_create", args)
        .await
        .unwrap();
    let run_id = result["run_id"].as_str().unwrap();
    let file = workspace
        .src
        .join(".fico/runs")
        .join(run_id)
        .join("logs/logs.log");
    assert_eq!(result["logs_file"], json!(file));
    assert_eq!(result["logs_bytes"], 50_000);
    // Local runs are never copied into the cache.
    assert!(!workspace.home().join("decision/cache").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn lists_the_local_runs_when_one_does_not_exist() {
    let workspace = Workspace::new(SOLVER);
    let args = json!({ "src": workspace.src, "run_id": "local-nope" });
    let first = reason(call(workspace.context(), "decision_local_run_get", args.clone()).await);
    assert_eq!(
        first,
        "local run `local-nope` does not exist. The workspace has no local runs."
    );
    let run = json!({ "src": workspace.src, "input": { "vehicles": 1 }, "wait_seconds": 30 });
    let created = call(workspace.context(), "decision_local_run_create", run)
        .await
        .unwrap();
    let run_id = created["run_id"].as_str().unwrap();
    let second = reason(call(workspace.context(), "decision_local_run_get", args).await);
    assert_eq!(
        second,
        format!(
            "local run `local-nope` does not exist. Its local runs, newest first: `{run_id}` \
             (succeeded)."
        )
    );
}

/// The platform's answers to tracking `runs` runs of JSON output with logs:
/// an upload and its presigned URL on `bucket` for the input, the output
/// and the logs, then the tracked run `tracked-<n>`.
fn track_responses(bucket: &MockServer, runs: usize) -> Vec<MockResponse> {
    let mut responses = Vec::new();
    for run in 0..runs {
        for part in 0..3 {
            let url = format!("{}/uploads/{run}-{part}?X-Amz-Signature=abc", bucket.url());
            let upload = json!({ "upload_id": format!("upload-{run}-{part}"), "upload_url": url });
            responses.push(MockResponse::json(200, upload));
        }
        responses.push(MockResponse::json(
            202,
            json!({ "run_id": format!("tracked-{run}") }),
        ));
    }
    responses
}

#[cfg(unix)]
#[tokio::test]
async fn syncs_local_runs_and_says_what_each_one_became() {
    let workspace = Workspace::new(SOLVER);
    let local = Local::with_home(workspace.home());
    let run = json!({ "src": workspace.src, "input": { "vehicles": 2 }, "wait_seconds": 30 });
    let cx = Context::new().with_local(local.clone());
    let created = call(cx, "decision_local_run_create", run).await.unwrap();
    let run_id = created["run_id"].as_str().unwrap().to_owned();
    let bucket = MockServer::start(vec![MockResponse::new(200); 3]).await;
    // The workspace, for its default environment, then the run.
    let mut responses = vec![MockResponse::json(
        200,
        json!({ "id": "routing", "default_instance": "production" }),
    )];
    responses.extend(track_responses(&bucket, 1));
    let server = MockServer::start(responses).await;
    let cx = Context::new()
        .with_platform(platform(&server))
        .with_local(local);
    let args = json!({ "src": workspace.src, "workspace_id": "routing", "environment_id": "" });
    let result = call(cx.clone(), "decision_local_workspace_sync", args.clone())
        .await
        .unwrap();
    assert_eq!(
        result,
        json!({
            "workspace_id": "routing",
            "counts": { "synced": 1 },
            "runs": [{ "run_id": run_id, "outcome": "synced", "platform_run_id": "tracked-0" }],
            "console_url": format!("{}/app/routing/runs", DEFAULT_CONSOLE_URL.unwrap()),
        })
    );
    let requests = server.requests();
    assert_eq!(
        sent(&requests),
        [
            "GET /v1/applications/routing",
            "POST /v1/applications/routing/runs/uploadurl",
            "POST /v1/applications/routing/runs/uploadurl",
            "POST /v1/applications/routing/runs/uploadurl",
            "POST /v1/applications/routing/runs?instance_id=production",
        ]
    );
    // A blank environment counts as absent: the workspace's default one.
    assert_eq!(bucket.requests().len(), 3);

    // A second sync sends nothing and says the run is already synced.
    let again = call(cx, "decision_local_workspace_sync", args)
        .await
        .unwrap();
    assert_eq!(again["counts"], json!({ "already_synced": 1 }));
    assert_eq!(again["runs"][0]["platform_run_id"], "tracked-0");
    assert_eq!(server.requests().len(), 5);
}

#[cfg(unix)]
#[tokio::test]
async fn lists_the_environments_when_a_sync_names_one_that_does_not_exist() {
    let workspace = Workspace::new(SOLVER);
    let local = Local::with_home(workspace.home());
    let run = json!({ "src": workspace.src, "input": { "vehicles": 1 }, "wait_seconds": 30 });
    let cx = Context::new().with_local(local.clone());
    call(cx, "decision_local_run_create", run).await.unwrap();
    let bucket = MockServer::start(vec![MockResponse::new(200); 3]).await;
    let mut responses = track_responses(&bucket, 1);
    responses.pop();
    responses.extend([
        // The tracked run, on an environment that does not exist.
        not_found("app instance with identifier routing not found"),
        MockResponse::json(200, json!({ "id": "routing" })),
        not_found("app instance with identifier routing not found"),
        page(vec![environment_json("production", "Production", "v1")]),
    ]);
    let server = MockServer::start(responses).await;
    let cx = Context::new()
        .with_platform(platform(&server))
        .with_local(local);
    let args = json!({ "src": workspace.src, "workspace_id": "routing", "environment_id": "prod" });
    let reason = reason(call(cx, "decision_local_workspace_sync", args).await);
    assert_eq!(
        reason,
        "environment `prod` does not exist in workspace `routing`. Its environments: \
         `production` (Production)."
    );
}

#[cfg(unix)]
#[tokio::test]
async fn validates_a_manifest() {
    let workspace = Workspace::new(SOLVER);
    let args = json!({ "src": workspace.src });
    let result = call(workspace.context(), "decision_manifest_validate", args)
        .await
        .unwrap();
    assert_eq!(result["valid"], true);
    assert_eq!(result["manifest"]["type"], "binary");

    std::fs::write(workspace.src.join("workspace.yaml"), "type: cobol\n").unwrap();
    let args = json!({ "src": workspace.src });
    let result = call(workspace.context(), "decision_manifest_validate", args)
        .await
        .unwrap();
    assert_eq!(result["valid"], false);
    assert!(!result["problem"].as_str().unwrap().is_empty());

    let args = json!({ "src": workspace.root.path() });
    let result = call(workspace.context(), "decision_manifest_validate", args)
        .await
        .unwrap();
    assert_eq!(result["valid"], false);
    assert!(
        result["hint"]
            .as_str()
            .unwrap()
            .contains("decision_manifest_init")
    );
}

#[tokio::test]
async fn needs_a_source_directory() {
    let home = TempDir::new();
    let cx = Context::new().with_local(Local::with_home(home.path()));
    for (name, args) in [
        ("decision_manifest_validate", json!({ "src": " " })),
        ("decision_local_run_list", json!({ "src": "" })),
        (
            "decision_local_run_get",
            json!({ "src": " ", "run_id": "local-a" }),
        ),
    ] {
        let result = tool(name).call(cx.clone(), args).await;
        assert_eq!(
            reason(result),
            "`src` is required and must not be empty",
            "{name}"
        );
    }
}

// Over the MCP transports.

#[tokio::test]
async fn advertises_and_serves_the_run_resources() {
    let options = SurfaceOptions::default().mode(Mode::Default);
    let mut client = client(&options, true).await;
    let response = client.request("resources/templates/list", json!({})).await;
    let templates = response["result"]["resourceTemplates"].as_array().unwrap();
    let uris: Vec<&str> = templates
        .iter()
        .map(|template| template["uriTemplate"].as_str().unwrap())
        .collect();
    assert_eq!(
        uris,
        [
            "fico://decision/workspaces/{workspace_id}/runs/{run_id}/output",
            "fico://decision/workspaces/{workspace_id}/runs/{run_id}/logs",
            "fico://decision/workspaces/{workspace_id}/runs/{run_id}/input",
        ]
    );
    // An unknown URI is the caller's error.
    let response = client
        .request("resources/read", json!({ "uri": "https://example.com" }))
        .await;
    assert_eq!(response["error"]["code"], -32602, "{response}");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not a resource of this server")
    );
}

#[tokio::test]
async fn reads_a_run_resource_with_the_platform() {
    let large = json!({ "solution": "y".repeat(MAX_INLINE_BYTES as usize) });
    let server = MockServer::start(vec![
        MockResponse::json(200, information_json("run-1", "succeeded")),
        MockResponse::json(200, information_json("run-1", "succeeded")),
        MockResponse::json(200, with_output("run-1", large.clone())),
    ])
    .await;
    let text = read_resource(&platform(&server), "routing", "run-1", Part::Output)
        .await
        .unwrap();
    let read: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(read, large);
    assert_eq!(
        sent(&server.requests()),
        [
            "GET /v1/applications/routing/runs/run-1/metadata",
            "GET /v1/applications/routing/runs/run-1/metadata",
            "GET /v1/applications/routing/runs/run-1",
        ]
    );
}

#[tokio::test]
async fn links_large_output_over_http_and_serves_it_with_the_callers_context() {
    let large = json!({ "solution": "z".repeat(MAX_INLINE_BYTES as usize) });
    let mut information = information_json("run-1", "succeeded");
    information["metadata"]["output_size"] = json!(MAX_INLINE_BYTES * 2);
    let api = MockServer::start(vec![
        // `decision_run_get`: the run and its environment, no output.
        MockResponse::json(200, information.clone()),
        MockResponse::json(200, environment_json("production", "Production", "v1")),
        // `resources/read`: the run's information, then its output.
        MockResponse::json(200, information.clone()),
        MockResponse::json(200, information.clone()),
        MockResponse::json(200, with_output("run-1", large.clone())),
    ])
    .await;
    // The provider builds the platform from the caller's token, and local
    // mode, which remote mode drops.
    let url = api.url().to_owned();
    let home = TempDir::new();
    let home_path = home.path().to_owned();
    let provider = move |info: crate::mcp::RequestInfo| {
        let token = info
            .authorization
            .unwrap_or_default()
            .trim_start_matches("Bearer ")
            .to_owned();
        let client = crate::Client::builder(&url)
            .bearer_token(token)
            .retry(RetryPolicy::none())
            .build();
        let local = Local::with_home(&home_path);
        async move {
            Ok(Context::new()
                .with_platform(Platform::new(client?))
                .with_local(local))
        }
    };
    let surface = crate::mcp::surface::Surface::new(&SurfaceOptions::default(), true);
    let server = crate::mcp::Server::new(provider, surface, true);
    let client =
        crate::mcp::tests::McpClient::http(server, crate::mcp::HttpOptions::default()).await;
    let crate::mcp::tests::McpClient::Http {
        client: http, url, ..
    } = &client
    else {
        unreachable!()
    };
    let headers = [("authorization", "Bearer caller-token")];
    let message = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {
            "name": "decision_run_get",
            "arguments": { "workspace_id": "routing", "run_id": "run-1", "include": ["output"] },
        },
    });
    let response = crate::mcp::tests::post(http, url, &message, &headers).await;
    let response: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    let result = &response["result"];
    let uri = "fico://decision/workspaces/routing/runs/run-1/output";
    assert_eq!(
        result["structuredContent"]["output_resource_link"], uri,
        "{response}"
    );
    assert_eq!(result["content"][1]["type"], "resource_link");
    assert_eq!(result["content"][1]["uri"], uri);
    assert_eq!(result["content"][1]["mimeType"], "application/json");
    // Remote mode wrote nothing into the FICO home.
    assert!(!home.path().join("decision").exists());

    let message = json!({
        "jsonrpc": "2.0", "id": 2, "method": "resources/read", "params": { "uri": uri },
    });
    let response = crate::mcp::tests::post(http, url, &message, &headers).await;
    let response: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    let contents = &response["result"]["contents"][0];
    assert_eq!(contents["uri"], uri, "{response}");
    assert_eq!(contents["mimeType"], "application/json");
    let read: Value = serde_json::from_str(contents["text"].as_str().unwrap()).unwrap();
    assert_eq!(read, large);
    // Every request went out with the caller's token.
    for request in api.requests() {
        assert_eq!(request.header("authorization"), Some("Bearer caller-token"));
    }
}

#[tokio::test]
async fn adds_a_resource_link_block_for_each_link() {
    let mut value = Map::new();
    cache::Placed::Link(cache::uri("routing", "run-1", Part::Logs), 50_000)
        .insert(&mut value, "logs");
    let result = crate::mcp::structured(Value::Object(value));
    let result = serde_json::to_value(result).unwrap();
    assert_eq!(result["content"][1]["type"], "resource_link");
    assert_eq!(
        result["content"][1]["uri"],
        "fico://decision/workspaces/routing/runs/run-1/logs"
    );
    assert_eq!(result["content"][1]["name"], "run-1 logs");
    // Logs are text, as `resources/read` serves them.
    assert_eq!(result["content"][1]["mimeType"], "text/plain");
}

#[tokio::test]
async fn reports_a_failed_call_as_an_error_result() {
    let options = SurfaceOptions::default().mode(Mode::Default);
    let mut client = client(&options, false).await;
    let result = client
        .call(
            "decision_run_get",
            json!({ "workspace_id": "", "run_id": "r" }),
        )
        .await;
    assert_error(&result, "`workspace_id` is required and must not be empty");
}
