//! Tests for the local run operations and their tools, against run
//! directories written to the run-directory layout (`local/run_dir.rs`) in
//! temporary workspaces, with a temporary FICO home.

mod mock_server;
mod support;

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fico_sdk::Error;
use fico_sdk::decision::local::{
    Local, RunCloneParams, RunCompareParams, RunCreateParams, RunGetParams, RunInputGetParams,
    RunListParams, RunLogsGetParams, WorkspaceRegisterParams,
};
use fico_sdk::decision::{ContentFormat, LocalWorkspaceRef, PollingOptions, RunStatus};
use fico_sdk::tool::{Context, ToolError};
use serde_json::{Value, json};
use support::TempDir;

/// The manifest of a workspace with JSON input and output.
const JSON_MANIFEST: &str = "files: [main.py]\n";

/// The manifest of a workspace with multi-file input and output.
const MULTI_FILE_MANIFEST: &str = "\
files: [main.py]
configuration:
  content:
    format: multi-file
";

/// A temporary directory with a FICO home, `.fico`, and a workspace
/// directory, `routing`, next to it.
struct Setup {
    root: TempDir,
    local: Local,
    src: PathBuf,
}

impl Setup {
    fn new(manifest: &str) -> Setup {
        let root = TempDir::new("local-run");
        let local = Local::with_home(root.path().join(".fico"));
        let src = root.path().join("routing");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("workspace.yaml"), manifest).unwrap();
        let src = src.canonicalize().unwrap();
        Setup { root, local, src }
    }

    /// The workspace, by its directory.
    fn workspace(&self) -> LocalWorkspaceRef {
        LocalWorkspaceRef {
            src: Some(self.src.clone()),
            ..LocalWorkspaceRef::default()
        }
    }

    /// The directory of the run `run_id`.
    fn run_dir(&self, run_id: &str) -> PathBuf {
        self.src.join(".fico").join("runs").join(run_id)
    }

    /// Writes `contents` to `relative` in the directory of the run
    /// `run_id`.
    fn write(&self, run_id: &str, relative: &str, contents: impl AsRef<[u8]>) {
        let path = self.run_dir(run_id).join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// Writes the information file of a run, as `information` returns it.
    fn run(&self, run_id: &str, status: &str, format: &str) {
        self.write_information(run_id, &information(run_id, status, format));
    }

    fn write_information(&self, run_id: &str, information: &Value) {
        let file = format!("{run_id}.json");
        self.write(
            run_id,
            &file,
            serde_json::to_vec_pretty(information).unwrap(),
        );
    }

    /// A temporary directory next to the workspace.
    fn dir(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn context(&self) -> Context {
        Context::new().with_local(self.local.clone())
    }
}

/// The information file of a local run, as `create_run` writes it and the
/// executor updates it: a run in `status`, with `format` input and output.
fn information(run_id: &str, status: &str, format: &str) -> Value {
    let error = match status {
        "failed" => "Run failed, please check logs for details.",
        _ => "",
    };
    json!({
        "description": "Local run created at 2026-10-13T15:00:00.123456Z",
        "id": run_id,
        "metadata": {
            "application_id": "routing",
            "application_instance_id": "",
            "application_version_id": "",
            "created_at": "2026-10-13T15:00:00.123456Z",
            "duration": 1250.5,
            "error": error,
            "execution_class": "local",
            "execution_duration": 1250.5,
            "experiment_id": "",
            "experiment_type": "",
            "format": { "input": { "type": format }, "output": { "type": format } },
            "initiated_at": "2026-10-13T15:00:00.123456Z",
            "input_size": 18.0,
            "options": {
                "active_options": { "duration": "10s" },
                "options_summary": [{ "name": "duration", "source": "run", "value": "10s" }],
                "request_options": { "duration": "10s" }
            },
            "output_size": 0.0,
            "queuing_disabled": true,
            "queuing_priority": 0,
            "run_type": { "type": "standard", "definition_id": "", "reference_id": "" },
            "runtime": "local",
            "secrets_collection_id": "",
            "status_v2": status,
            "tracking": { "cloned_run_id": "", "input_id": "" }
        },
        "name": format!("local run {run_id}"),
        "user_email": "",
        "console_url": ""
    })
}

/// Polling options that check every 10 ms, with at most `max_tries`
/// checks.
fn fast(max_tries: Option<u32>) -> PollingOptions {
    PollingOptions {
        backoff: 0.0,
        delay: 0.01,
        initial_delay: 0.0,
        max_delay: 0.01,
        max_duration: None,
        max_tries,
        jitter: 0.0,
    }
}

/// Replaces the information file of the run `run_id` in `dir` with one in
/// `status` after `delay`, atomically, as the executor does when the run
/// ends.
fn finish_later(dir: PathBuf, run_id: &'static str, status: &'static str, delay: Duration) {
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        let file = dir.join(format!("{run_id}.json"));
        let temporary = dir.join(format!("{run_id}.json.tmp"));
        let information = information(run_id, status, "json");
        std::fs::write(&temporary, serde_json::to_vec(&information).unwrap()).unwrap();
        std::fs::rename(temporary, file).unwrap();
    });
}

fn assert_not_found(err: Error, run_id: &str) {
    match err {
        Error::NotFound { kind, id, .. } => {
            assert_eq!(kind, "local run");
            assert_eq!(id, run_id);
        }
        other => panic!("expected a missing run, got {other:?}"),
    }
}

fn assert_invalid_data(err: Error, file: &Path) {
    match err {
        Error::Io { path, source, .. } => {
            assert_eq!(path, file);
            assert_eq!(source.kind(), io::ErrorKind::InvalidData);
        }
        other => panic!("expected invalid data in {}, got {other:?}", file.display()),
    }
}

#[tokio::test]
async fn gets_the_information_of_runs_in_each_status() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-succeeded", "succeeded", "json");
    setup.write(
        "local-succeeded",
        "outputs/metrics.json",
        r#"{"metrics": {"cost": 12.5, "optimal": true}}"#,
    );
    setup.run("local-failed", "failed", "json");
    setup.run("local-running", "running", "json");
    let workspace = setup.workspace();

    let info = setup
        .local
        .get_run_information(&workspace, "local-succeeded")
        .await
        .unwrap();
    assert_eq!(info.id, "local-succeeded");
    assert_eq!(info.name, "local run local-succeeded");
    assert_eq!(info.metadata.status, RunStatus::Succeeded);
    assert_eq!(info.metadata.workspace_id, "routing");
    let metrics = info.metadata.metrics.as_ref().unwrap();
    assert_eq!(metrics["cost"], json!(12.5));
    assert_eq!(metrics["optimal"], json!(true));
    // Emitted JSON is the file, with API names, plus the metrics.
    let mut expected = information("local-succeeded", "succeeded", "json");
    expected["metadata"]["metrics"] = json!({ "cost": 12.5, "optimal": true });
    assert_eq!(serde_json::to_value(&info).unwrap(), expected);

    let info = setup
        .local
        .get_run_information(&workspace, "local-failed")
        .await
        .unwrap();
    assert_eq!(info.metadata.status, RunStatus::Failed);
    assert_eq!(
        info.metadata.error,
        "Run failed, please check logs for details."
    );
    assert_eq!(info.metadata.metrics, None);

    let info = setup
        .local
        .get_run_information(&workspace, "local-running")
        .await
        .unwrap();
    assert_eq!(info.metadata.status, RunStatus::Running);
}

#[tokio::test]
async fn reads_metrics_without_the_metrics_key() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "succeeded", "json");
    setup.write("local-a", "outputs/metrics.json", r#"{"cost": 3}"#);
    let info = setup
        .local
        .get_run_information(&setup.workspace(), "local-a")
        .await
        .unwrap();
    assert_eq!(info.metadata.metrics.unwrap()["cost"], json!(3));
}

#[tokio::test]
async fn leaves_out_metrics_that_are_not_an_object() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "succeeded", "json");
    setup.write("local-a", "outputs/solutions/solution.json", "{}");
    let workspace = setup.workspace();
    // `{"metrics": null}` is what executors write for a workspace that
    // printed `"metrics": null`.
    for metrics in [
        r#"{"metrics": null}"#,
        r#"{"metrics": [1, 2]}"#,
        "[1, 2]",
        "12",
    ] {
        setup.write("local-a", "outputs/metrics.json", metrics);
        let info = setup
            .local
            .get_run_information(&workspace, "local-a")
            .await
            .unwrap();
        assert_eq!(info.metadata.metrics, None, "{metrics}");
        let result = setup
            .local
            .get_run(&workspace, "local-a", RunGetParams::default())
            .await
            .unwrap();
        assert_eq!(result.information.metadata.status, RunStatus::Succeeded);
        assert_eq!(result.information.metadata.metrics, None, "{metrics}");
        assert_eq!(result.output, Some(serde_json::Map::new()));
    }
}

#[tokio::test]
async fn rejects_metrics_that_are_not_json() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "succeeded", "json");
    setup.write("local-a", "outputs/metrics.json", "{");
    let err = setup
        .local
        .get_run_information(&setup.workspace(), "local-a")
        .await
        .unwrap_err();
    let file = setup.run_dir("local-a").join("outputs/metrics.json");
    assert_invalid_data(err, &file);
}

#[tokio::test]
async fn gets_a_succeeded_run_with_its_json_output() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "succeeded", "json");
    setup.write(
        "local-a",
        "outputs/solutions/solution.json",
        r#"{"solutions": [{"routes": []}], "metrics": {"cost": 12.5}}"#,
    );
    setup.write(
        "local-a",
        "outputs/metrics.json",
        r#"{"metrics": {"cost": 12.5}}"#,
    );
    let result = setup
        .local
        .get_run(&setup.workspace(), "local-a", RunGetParams::default())
        .await
        .unwrap();
    assert_eq!(result.information.metadata.status, RunStatus::Succeeded);
    assert_eq!(
        result.information.metadata.metrics.as_ref().unwrap()["cost"],
        json!(12.5)
    );
    let output = result.output.as_ref().unwrap();
    assert_eq!(output["solutions"], json!([{ "routes": [] }]));
    assert_eq!(result.error_log, None);
    assert_eq!(result.ensemble, None);
    let emitted = serde_json::to_value(&result).unwrap();
    assert_eq!(emitted["id"], "local-a");
    assert_eq!(emitted["metadata"]["status_v2"], "succeeded");
    assert_eq!(emitted["output"]["metrics"]["cost"], json!(12.5));
}

#[tokio::test]
async fn gets_a_failed_run_with_its_error_and_without_output() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "failed", "json");
    setup.write("local-a", "outputs/solutions/solution.json", "{}");
    let result = setup
        .local
        .get_run(&setup.workspace(), "local-a", RunGetParams::default())
        .await
        .unwrap();
    assert_eq!(result.information.metadata.status, RunStatus::Failed);
    let error_log = result.error_log.unwrap();
    assert_eq!(
        error_log.error.as_deref(),
        Some("Run failed, please check logs for details.")
    );
    assert_eq!(error_log.stdout, None);
    assert_eq!(error_log.stderr, None);
    assert_eq!(result.output, None);
}

#[tokio::test]
async fn gets_a_running_run_without_output() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "running", "json");
    let result = setup
        .local
        .get_run(&setup.workspace(), "local-a", RunGetParams::default())
        .await
        .unwrap();
    assert_eq!(result.information.metadata.status, RunStatus::Running);
    assert_eq!(result.output, None);
    assert_eq!(result.error_log, None);
}

/// Leaves the run `run_id` `running` with a workspace that exited with
/// `exit_code` and printed `stdout`, as a host that stopped before it could
/// finalize the run leaves it.
fn exited(setup: &Setup, run_id: &str, exit_code: i32, stdout: &str) {
    setup.run(run_id, "running", "json");
    setup.write(run_id, "outputs/solutions/solution.json", stdout);
    let exit = json!({ "exit_code": exit_code, "ended_at": "2026-10-13T15:00:02.500000Z" });
    setup.write(run_id, "exit.json", exit.to_string());
}

#[tokio::test]
async fn finalizes_a_run_whose_workspace_exited_on_every_read() {
    let setup = Setup::new(JSON_MANIFEST);
    let stdout = r#"{"solutions": [], "metrics": {"cost": 7}}"#;
    let workspace = setup.workspace();
    for (run_id, exit_code, status) in [
        ("local-a", 0, RunStatus::Succeeded),
        ("local-b", 3, RunStatus::Failed),
    ] {
        exited(&setup, run_id, exit_code, stdout);
        let information = setup
            .local
            .get_run_information(&workspace, run_id)
            .await
            .unwrap();
        assert_eq!(information.metadata.status, status);
        // From `created_at`, 15:00:00.123456, to the exit.
        assert_eq!(information.metadata.duration, 2376.5);
        let metrics = information.metadata.metrics.unwrap();
        assert_eq!(metrics["cost"], json!(7));
    }

    // Each read finalizes: get, input, logs and list too.
    exited(&setup, "local-c", 0, stdout);
    let result = setup
        .local
        .get_run(&workspace, "local-c", RunGetParams::default())
        .await
        .unwrap();
    assert_eq!(result.information.metadata.status, RunStatus::Succeeded);
    assert_eq!(result.output.unwrap()["solutions"], json!([]));
    exited(&setup, "local-d", 0, stdout);
    let params = RunInputGetParams::default();
    let _ = setup
        .local
        .get_run_input(&workspace, "local-d", params)
        .await;
    exited(&setup, "local-e", 0, stdout);
    let params = RunLogsGetParams::default();
    setup
        .local
        .get_run_logs(&workspace, "local-e", params)
        .await
        .unwrap();
    exited(&setup, "local-f", 1, stdout);
    let runs = setup
        .local
        .list_runs(&workspace, RunListParams::default())
        .await
        .unwrap();
    assert_eq!(runs.len(), 6);
    let file = |run_id: &str| -> Value {
        let path = setup.run_dir(run_id).join(format!("{run_id}.json"));
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    };
    for (run_id, status) in [
        ("local-d", "succeeded"),
        ("local-e", "succeeded"),
        ("local-f", "failed"),
    ] {
        assert_eq!(file(run_id)["metadata"]["status_v2"], status, "{run_id}");
    }

    // A run whose workspace has not exited stays as it is.
    setup.run("local-g", "running", "json");
    let information = setup
        .local
        .get_run_information(&workspace, "local-g")
        .await
        .unwrap();
    assert_eq!(information.metadata.status, RunStatus::Running);
}

#[tokio::test]
async fn polls_a_run_that_finalizes_on_read() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "running", "json");
    let dir = setup.run_dir("local-a");
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let exit = json!({ "exit_code": 0, "ended_at": "2026-10-13T15:00:02.500000Z" });
        std::fs::write(dir.join("exit.json"), exit.to_string()).unwrap();
    });
    let params = RunGetParams {
        polling: Some(fast(Some(500))),
        ..RunGetParams::default()
    };
    let result = setup
        .local
        .get_run(&setup.workspace(), "local-a", params)
        .await
        .unwrap();
    assert_eq!(result.information.metadata.status, RunStatus::Succeeded);
}

#[tokio::test]
async fn gets_a_succeeded_run_that_wrote_no_output() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "succeeded", "json");
    let result = setup
        .local
        .get_run(&setup.workspace(), "local-a", RunGetParams::default())
        .await
        .unwrap();
    assert_eq!(result.output, None);
}

#[tokio::test]
async fn rejects_json_output_that_is_not_an_object() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "succeeded", "json");
    let file = setup
        .run_dir("local-a")
        .join("outputs/solutions/solution.json");
    for output in ["[1, 2]", "routes"] {
        setup.write("local-a", "outputs/solutions/solution.json", output);
        let err = setup
            .local
            .get_run(&setup.workspace(), "local-a", RunGetParams::default())
            .await
            .unwrap_err();
        assert_invalid_data(err, &file);
    }
}

#[tokio::test]
async fn copies_multi_file_output_into_output_dir() {
    let setup = Setup::new(MULTI_FILE_MANIFEST);
    setup.run("local-a", "succeeded", "multi-file");
    setup.write("local-a", "outputs/solutions/routes.csv", "vehicle,stop\n");
    setup.write("local-a", "outputs/solutions/maps/day1.json", "{}");
    let workspace = setup.workspace();

    // Without `output_dir`, the files stay in the run.
    let result = setup
        .local
        .get_run(&workspace, "local-a", RunGetParams::default())
        .await
        .unwrap();
    assert_eq!(result.information.metadata.status, RunStatus::Succeeded);
    assert_eq!(result.output, None);

    let output_dir = setup.dir("output");
    let params = RunGetParams {
        output_dir: Some(output_dir.clone()),
        ..RunGetParams::default()
    };
    let result = setup
        .local
        .get_run(&workspace, "local-a", params)
        .await
        .unwrap();
    assert_eq!(result.output, None);
    let format = result.information.metadata.format.format_output.unwrap();
    assert_eq!(format.output_type, ContentFormat::MultiFile);
    let read = |name: &str| std::fs::read_to_string(output_dir.join(name)).unwrap();
    assert_eq!(read("routes.csv"), "vehicle,stop\n");
    assert_eq!(read("maps/day1.json"), "{}");
}

#[tokio::test]
async fn writes_nothing_into_output_dir_for_json_or_unfinished_runs() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-json", "succeeded", "json");
    setup.write("local-json", "outputs/solutions/solution.json", "{}");
    setup.run("local-running", "running", "multi-file");
    setup.write("local-running", "outputs/solutions/routes.csv", "partial");
    let output_dir = setup.dir("output");
    for run_id in ["local-json", "local-running"] {
        let params = RunGetParams {
            output_dir: Some(output_dir.clone()),
            ..RunGetParams::default()
        };
        setup
            .local
            .get_run(&setup.workspace(), run_id, params)
            .await
            .unwrap();
    }
    assert!(!output_dir.exists());
}

#[tokio::test]
async fn reads_formats_it_does_not_know_as_an_error() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "succeeded", "csv-archive");
    let workspace = setup.workspace();
    let err = setup
        .local
        .get_run(&workspace, "local-a", RunGetParams::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
    assert!(err.to_string().contains("output format"), "{err}");
    let err = setup
        .local
        .get_run_input(&workspace, "local-a", RunInputGetParams::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("input format"), "{err}");
}

#[tokio::test]
async fn polls_until_the_run_succeeds() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "running", "json");
    setup.write(
        "local-a",
        "outputs/solutions/solution.json",
        r#"{"solutions": []}"#,
    );
    finish_later(
        setup.run_dir("local-a"),
        "local-a",
        "succeeded",
        Duration::from_millis(100),
    );
    let params = RunGetParams {
        polling: Some(fast(None)),
        ..RunGetParams::default()
    };
    let result = setup
        .local
        .get_run(&setup.workspace(), "local-a", params)
        .await
        .unwrap();
    assert_eq!(result.information.metadata.status, RunStatus::Succeeded);
    assert_eq!(result.output.unwrap()["solutions"], json!([]));
}

#[tokio::test]
async fn stops_polling_when_it_runs_out_of_tries() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "running", "json");
    let workspace = setup.workspace();
    let params = RunGetParams {
        polling: Some(fast(Some(3))),
        ..RunGetParams::default()
    };
    let err = setup
        .local
        .get_run(&workspace, "local-a", params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Wait { .. }), "{err:?}");
    let params = RunLogsGetParams {
        polling: Some(fast(Some(3))),
    };
    let err = setup
        .local
        .get_run_logs(&workspace, "local-a", params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Wait { .. }), "{err:?}");
}

#[tokio::test]
async fn gets_the_logs() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "succeeded", "json");
    setup.write("local-a", "logs/logs.log", b"solving\n\xffdone\n");
    setup.run("local-b", "running", "json");
    let workspace = setup.workspace();
    let logs = setup
        .local
        .get_run_logs(&workspace, "local-a", RunLogsGetParams::default())
        .await
        .unwrap();
    // Bytes that are not UTF-8 are replaced.
    assert_eq!(logs.log, "solving\n\u{fffd}done\n");
    assert_eq!(
        serde_json::to_value(&logs).unwrap(),
        json!({ "log": "solving\n\u{fffd}done\n" })
    );
    let logs = setup
        .local
        .get_run_logs(&workspace, "local-b", RunLogsGetParams::default())
        .await
        .unwrap();
    assert_eq!(logs.log, "");
}

#[tokio::test]
async fn polls_the_logs_until_the_run_ends() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "running", "json");
    setup.write("local-a", "logs/logs.log", "infeasible\n");
    finish_later(
        setup.run_dir("local-a"),
        "local-a",
        "failed",
        Duration::from_millis(100),
    );
    let params = RunLogsGetParams {
        polling: Some(fast(None)),
    };
    let logs = setup
        .local
        .get_run_logs(&setup.workspace(), "local-a", params)
        .await
        .unwrap();
    assert_eq!(logs.log, "infeasible\n");
    let info = setup
        .local
        .get_run_information(&setup.workspace(), "local-a")
        .await
        .unwrap();
    assert_eq!(info.metadata.status, RunStatus::Failed);
}

#[tokio::test]
async fn gets_json_input() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "succeeded", "json");
    setup.write("local-a", "inputs/input.json", r#"{"vehicles": [1, 2]}"#);
    setup.run("local-b", "queued", "json");
    let workspace = setup.workspace();
    let input = setup
        .local
        .get_run_input(&workspace, "local-a", RunInputGetParams::default())
        .await
        .unwrap();
    assert_eq!(input, Some(json!({ "vehicles": [1, 2] })));
    // A run whose input was never written has none.
    let input = setup
        .local
        .get_run_input(&workspace, "local-b", RunInputGetParams::default())
        .await
        .unwrap();
    assert_eq!(input, None);
}

#[tokio::test]
async fn copies_multi_file_input_into_output_dir() {
    let setup = Setup::new(MULTI_FILE_MANIFEST);
    setup.run("local-a", "succeeded", "multi-file");
    setup.write("local-a", "inputs/stops.csv", "id\n1\n");
    setup.write("local-a", "inputs/fleet/vehicles.csv", "id\nv1\n");
    let output_dir = setup.dir("input");
    std::fs::create_dir_all(&output_dir).unwrap();
    std::fs::write(output_dir.join("stops.csv"), "old").unwrap();
    std::fs::write(output_dir.join("notes.txt"), "kept").unwrap();
    let params = RunInputGetParams {
        output_dir: Some(output_dir.clone()),
    };
    let input = setup
        .local
        .get_run_input(&setup.workspace(), "local-a", params)
        .await
        .unwrap();
    assert_eq!(input, None);
    let read = |name: &str| std::fs::read_to_string(output_dir.join(name)).unwrap();
    assert_eq!(read("stops.csv"), "id\n1\n");
    assert_eq!(read("fleet/vehicles.csv"), "id\nv1\n");
    assert_eq!(read("notes.txt"), "kept");
}

#[tokio::test]
async fn needs_output_dir_for_multi_file_input() {
    let setup = Setup::new(MULTI_FILE_MANIFEST);
    setup.run("local-a", "succeeded", "multi-file");
    setup.write("local-a", "inputs/stops.csv", "id\n1\n");
    let err = setup
        .local
        .get_run_input(&setup.workspace(), "local-a", RunInputGetParams::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
    assert!(err.to_string().contains("`output_dir`"), "{err}");
}

#[tokio::test]
async fn never_copies_input_into_itself() {
    let setup = Setup::new(MULTI_FILE_MANIFEST);
    setup.run("local-a", "succeeded", "multi-file");
    setup.write("local-a", "inputs/stops.csv", "id\n1\n");
    let inputs = setup.run_dir("local-a").join("inputs");
    for output_dir in [inputs.clone(), inputs.join("copy").join("deeper")] {
        let params = RunInputGetParams {
            output_dir: Some(output_dir),
        };
        let err = setup
            .local
            .get_run_input(&setup.workspace(), "local-a", params)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
    }
    // A refused copy creates nothing.
    assert!(!inputs.join("copy").exists());
    let stops = std::fs::read_to_string(inputs.join("stops.csv")).unwrap();
    assert_eq!(stops, "id\n1\n");
}

#[tokio::test]
async fn never_copies_output_into_itself() {
    let setup = Setup::new(MULTI_FILE_MANIFEST);
    setup.run("local-a", "succeeded", "multi-file");
    setup.write("local-a", "outputs/solutions/routes.csv", "vehicle,stop\n");
    let solutions = setup.run_dir("local-a").join("outputs/solutions");
    // Through a path with `..`, which still ends up inside the output.
    let output_dir = solutions.join("..").join("solutions").join("copy");
    let params = RunGetParams {
        output_dir: Some(output_dir),
        ..RunGetParams::default()
    };
    let err = setup
        .local
        .get_run(&setup.workspace(), "local-a", params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
    assert!(!solutions.join("copy").exists());
}

#[tokio::test]
async fn takes_an_empty_output_dir_as_unset() {
    let setup = Setup::new(MULTI_FILE_MANIFEST);
    setup.run("local-a", "succeeded", "multi-file");
    setup.write("local-a", "inputs/stops.csv", "id\n1\n");
    setup.write("local-a", "outputs/solutions/routes.csv", "vehicle,stop\n");
    let workspace = setup.workspace();
    let params = RunGetParams {
        output_dir: Some(PathBuf::new()),
        ..RunGetParams::default()
    };
    let result = setup.local.get_run(&workspace, "local-a", params).await;
    assert_eq!(result.unwrap().output, None);
    let params = RunInputGetParams {
        output_dir: Some(PathBuf::new()),
    };
    let err = setup
        .local
        .get_run_input(&workspace, "local-a", params)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("`output_dir`"), "{err}");
}

#[tokio::test]
async fn reports_a_missing_run_as_not_found() {
    let setup = Setup::new(JSON_MANIFEST);
    let workspace = setup.workspace();
    let local = &setup.local;
    let run_id = "local-missing";
    assert_not_found(
        local
            .get_run_information(&workspace, run_id)
            .await
            .unwrap_err(),
        run_id,
    );
    assert_not_found(
        local
            .get_run(&workspace, run_id, RunGetParams::default())
            .await
            .unwrap_err(),
        run_id,
    );
    assert_not_found(
        local
            .get_run_input(&workspace, run_id, RunInputGetParams::default())
            .await
            .unwrap_err(),
        run_id,
    );
    assert_not_found(
        local
            .get_run_logs(&workspace, run_id, RunLogsGetParams::default())
            .await
            .unwrap_err(),
        run_id,
    );
    // A run directory without its information file is no run either.
    std::fs::create_dir_all(setup.run_dir("local-empty")).unwrap();
    assert_not_found(
        local
            .get_run_information(&workspace, "local-empty")
            .await
            .unwrap_err(),
        "local-empty",
    );
}

#[tokio::test]
async fn rejects_run_ids_that_leave_the_runs_directory() {
    let setup = Setup::new(JSON_MANIFEST);
    for run_id in ["", "..", "../routing", "a/b"] {
        let err = setup
            .local
            .get_run_information(&setup.workspace(), run_id)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{run_id}: {err:?}");
    }
}

#[tokio::test]
async fn reports_a_corrupt_information_file() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.write("local-a", "local-a.json", r#"{"id": "local-a"}"#);
    let err = setup
        .local
        .get_run_information(&setup.workspace(), "local-a")
        .await
        .unwrap_err();
    assert_invalid_data(err, &setup.run_dir("local-a").join("local-a.json"));
}

#[tokio::test]
async fn needs_a_workspace_manifest() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "succeeded", "json");
    let manifest = setup.src.join("workspace.yaml");
    std::fs::remove_file(&manifest).unwrap();
    let err = setup
        .local
        .get_run_information(&setup.workspace(), "local-a")
        .await
        .unwrap_err();
    match err {
        Error::Io { path, source, .. } => {
            assert_eq!(path, manifest);
            assert_eq!(source.kind(), io::ErrorKind::NotFound);
        }
        other => panic!("expected a missing manifest, got {other:?}"),
    }
}

#[tokio::test]
async fn finds_a_registered_workspace_by_its_id_alone() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "succeeded", "json");
    let params = WorkspaceRegisterParams {
        src: setup.src.clone(),
        workspace_id: Some("routing".to_owned()),
        description: None,
    };
    setup.local.register_workspace(params).await.unwrap();
    let by_id = LocalWorkspaceRef {
        workspace_id: Some("routing".to_owned()),
        ..LocalWorkspaceRef::default()
    };
    let info = setup
        .local
        .get_run_information(&by_id, "local-a")
        .await
        .unwrap();
    assert_eq!(info.id, "local-a");

    let unknown = LocalWorkspaceRef {
        workspace_id: Some("dispatch".to_owned()),
        ..LocalWorkspaceRef::default()
    };
    let err = setup
        .local
        .get_run_information(&unknown, "local-a")
        .await
        .unwrap_err();
    match err {
        Error::NotFound { kind, .. } => assert_eq!(kind, "local workspace"),
        other => panic!("expected a missing workspace, got {other:?}"),
    }
    let err = setup
        .local
        .get_run_information(&LocalWorkspaceRef::default(), "local-a")
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
}

#[tokio::test]
async fn reads_by_src_without_registering_the_workspace() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "succeeded", "json");
    // A path with `..` names the same workspace.
    let workspace = LocalWorkspaceRef {
        src: Some(setup.src.join("..").join("routing")),
        ..LocalWorkspaceRef::default()
    };
    let workspace_id = setup
        .local
        .get_run_information(&workspace, "local-a")
        .await
        .unwrap()
        .metadata
        .workspace_id;
    assert_eq!(workspace_id, "routing");
    let registered = setup
        .local
        .workspace_registered(&setup.workspace())
        .await
        .unwrap();
    assert!(!registered);
}

/// Writes a run in `status`, created at `created_at`.
fn run_created_at(setup: &Setup, run_id: &str, status: &str, created_at: &str) {
    let mut information = information(run_id, status, "json");
    information["metadata"]["created_at"] = json!(created_at);
    setup.write_information(run_id, &information);
}

#[tokio::test]
async fn lists_runs_newest_first() {
    let setup = Setup::new(JSON_MANIFEST);
    run_created_at(&setup, "local-b", "running", "2026-10-13T15:00:00.000002Z");
    run_created_at(
        &setup,
        "local-a",
        "succeeded",
        "2026-10-13T15:00:00.000001Z",
    );
    run_created_at(&setup, "local-c", "failed", "2026-10-14T09:00:00.000000Z");
    let runs = setup
        .local
        .list_runs(&setup.workspace(), RunListParams::default())
        .await
        .unwrap();
    let ids: Vec<&str> = runs.iter().map(|run| run.id.as_str()).collect();
    assert_eq!(ids, ["local-c", "local-b", "local-a"]);
    let statuses: Vec<RunStatus> = runs.iter().map(|run| run.status).collect();
    assert_eq!(
        statuses,
        [RunStatus::Failed, RunStatus::Running, RunStatus::Succeeded]
    );
    // A listed run has the list fields of the information, with an empty
    // run type, execution class and runtime.
    assert_eq!(
        serde_json::to_value(&runs[2]).unwrap(),
        json!({
            "id": "local-a",
            "user_email": "",
            "name": "local run local-a",
            "description": "Local run created at 2026-10-13T15:00:00.123456Z",
            "created_at": "2026-10-13T15:00:00.000001Z",
            "application_id": "routing",
            "application_instance_id": "",
            "application_version_id": "",
            "run_type": {},
            "execution_class": "",
            "runtime": "",
            "status_v2": "succeeded"
        })
    );
}

#[tokio::test]
async fn lists_runs_with_a_status() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "succeeded", "json");
    setup.run("local-b", "failed", "json");
    setup.run("local-c", "succeeded", "json");
    let params = RunListParams {
        status: Some(RunStatus::Succeeded),
    };
    let runs = setup
        .local
        .list_runs(&setup.workspace(), params)
        .await
        .unwrap();
    let ids: Vec<&str> = runs.iter().map(|run| run.id.as_str()).collect();
    assert_eq!(ids, ["local-a", "local-c"]);
    let params = RunListParams {
        status: Some(RunStatus::Canceled),
    };
    let runs = setup.local.list_runs(&setup.workspace(), params).await;
    assert!(runs.unwrap().is_empty());
}

#[tokio::test]
async fn lists_no_runs_for_a_workspace_without_runs() {
    let setup = Setup::new(JSON_MANIFEST);
    let runs = setup
        .local
        .list_runs(&setup.workspace(), RunListParams::default())
        .await
        .unwrap();
    assert!(runs.is_empty());
    assert!(!setup.src.join(".fico").exists());
}

#[tokio::test]
async fn skips_what_is_not_a_run() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "succeeded", "json");
    let runs_dir = setup.src.join(".fico").join("runs");
    // A file, a directory without information, corrupt information, and a
    // directory whose name is no run ID.
    std::fs::write(runs_dir.join("notes.txt"), "x").unwrap();
    std::fs::create_dir_all(runs_dir.join("local-empty")).unwrap();
    setup.write("local-corrupt", "local-corrupt.json", "{");
    std::fs::create_dir_all(runs_dir.join("not a run")).unwrap();
    let runs = setup
        .local
        .list_runs(&setup.workspace(), RunListParams::default())
        .await
        .unwrap();
    let ids: Vec<&str> = runs.iter().map(|run| run.id.as_str()).collect();
    assert_eq!(ids, ["local-a"]);
}

#[tokio::test]
async fn lists_a_run_that_cannot_be_finalized() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "running", "json");
    setup.write("local-a", "exit.json", "{ not json");
    let workspace = setup.workspace();
    let runs = setup
        .local
        .list_runs(&workspace, RunListParams::default())
        .await
        .unwrap();
    let listed: Vec<(&str, RunStatus)> = runs
        .iter()
        .map(|run| (run.id.as_str(), run.status))
        .collect();
    assert_eq!(listed, [("local-a", RunStatus::Running)]);
    // A get says why.
    let err = setup
        .local
        .get_run_information(&workspace, "local-a")
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
}

#[tokio::test]
async fn creates_a_run_up_to_the_executor() {
    let setup = Setup::new(JSON_MANIFEST);
    let params = RunCreateParams {
        input: Some(json!({ "vehicles": 2 })),
        name: Some("Monday".to_owned()),
        options: [("duration".to_owned(), "10s".to_owned())].into(),
        ..RunCreateParams::default()
    };
    // The manifest lists a `main.py` that does not exist, so the executor
    // fails the call after the hand-off without starting anything. The
    // run's status depends on the executor, so it is not checked.
    let result = setup.local.create_run(&setup.workspace(), params).await;
    assert!(result.is_err(), "{result:?}");

    let entry = setup.local.get_workspace(&setup.workspace()).await.unwrap();
    let fico = setup.src.join(".fico");
    assert!(fico.join(".gitignore").is_file());
    let runs: Vec<String> = std::fs::read_dir(fico.join("runs"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    let [run_id] = runs.as_slice() else {
        panic!("expected one run, got {runs:?}");
    };
    assert!(run_id.starts_with("local-"), "{run_id}");
    let dir = setup.run_dir(run_id);
    let input = std::fs::read(dir.join("inputs/input.json")).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&input).unwrap(),
        json!({ "vehicles": 2 })
    );
    let file = std::fs::read(dir.join(format!("{run_id}.json"))).unwrap();
    let information: Value = serde_json::from_slice(&file).unwrap();
    assert_eq!(information["id"], json!(run_id));
    assert_eq!(information["name"], "Monday");
    let metadata = &information["metadata"];
    assert_eq!(metadata["application_id"], json!(entry.workspace_id));
    assert_eq!(metadata["input_size"], json!(input.len() as f64));
    assert_eq!(
        metadata["options"]["request_options"],
        json!({ "duration": "10s" })
    );
    assert_eq!(
        metadata["format"],
        json!({ "input": { "type": "json" }, "output": { "type": "json" } })
    );
}

#[tokio::test]
async fn checks_the_input_before_creating_the_run() {
    let setup = Setup::new(JSON_MANIFEST);
    let inputs = setup.dir("inputs");
    std::fs::create_dir_all(&inputs).unwrap();
    std::fs::write(inputs.join("stops.csv"), "id\n1\n").unwrap();
    let input = || Some(json!({ "vehicles": 2 }));
    let invalid = [
        RunCreateParams::default(),
        RunCreateParams {
            input: input(),
            input_dir: Some(inputs.clone()),
            ..RunCreateParams::default()
        },
        RunCreateParams {
            input: Some(json!([{ "id": "v1" }])),
            ..RunCreateParams::default()
        },
        RunCreateParams {
            input: input(),
            input_file: Some(inputs.join("stops.csv")),
            ..RunCreateParams::default()
        },
        // An input file holds JSON.
        RunCreateParams {
            input_file: Some(inputs.join("stops.csv")),
            ..RunCreateParams::default()
        },
        // The workspace's input is JSON.
        RunCreateParams {
            input_dir: Some(inputs.clone()),
            ..RunCreateParams::default()
        },
        RunCreateParams {
            input: input(),
            content_format: Some(ContentFormat::MultiFile),
            ..RunCreateParams::default()
        },
        RunCreateParams {
            input_dir: Some(inputs.join("stops.csv")),
            content_format: Some(ContentFormat::MultiFile),
            ..RunCreateParams::default()
        },
    ];
    let workspace = setup.workspace();
    for params in invalid {
        let err = setup
            .local
            .create_run(&workspace, params.clone())
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{params:?}: {err:?}");
    }
    let missing = RunCreateParams {
        input_dir: Some(setup.dir("missing")),
        content_format: Some(ContentFormat::MultiFile),
        ..RunCreateParams::default()
    };
    let err = setup
        .local
        .create_run(&workspace, missing)
        .await
        .unwrap_err();
    match err {
        Error::Io { path, source, .. } => {
            assert_eq!(path, setup.dir("missing"));
            assert_eq!(source.kind(), io::ErrorKind::NotFound);
        }
        other => panic!("expected a missing directory, got {other:?}"),
    }
    // Nothing was written: no run, and no registration.
    assert!(!setup.src.join(".fico").exists());
    let registered = setup.local.workspace_registered(&workspace).await.unwrap();
    assert!(!registered);
}

#[tokio::test]
async fn checks_the_input_against_a_multi_file_workspace() {
    let setup = Setup::new(MULTI_FILE_MANIFEST);
    let params = RunCreateParams {
        input: Some(json!({ "vehicles": 2 })),
        ..RunCreateParams::default()
    };
    let err = setup
        .local
        .create_run(&setup.workspace(), params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
    assert!(err.to_string().contains("`multi-file`"), "{err}");
    assert!(!setup.src.join(".fico").exists());
}

#[tokio::test]
async fn needs_a_workspace_to_create_a_run_in() {
    let setup = Setup::new(JSON_MANIFEST);
    std::fs::remove_file(setup.src.join("workspace.yaml")).unwrap();
    let params = RunCreateParams {
        input: Some(json!({})),
        ..RunCreateParams::default()
    };
    let err = setup
        .local
        .create_run(&setup.workspace(), params.clone())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Io { .. }), "{err:?}");
    let by_id = LocalWorkspaceRef {
        workspace_id: Some("routing".to_owned()),
        ..LocalWorkspaceRef::default()
    };
    let err = setup.local.create_run(&by_id, params).await.unwrap_err();
    assert!(matches!(err, Error::NotFound { .. }), "{err:?}");
}

#[tokio::test]
async fn checks_create_arguments_through_the_tool() {
    let setup = Setup::new(JSON_MANIFEST);
    let args = json!({
        "src": setup.src,
        "input": { "vehicles": 2 },
        "input_dir": setup.dir("inputs"),
        "name": "Monday",
        "options": { "duration": "10s" }
    });
    let err = support::call_tool(setup.context(), "decision_local_run_create", args)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ToolError::Failed(Error::Invalid { .. })),
        "{err:?}"
    );
    let args = json!({ "src": setup.src, "input": {}, "content_format": "csv-archive" });
    let err = support::call_tool(setup.context(), "decision_local_run_create", args)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArguments(_)), "{err:?}");
}

#[tokio::test]
async fn reads_runs_through_the_tools() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "succeeded", "json");
    setup.write("local-a", "inputs/input.json", r#"{"vehicles": 2}"#);
    setup.write("local-a", "logs/logs.log", "done\n");
    setup.write(
        "local-a",
        "outputs/solutions/solution.json",
        r#"{"solutions": []}"#,
    );
    let args = json!({ "src": setup.src, "run_id": "local-a" });

    let result = support::call_tool(setup.context(), "decision_local_run_get", args.clone());
    let result = result.await.unwrap();
    assert_eq!(result["id"], "local-a");
    assert_eq!(result["metadata"]["status_v2"], "succeeded");
    assert_eq!(result["output"], json!({ "solutions": [] }));

    let mut polling = args.clone();
    polling["polling"] = json!({ "initial_delay": 0, "max_tries": 1 });
    polling["output_dir"] = json!(setup.dir("output"));
    let result = support::call_tool(setup.context(), "decision_local_run_get", polling);
    assert_eq!(result.await.unwrap()["id"], "local-a");

    let info = support::call_tool(
        setup.context(),
        "decision_local_run_information_get",
        args.clone(),
    );
    let info = info.await.unwrap();
    assert_eq!(info["metadata"]["application_id"], "routing");
    assert!(info.get("output").is_none());

    let input = support::call_tool(
        setup.context(),
        "decision_local_run_input_get",
        args.clone(),
    );
    assert_eq!(input.await.unwrap(), json!({ "vehicles": 2 }));

    let logs = support::call_tool(setup.context(), "decision_local_run_logs_get", args);
    assert_eq!(logs.await.unwrap(), json!({ "log": "done\n" }));

    setup.run("local-b", "failed", "json");
    let args = json!({ "src": setup.src, "status": "failed" });
    let runs = support::call_tool(setup.context(), "decision_local_run_list", args);
    let runs = runs.await.unwrap();
    assert_eq!(runs.as_array().unwrap().len(), 1);
    assert_eq!(runs[0]["id"], "local-b");
}

/// An asset with a visual of `schema`, labeled `label`, with `content`.
fn visual_asset(schema: &str, label: &str, content: Value) -> Value {
    json!({
        "name": label,
        "content_type": "json",
        "visual": { "schema": schema, "label": label, "type": "custom-tab" },
        "content": content,
    })
}

/// Writes the assets of the run `run_id`: a Plotly chart and a GeoJSON map.
fn write_visual_assets(setup: &Setup, run_id: &str) {
    let geojson = json!({
        "type": "FeatureCollection",
        "features": [{
            "type": "Feature",
            "geometry": { "type": "Point", "coordinates": [-73.9, 40.7] },
            "properties": { "name": "depot" }
        }]
    });
    let assets = json!({ "assets": [
        visual_asset("plotly", "cost", json!({ "data": [{ "type": "bar", "y": [1, 2] }] })),
        visual_asset("geojson", "routes", geojson),
    ]});
    setup.write(run_id, "outputs/assets.json", assets.to_string());
}

#[tokio::test]
async fn gets_the_visuals_of_a_run() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "succeeded", "json");
    write_visual_assets(&setup, "local-a");
    let paths = setup
        .local
        .get_run_visuals(&setup.workspace(), "local-a")
        .await
        .unwrap();
    let visuals = setup.run_dir("local-a").join("visuals");
    assert_eq!(
        paths,
        [visuals.join("cost.html"), visuals.join("routes.html")]
    );
    let chart = std::fs::read_to_string(&paths[0]).unwrap();
    assert!(chart.contains("cdn.plot.ly/plotly-"), "{chart}");
    assert!(chart.contains(r#""type":"bar""#), "{chart}");
    let map = std::fs::read_to_string(&paths[1]).unwrap();
    assert!(map.contains("unpkg.com/leaflet@"), "{map}");
    assert!(map.contains(r#""name":"depot""#), "{map}");

    // A run without visual assets has none.
    setup.run("local-b", "succeeded", "json");
    let paths = setup
        .local
        .get_run_visuals(&setup.workspace(), "local-b")
        .await
        .unwrap();
    assert_eq!(paths, Vec::<PathBuf>::new());
}

#[tokio::test]
async fn has_no_visuals_before_the_run_ends() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "running", "json");
    write_visual_assets(&setup, "local-a");
    let paths = setup
        .local
        .get_run_visuals(&setup.workspace(), "local-a")
        .await
        .unwrap();
    assert_eq!(paths, Vec::<PathBuf>::new());
    assert!(!setup.run_dir("local-a").join("visuals").exists());
}

#[tokio::test]
async fn reports_assets_that_cannot_be_rendered() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "failed", "json");
    setup.write("local-a", "outputs/assets.json", "{ not json");
    let err = setup
        .local
        .get_run_visuals(&setup.workspace(), "local-a")
        .await
        .unwrap_err();
    assert_invalid_data(err, &setup.run_dir("local-a").join("outputs/assets.json"));
    let err = setup
        .local
        .get_run_visuals(&setup.workspace(), "local-missing")
        .await
        .unwrap_err();
    assert_not_found(err, "local-missing");
}

#[tokio::test]
async fn gets_visuals_through_the_tool() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "succeeded", "json");
    write_visual_assets(&setup, "local-a");
    let args = json!({ "src": setup.src, "run_id": "local-a" });
    let paths = support::call_tool(setup.context(), "decision_local_run_visuals_get", args);
    let visuals = setup.run_dir("local-a").join("visuals");
    assert_eq!(
        paths.await.unwrap(),
        json!([visuals.join("cost.html"), visuals.join("routes.html")])
    );
}

/// The manifest of a `binary` workspace with JSON input and output, whose
/// entrypoint is the shell script `main`.
const SCRIPT_MANIFEST: &str = "type: binary\nfiles: [main]\n";

/// The manifest of a `binary` workspace with multi-file input and output.
const MULTI_FILE_SCRIPT_MANIFEST: &str = "\
type: binary
files: [main]
configuration:
  content:
    format: multi-file
";

impl Setup {
    /// Writes `main`, an executable shell script with `body`, into the
    /// workspace.
    #[cfg(unix)]
    fn script(&self, body: &str) {
        use std::os::unix::fs::PermissionsExt;

        let path = self.src.join("main");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The information file of the run `run_id`, as JSON.
    fn information_file(&self, run_id: &str) -> Value {
        let path = self.run_dir(run_id).join(format!("{run_id}.json"));
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    /// Waits for the run `run_id` to end, and returns it.
    async fn wait(&self, run_id: &str) -> fico_sdk::decision::RunResult {
        let params = RunGetParams {
            polling: Some(PollingOptions {
                max_duration: Some(30.0),
                ..fast(None)
            }),
            ..RunGetParams::default()
        };
        let workspace = self.workspace();
        self.local
            .get_run(&workspace, run_id, params)
            .await
            .unwrap()
    }
}

/// A script that echoes its JSON input with its arguments.
const ECHO: &str = r#"printf '{"input": %s, "args": "%s"}' "$(cat)" "$*""#;

#[cfg(unix)]
#[tokio::test]
async fn clones_a_run_with_its_input_and_options() {
    let setup = Setup::new(SCRIPT_MANIFEST);
    setup.script(ECHO);
    // The original was created elsewhere, so the clone registers it.
    setup.run("local-a", "succeeded", "json");
    setup.write("local-a", "inputs/input.json", r#"{"vehicles": 2}"#);
    let created = setup
        .local
        .clone_run(&setup.workspace(), "local-a", RunCloneParams::default())
        .await
        .unwrap();
    assert_ne!(created.run_id, "local-a");
    assert!(created.run_id.starts_with("local-"), "{}", created.run_id);
    assert!(created.workspace_registered);

    let result = setup.wait(&created.run_id).await;
    let information = &result.information;
    assert_eq!(information.metadata.status, RunStatus::Succeeded);
    assert_eq!(information.name, "local run local-a clone");
    assert_eq!(
        information.description,
        "Clone of Local run created at 2026-10-13T15:00:00.123456Z"
    );
    let output = result.output.unwrap();
    assert_eq!(output["input"], json!({ "vehicles": 2 }));
    assert_eq!(output["args"], "-duration 10s");
    let file = setup.information_file(&created.run_id);
    let metadata = &file["metadata"];
    assert_eq!(metadata["tracking"]["cloned_run_id"], "local-a");
    assert_eq!(
        metadata["options"]["request_options"],
        json!({ "duration": "10s" })
    );
    assert_eq!(metadata["format"]["input"]["type"], "json");
    let entry = setup.local.get_workspace(&setup.workspace()).await.unwrap();
    assert_eq!(metadata["application_id"], json!(entry.workspace_id));

    // The workspace is registered now.
    let again = setup
        .local
        .clone_run(&setup.workspace(), "local-a", RunCloneParams::default())
        .await
        .unwrap();
    assert!(!again.workspace_registered);
    setup.wait(&again.run_id).await;
}

#[cfg(unix)]
#[tokio::test]
async fn clones_a_run_with_what_params_replace() {
    let setup = Setup::new(SCRIPT_MANIFEST);
    setup.script(ECHO);
    setup.run("local-a", "succeeded", "json");
    setup.write("local-a", "inputs/input.json", r#"{"vehicles": 2}"#);
    let input_file = setup.dir("input.json");
    std::fs::write(&input_file, r#"{"vehicles": 5}"#).unwrap();
    let params = RunCloneParams {
        input_file: Some(input_file),
        name: Some("n".repeat(130)),
        description: Some("Tuesday".to_owned()),
        options: [("solve.iterations".to_owned(), "3".to_owned())].into(),
        ..RunCloneParams::default()
    };
    let created = setup
        .local
        .clone_run(&setup.workspace(), "local-a", params)
        .await
        .unwrap();
    let result = setup.wait(&created.run_id).await;
    assert_eq!(result.information.name, "n".repeat(128));
    assert_eq!(result.information.description, "Tuesday");
    let output = result.output.unwrap();
    assert_eq!(output["input"], json!({ "vehicles": 5 }));
    assert_eq!(output["args"], "-solve.iterations 3");

    let params = RunCloneParams {
        input: Some(json!({ "vehicles": 7 })),
        ..RunCloneParams::default()
    };
    let created = setup
        .local
        .clone_run(&setup.workspace(), "local-a", params)
        .await
        .unwrap();
    let output = setup.wait(&created.run_id).await.output.unwrap();
    assert_eq!(output["input"], json!({ "vehicles": 7 }));
}

#[cfg(unix)]
#[tokio::test]
async fn clones_a_run_with_multi_file_input() {
    let setup = Setup::new(MULTI_FILE_SCRIPT_MANIFEST);
    setup.script(
        "mkdir -p outputs/solutions && cp inputs/fleet/vehicles.csv outputs/solutions/copy.csv",
    );
    setup.run("local-a", "succeeded", "multi-file");
    setup.write("local-a", "inputs/stops.csv", "id\n1\n");
    setup.write("local-a", "inputs/fleet/vehicles.csv", "id\nv1\n");
    let created = setup
        .local
        .clone_run(&setup.workspace(), "local-a", RunCloneParams::default())
        .await
        .unwrap();
    let result = setup.wait(&created.run_id).await;
    assert_eq!(result.information.metadata.status, RunStatus::Succeeded);
    let inputs = setup.run_dir(&created.run_id).join("inputs");
    let copied = |name: &str| std::fs::read_to_string(inputs.join(name)).unwrap();
    assert_eq!(copied("stops.csv"), "id\n1\n");
    assert_eq!(copied("fleet/vehicles.csv"), "id\nv1\n");
    let solutions = setup.run_dir(&created.run_id).join("outputs/solutions");
    let copy = std::fs::read_to_string(solutions.join("copy.csv")).unwrap();
    assert_eq!(copy, "id\nv1\n");

    // Another directory replaces the input.
    let other = setup.dir("other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("depots.csv"), "id\nd1\n").unwrap();
    let params = RunCloneParams {
        input_dir: Some(other),
        ..RunCloneParams::default()
    };
    let created = setup
        .local
        .clone_run(&setup.workspace(), "local-a", params)
        .await
        .unwrap();
    setup.wait(&created.run_id).await;
    let inputs = setup.run_dir(&created.run_id).join("inputs");
    assert!(inputs.join("depots.csv").is_file());
    assert!(!inputs.join("stops.csv").exists());
}

#[tokio::test]
async fn checks_a_clone_before_starting_it() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "succeeded", "json");
    setup.write("local-a", "inputs/input.json", r#"{"vehicles": 2}"#);
    let inputs = setup.dir("inputs");
    std::fs::create_dir_all(&inputs).unwrap();
    let invalid = [
        RunCloneParams {
            input: Some(json!({})),
            input_dir: Some(inputs.clone()),
            ..RunCloneParams::default()
        },
        RunCloneParams {
            input: Some(json!([1])),
            ..RunCloneParams::default()
        },
        // The cloned run's input is JSON.
        RunCloneParams {
            content_format: Some(ContentFormat::MultiFile),
            ..RunCloneParams::default()
        },
        RunCloneParams {
            input_dir: Some(inputs),
            content_format: Some(ContentFormat::Json),
            ..RunCloneParams::default()
        },
    ];
    let workspace = setup.workspace();
    for params in invalid {
        let err = setup
            .local
            .clone_run(&workspace, "local-a", params.clone())
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{params:?}: {err:?}");
    }
    // A run without its input has nothing to clone.
    setup.run("local-b", "succeeded", "json");
    let err = setup
        .local
        .clone_run(&workspace, "local-b", RunCloneParams::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
    let err = setup
        .local
        .clone_run(&workspace, "local-missing", RunCloneParams::default())
        .await
        .unwrap_err();
    assert_not_found(err, "local-missing");
    // Nothing was written: no new run, and no registration.
    let runs = std::fs::read_dir(setup.src.join(".fico/runs"))
        .unwrap()
        .count();
    assert_eq!(runs, 2);
    let registered = setup.local.workspace_registered(&workspace).await.unwrap();
    assert!(!registered);
}

#[cfg(unix)]
#[tokio::test]
async fn clones_through_the_tool() {
    let setup = Setup::new(SCRIPT_MANIFEST);
    setup.script(ECHO);
    setup.run("local-a", "succeeded", "json");
    setup.write("local-a", "inputs/input.json", r#"{"vehicles": 2}"#);
    let args = json!({ "src": setup.src, "run_id": "local-a", "name": "Wednesday" });
    let created = support::call_tool(setup.context(), "decision_local_run_clone", args);
    let created = created.await.unwrap();
    assert_eq!(created["workspace_registered"], true);
    let run_id = created["run_id"].as_str().unwrap();
    let result = setup.wait(run_id).await;
    assert_eq!(result.information.name, "Wednesday");
}

/// The parameters to compare the runs `run_ids`.
fn compared(run_ids: &[&str]) -> RunCompareParams {
    RunCompareParams {
        run_ids: run_ids.iter().map(|&run_id| run_id.to_owned()).collect(),
    }
}

#[tokio::test]
async fn compares_runs_field_by_field() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "succeeded", "json");
    setup.write(
        "local-a",
        "outputs/metrics.json",
        r#"{"metrics": {"cost": 12.3456789, "vehicles": 3, "routes": [1, 2]}}"#,
    );
    let mut b = information("local-b", "failed", "json");
    b["name"] = json!("Tuesday");
    b["metadata"]["options"]["active_options"] = json!({ "duration": "20s", "solver": "x" });
    b["metadata"]["integration"] = json!({ "id": "int-1", "provider": "dbx", "run_id": "r-9" });
    b["metadata"]["tracking"]["cloned_run_id"] = json!("local-a");
    setup.write_information("local-b", &b);
    setup.write(
        "local-b",
        "outputs/metrics.json",
        r#"{"metrics": {"cost": 12.3456801, "vehicles": 3.0, "routes": [1, 2]}}"#,
    );

    let comparison = setup
        .local
        .compare_runs(&setup.workspace(), compared(&["local-b", "local-a"]))
        .await
        .unwrap();
    assert_eq!(comparison.run_ids, ["local-b", "local-a"]);
    let emitted = serde_json::to_value(&comparison).unwrap();
    let field = |section: &str, name: &str| emitted[section][name].clone();

    assert_eq!(
        field("information", "name"),
        json!({
            "has_differences": true,
            "values": { "local-a": "local run local-a", "local-b": "Tuesday" }
        })
    );
    assert_eq!(field("information", "user_email")["has_differences"], false);
    let information = emitted["information"].as_object().unwrap();
    let names: Vec<&String> = information.keys().collect();
    assert_eq!(names, ["description", "id", "name", "user_email"]);

    let metadata = emitted["metadata"].as_object().unwrap();
    for left_out in [
        "metrics",
        "options",
        "format",
        "status_v2",
        "integration",
        "tracking",
    ] {
        assert!(!metadata.contains_key(left_out), "{left_out}");
    }
    assert_eq!(
        field("metadata", "status"),
        json!({
            "has_differences": true,
            "values": { "local-a": "succeeded", "local-b": "failed" }
        })
    );
    assert_eq!(
        field("metadata", "content_format")["values"],
        json!({ "local-a": "json", "local-b": "json" })
    );
    assert_eq!(
        field("metadata", "content_format")["has_differences"],
        false
    );
    assert_eq!(
        field("metadata", "run_type")["values"]["local-a"],
        "standard"
    );
    assert_eq!(field("metadata", "cloned_run_id")["has_differences"], true);
    // Only the run with an integration has its fields.
    assert_eq!(
        field("metadata", "provider"),
        json!({ "has_differences": false, "values": { "local-b": "dbx" } })
    );
    assert_eq!(field("metadata", "duration")["has_differences"], false);

    // Rounded to 5 digits, and 3 is 3.0.
    assert_eq!(
        field("metrics", "cost"),
        json!({
            "has_differences": false,
            "values": { "local-a": 12.34568, "local-b": 12.34568 }
        })
    );
    assert_eq!(field("metrics", "vehicles")["has_differences"], false);
    assert_eq!(field("metrics", "routes")["has_differences"], false);
    assert_eq!(
        field("options", "duration"),
        json!({
            "has_differences": true,
            "values": { "local-a": "10s", "local-b": "20s" }
        })
    );
    assert_eq!(
        field("options", "solver"),
        json!({ "has_differences": false, "values": { "local-b": "x" } })
    );
}

#[tokio::test]
async fn compares_runs_without_metrics_or_options() {
    let setup = Setup::new(JSON_MANIFEST);
    let mut a = information("local-a", "succeeded", "json");
    a["metadata"]["options"] = json!(null);
    setup.write_information("local-a", &a);
    let comparison = setup
        .local
        .compare_runs(&setup.workspace(), compared(&["local-a"]))
        .await
        .unwrap();
    assert_eq!(comparison.metrics, None);
    assert_eq!(comparison.options, None);
    let emitted = serde_json::to_value(&comparison).unwrap();
    assert!(emitted.get("metrics").is_none() && emitted.get("options").is_none());
    assert_eq!(emitted["information"]["id"]["has_differences"], false);
}

#[tokio::test]
async fn checks_the_runs_to_compare() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "succeeded", "json");
    let workspace = setup.workspace();
    let many: Vec<String> = (0..21).map(|i| format!("local-{i}")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    for run_ids in [&[][..], &many, &["local-a", "local-a"], &["../x"]] {
        let err = setup
            .local
            .compare_runs(&workspace, compared(run_ids))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{run_ids:?}: {err:?}");
    }
    let err = setup
        .local
        .compare_runs(&workspace, compared(&["local-a", "local-missing"]))
        .await
        .unwrap_err();
    assert_not_found(err, "local-missing");
}

#[tokio::test]
async fn compares_runs_through_the_tool() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.run("local-a", "succeeded", "json");
    setup.run("local-b", "failed", "json");
    let args = json!({ "src": setup.src, "run_ids": ["local-a", "local-b"] });
    let comparison = support::call_tool(setup.context(), "decision_local_run_compare", args);
    let comparison = comparison.await.unwrap();
    assert_eq!(comparison["run_ids"], json!(["local-a", "local-b"]));
    assert_eq!(comparison["metadata"]["status"]["has_differences"], true);
    let args = json!({ "src": setup.src });
    let err = support::call_tool(setup.context(), "decision_local_run_compare", args);
    assert!(
        matches!(err.await.unwrap_err(), ToolError::InvalidArguments(_)),
        "`run_ids` is required"
    );
}

/// A workspace that solves a JSON input: it reads `vehicles`, logs to
/// stderr, and prints its output with metrics and a Plotly asset.
const SOLVER: &str = r#"input="$(cat)"
vehicles=$(printf '%s' "$input" | tr -cd '0-9')
echo "solving for $vehicles vehicles" >&2
printf '{"solutions": [{"vehicles": %s}], "metrics": {"cost": %s.5}, "assets": [{"name": "cost", "content_type": "json", "visual": {"schema": "plotly", "label": "cost", "type": "custom-tab"}, "content": {"data": [{"type": "bar", "y": [%s]}]}}]}' "$vehicles" "$vehicles" "$vehicles""#;

#[cfg(unix)]
#[tokio::test]
async fn runs_a_workspace_end_to_end() {
    let setup = Setup::new(SCRIPT_MANIFEST);
    setup.script(SOLVER);
    let workspace = setup.workspace();
    let params = RunCreateParams {
        input: Some(json!({ "vehicles": 2 })),
        ..RunCreateParams::default()
    };
    let created = setup.local.create_run(&workspace, params).await.unwrap();
    assert!(created.workspace_registered);
    let run_id = created.run_id;

    // Create, then get with polling.
    let result = setup.wait(&run_id).await;
    let information = &result.information;
    assert_eq!(information.id, run_id);
    assert_eq!(information.metadata.status, RunStatus::Succeeded);
    assert_eq!(information.metadata.error, "");
    assert!(information.metadata.duration > 0.0);
    assert!(information.metadata.output_size > 0.0);
    let metrics = information.metadata.metrics.as_ref().unwrap();
    assert_eq!(metrics["cost"], json!(2.5));
    let output = result.output.unwrap();
    assert_eq!(output["solutions"], json!([{ "vehicles": 2 }]));
    assert_eq!(result.error_log, None);

    let logs = setup
        .local
        .get_run_logs(&workspace, &run_id, RunLogsGetParams::default())
        .await
        .unwrap();
    assert_eq!(logs.log, "solving for 2 vehicles\n");
    let input = setup
        .local
        .get_run_input(&workspace, &run_id, RunInputGetParams::default())
        .await
        .unwrap();
    assert_eq!(input, Some(json!({ "vehicles": 2 })));

    // Finalization rendered the visuals.
    let visuals = setup.run_dir(&run_id).join("visuals");
    assert!(visuals.join("cost.html").is_file());
    let paths = setup
        .local
        .get_run_visuals(&workspace, &run_id)
        .await
        .unwrap();
    assert_eq!(paths, [visuals.join("cost.html")]);

    // A clone with another input, compared with the original.
    let params = RunCloneParams {
        input: Some(json!({ "vehicles": 4 })),
        ..RunCloneParams::default()
    };
    let clone = setup
        .local
        .clone_run(&workspace, &run_id, params)
        .await
        .unwrap();
    assert!(!clone.workspace_registered);
    let cloned = setup.wait(&clone.run_id).await;
    assert_eq!(cloned.information.metadata.status, RunStatus::Succeeded);
    assert_eq!(
        cloned.output.unwrap()["solutions"],
        json!([{ "vehicles": 4 }])
    );
    let comparison = setup
        .local
        .compare_runs(&workspace, compared(&[&run_id, &clone.run_id]))
        .await
        .unwrap();
    let cost = &comparison.metrics.as_ref().unwrap()["cost"];
    assert!(cost.has_differences);
    assert_eq!(cost.values[&run_id], json!(2.5));
    assert_eq!(cost.values[&clone.run_id], json!(4.5));
    assert_eq!(
        comparison.metadata["cloned_run_id"].values[&clone.run_id],
        json!(run_id)
    );

    let runs = setup
        .local
        .list_runs(&workspace, RunListParams::default())
        .await
        .unwrap();
    assert_eq!(runs.len(), 2);
    assert!(runs.iter().all(|run| run.status == RunStatus::Succeeded));
}

#[cfg(unix)]
#[tokio::test]
async fn runs_a_failing_workspace_end_to_end() {
    let setup = Setup::new(SCRIPT_MANIFEST);
    setup.script("cat > /dev/null\necho broken >&2\nexit 3");
    let params = RunCreateParams {
        input: Some(json!({})),
        ..RunCreateParams::default()
    };
    let created = setup
        .local
        .create_run(&setup.workspace(), params)
        .await
        .unwrap();
    let result = setup.wait(&created.run_id).await;
    assert_eq!(result.information.metadata.status, RunStatus::Failed);
    assert_eq!(
        result.error_log.unwrap().error.as_deref(),
        Some("Run failed, please check logs for details.")
    );
    assert_eq!(result.output, None);
    let paths = setup
        .local
        .get_run_visuals(&setup.workspace(), &created.run_id)
        .await
        .unwrap();
    assert_eq!(paths, Vec::<PathBuf>::new());
}
