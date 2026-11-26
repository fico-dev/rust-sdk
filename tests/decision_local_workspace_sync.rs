//! Tests for syncing a local workspace to the platform and its tool, with
//! run directories written to the run-directory layout (`local/run_dir.rs`)
//! in a temporary workspace, a temporary FICO home, and mock servers for
//! the platform and the presigned uploads.

mod mock_server;
mod support;

use std::io::Read;
use std::path::PathBuf;

use fico_sdk::Error;
use fico_sdk::decision::local::{Local, WorkspaceRegisterParams, WorkspaceSyncParams};
use fico_sdk::decision::platform::Platform;
use fico_sdk::decision::{LocalWorkspaceRef, RunSyncOutcome, SyncedRun, WorkspaceSynced};
use fico_sdk::tool::{Context, ToolError};
use flate2::read::GzDecoder;
use serde_json::{Value, json};

use crate::mock_server::{MockResponse, MockServer, RecordedRequest};
use crate::support::TempDir;

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
    _root: TempDir,
    local: Local,
    src: PathBuf,
}

impl Setup {
    fn new(manifest: &str) -> Setup {
        let root = TempDir::new("local-workspace-sync");
        let local = Local::with_home(root.path().join(".fico"));
        let src = root.path().join("routing");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("workspace.yaml"), manifest).unwrap();
        let src = src.canonicalize().unwrap();
        Setup {
            _root: root,
            local,
            src,
        }
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

    /// Writes the information file of a run.
    fn write_information(&self, run_id: &str, information: &Value) {
        let file = format!("{run_id}.json");
        let bytes = serde_json::to_vec_pretty(information).unwrap();
        self.write(run_id, &file, bytes);
    }

    /// The information file of the run `run_id`, as JSON.
    fn information(&self, run_id: &str) -> Value {
        let path = self.run_dir(run_id).join(format!("{run_id}.json"));
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    /// A JSON run `run_id` in `status`, created at second `second` of the
    /// day, with its input, its output if it succeeded, and logs.
    fn json_run(&self, run_id: &str, status: &str, second: u32) {
        self.write_information(run_id, &information(run_id, status, "json", second));
        self.write(run_id, "inputs/input.json", r#"{"stops": ["a"]}"#);
        if status == "succeeded" {
            self.write(
                run_id,
                "outputs/solutions/solution.json",
                r#"{"routes": [], "metrics": {"cost": 1}}"#,
            );
        }
        self.write(run_id, "logs/logs.log", format!("solving {run_id}\n"));
    }

    async fn sync(
        &self,
        platform: &Platform,
        params: WorkspaceSyncParams,
    ) -> fico_sdk::Result<WorkspaceSynced> {
        self.local
            .sync_workspace(&self.workspace(), platform, "routing-cloud", params)
            .await
    }
}

/// The information file of a local run `run_id` that ended in `status`,
/// with `format` input and output, created at second `second` of the day,
/// as `create_run` writes it and the executor finalizes it, with a key this
/// SDK does not know.
fn information(run_id: &str, status: &str, format: &str, second: u32) -> Value {
    let error = match status {
        "failed" => "Run failed, please check logs for details.",
        _ => "",
    };
    let created_at = format!("2026-10-13T15:00:{second:02}.123456Z");
    json!({
        "description": format!("Local run created at {created_at}"),
        "id": run_id,
        "metadata": {
            "application_id": "routing",
            "application_instance_id": "",
            "application_version_id": "",
            "created_at": created_at,
            "duration": 1250.7,
            "error": error,
            "execution_class": "local",
            "execution_duration": 1250.7,
            "experiment_id": "",
            "experiment_type": "",
            "format": { "input": { "type": format }, "output": { "type": format } },
            "initiated_at": created_at,
            "input_size": 16.0,
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
        "console_url": "",
        "kept": { "unknown": true }
    })
}

/// A presigned URL on `bucket` for `key`.
fn presigned(bucket: &MockServer, key: &str) -> String {
    format!("{}/{key}?X-Amz-Signature=abc", bucket.url())
}

/// The platform's answers to tracking runs, one entry per run: its number
/// of uploads. Each upload gets `upload-<n>` at its own key on `bucket`, and
/// each run is created as `run-<i>`.
fn track_responses(bucket: &MockServer, uploads_per_run: &[usize]) -> Vec<MockResponse> {
    let mut responses = Vec::new();
    let mut n = 0;
    for (i, uploads) in uploads_per_run.iter().enumerate() {
        for _ in 0..*uploads {
            let url = presigned(bucket, &format!("uploads/{n}"));
            let upload = json!({ "upload_id": format!("upload-{n}"), "upload_url": url });
            responses.push(MockResponse::json(200, upload));
            n += 1;
        }
        let created = json!({ "run_id": format!("run-{i}") });
        responses.push(MockResponse::json(202, created));
    }
    responses
}

/// The answers of [`track_responses`], after the platform workspace's, for
/// a sync that resolves its default environment: `production`, if any.
fn default_then_track(
    bucket: &MockServer,
    default: Option<&str>,
    uploads_per_run: &[usize],
) -> Vec<MockResponse> {
    let mut workspace = json!({ "id": "routing-cloud" });
    if let Some(default) = default {
        workspace["default_instance"] = default.into();
    }
    let mut responses = vec![MockResponse::json(200, workspace)];
    responses.extend(track_responses(bucket, uploads_per_run));
    responses
}

/// A bucket for presigned uploads that accepts `uploads` of them.
async fn bucket(uploads: usize) -> MockServer {
    MockServer::start(vec![MockResponse::new(200); uploads]).await
}

/// The method and target of each request.
fn sent(requests: &[RecordedRequest]) -> Vec<String> {
    let line = |r: &RecordedRequest| format!("{} {}", r.method, r.target);
    requests.iter().map(line).collect()
}

/// The JSON body of `request`.
fn body(request: &RecordedRequest) -> Value {
    serde_json::from_slice(&request.body).unwrap()
}

/// The files of the tar.gz `archive`, as (path, contents), sorted.
fn untar(archive: &[u8]) -> Vec<(String, String)> {
    let mut archive = tar::Archive::new(GzDecoder::new(archive));
    let mut files: Vec<(String, String)> = archive
        .entries()
        .unwrap()
        .map(|entry| {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().into_owned();
            let mut contents = String::new();
            entry.read_to_string(&mut contents).unwrap();
            (path, contents)
        })
        .collect();
    files.sort();
    files
}

/// The record of a run that the sync added, as the outcome has it.
fn synced(outcome: &RunSyncOutcome) -> &SyncedRun {
    match outcome {
        RunSyncOutcome::Synced { synced_run, .. } => synced_run,
        other => panic!("expected a synced run, got {other:?}"),
    }
}

/// The reason of a run that the sync skipped.
fn skipped(outcome: &RunSyncOutcome) -> &str {
    match outcome {
        RunSyncOutcome::Skipped { reason, .. } => reason,
        other => panic!("expected a skipped run, got {other:?}"),
    }
}

/// A JSON object with the keys of `record`, a synced run, that the test
/// checks, without the time it was synced at.
fn without_time(record: &Value) -> Value {
    let mut record = record.clone();
    let synced_at = record.as_object_mut().unwrap().remove("synced_at");
    let synced_at = synced_at.unwrap();
    // UTC, with microseconds.
    let synced_at = synced_at.as_str().unwrap();
    assert!(
        synced_at.ends_with('Z') && synced_at.len() == 27,
        "{synced_at}"
    );
    record
}

#[tokio::test]
async fn syncs_the_runs_that_ended_and_skips_the_others() {
    let setup = Setup::new(JSON_MANIFEST);
    // Created out of order, to check that the oldest run is synced first.
    setup.json_run("local-failed", "failed", 2);
    setup.json_run("local-done", "succeeded", 1);
    setup.json_run("local-running", "running", 3);
    setup.json_run("local-no-input", "succeeded", 4);
    std::fs::remove_file(setup.run_dir("local-no-input").join("inputs/input.json")).unwrap();
    setup.json_run("local-before", "succeeded", 5);
    let mut before = setup.information("local-before");
    let earlier = json!({
        "run_id": "run-old",
        "synced_at": "2026-10-12T09:00:00.000000Z",
        "app_id": "routing-cloud",
        "instance_id": "staging",
    });
    before["synced_runs"] = json!([earlier]);
    setup.write_information("local-before", &before);

    // The succeeded run uploads its input, output and logs; the failed one
    // its input and logs.
    let bucket = bucket(5).await;
    let server = MockServer::start(default_then_track(&bucket, Some("production"), &[3, 2])).await;
    let platform = support::platform(&server);
    let result = setup
        .sync(&platform, WorkspaceSyncParams::default())
        .await
        .unwrap();

    // Not registered before: a `src` registers it, as `create_run` does.
    assert!(result.workspace_registered);
    let run_ids: Vec<&str> = result.runs.iter().map(|run| run.run_id.as_str()).collect();
    assert_eq!(
        run_ids,
        [
            "local-done",
            "local-failed",
            "local-running",
            "local-no-input",
            "local-before"
        ]
    );
    let done = synced(&result.runs[0].outcome);
    assert_eq!(done.run_id, "run-0");
    assert_eq!(done.workspace_id, "routing-cloud");
    assert_eq!(done.environment_id, None);
    assert_eq!(synced(&result.runs[1].outcome).run_id, "run-1");
    let reason = skipped(&result.runs[2].outcome);
    assert!(reason.contains("has not ended"), "{reason}");
    let reason = skipped(&result.runs[3].outcome);
    assert!(reason.contains("has no input"), "{reason}");
    // Without an environment, a record in any environment of the platform
    // workspace counts.
    match &result.runs[4].outcome {
        RunSyncOutcome::AlreadySynced { synced_run, .. } => {
            assert_eq!(synced_run.run_id, "run-old");
            assert_eq!(synced_run.environment_id.as_deref(), Some("staging"));
        }
        other => panic!("expected an already synced run, got {other:?}"),
    }

    let requests = server.requests();
    assert_eq!(
        sent(&requests),
        [
            // Once, for the default environment, before the first run.
            "GET /v1/applications/routing-cloud",
            "POST /v1/applications/routing-cloud/runs/uploadurl",
            "POST /v1/applications/routing-cloud/runs/uploadurl",
            "POST /v1/applications/routing-cloud/runs/uploadurl",
            "POST /v1/applications/routing-cloud/runs?instance_id=production",
            "POST /v1/applications/routing-cloud/runs/uploadurl",
            "POST /v1/applications/routing-cloud/runs/uploadurl",
            "POST /v1/applications/routing-cloud/runs?instance_id=production",
        ]
    );
    // No environment: the default one.
    assert_eq!(
        body(&requests[4]),
        json!({
            "upload_id": "upload-0",
            "name": "local run local-done",
            "description": "Local run created at 2026-10-13T15:00:01.123456Z",
            "configuration": {
                "format": { "input": { "type": "json" }, "output": { "type": "json" } },
                "run_type": { "type": "external" },
            },
            "result": {
                "output_upload_id": "upload-1",
                "error_upload_id": "upload-2",
                "status": "succeeded",
                "execution_duration": 1250,
            },
        })
    );
    // A failed run has no output, and its error.
    assert_eq!(
        body(&requests[7])["result"],
        json!({
            "error_upload_id": "upload-4",
            "status": "failed",
            "error_message": "Run failed, please check logs for details.",
            "execution_duration": 1250,
        })
    );
    let uploads: Vec<Vec<u8>> = bucket.requests().into_iter().map(|r| r.body).collect();
    let json = |bytes: &[u8]| serde_json::from_slice::<Value>(bytes).unwrap();
    assert_eq!(json(&uploads[0]), json!({ "stops": ["a"] }));
    assert_eq!(
        json(&uploads[1]),
        json!({ "routes": [], "metrics": { "cost": 1 } })
    );
    assert_eq!(uploads[2], b"solving local-done\n");
    assert_eq!(uploads[4], b"solving local-failed\n");

    // Only `synced_runs` changes: the key this SDK does not know, and the
    // rest of the file, stay as they were.
    let mut expected = information("local-done", "succeeded", "json", 1);
    let written = setup.information("local-done");
    let records = written["synced_runs"].as_array().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(
        without_time(&records[0]),
        json!({ "run_id": "run-0", "app_id": "routing-cloud" })
    );
    expected["synced_runs"] = written["synced_runs"].clone();
    assert_eq!(written, expected);
    assert!(written.get("output").is_none());
    assert_eq!(setup.information("local-before"), before);
    assert!(
        setup
            .information("local-running")
            .get("synced_runs")
            .is_none()
    );
}

#[tokio::test]
async fn sends_nothing_the_second_time() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.json_run("local-done", "succeeded", 1);
    let bucket = bucket(3).await;
    let server = MockServer::start(default_then_track(&bucket, None, &[3])).await;
    let platform = support::platform(&server);
    let first = setup
        .sync(&platform, WorkspaceSyncParams::default())
        .await
        .unwrap();
    let record = synced(&first.runs[0].outcome).clone();
    assert_eq!(server.requests().len(), 5);
    // Without a default environment: none, for the platform's fallback.
    assert_eq!(
        server.requests()[4].target,
        "/v1/applications/routing-cloud/runs"
    );

    // Not even the workspace: no run needs its environment.
    let second = setup
        .sync(&platform, WorkspaceSyncParams::default())
        .await
        .unwrap();
    assert_eq!(server.requests().len(), 5);
    assert!(!second.workspace_registered);
    assert_eq!(second.runs.len(), 1);
    match &second.runs[0].outcome {
        RunSyncOutcome::AlreadySynced { synced_run, .. } => assert_eq!(*synced_run, record),
        other => panic!("expected an already synced run, got {other:?}"),
    }
    let records = setup.information("local-done")["synced_runs"].clone();
    assert_eq!(records.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn syncs_multi_file_runs_with_metrics_and_assets_to_an_environment() {
    let setup = Setup::new(MULTI_FILE_MANIFEST);
    let run_id = "local-files";
    setup.write_information(run_id, &information(run_id, "succeeded", "multi-file", 1));
    setup.write(run_id, "inputs/stops.csv", "id\na\n");
    setup.write(run_id, "outputs/solutions/routes.csv", "vehicle,stop\n");
    setup.write(
        run_id,
        "outputs/metrics.json",
        r#"{"metrics": {"cost": 12.5}}"#,
    );
    setup.write(
        run_id,
        "outputs/assets.json",
        r#"{"assets": [{"name": "map", "content_type": "json"}]}"#,
    );
    setup.write(run_id, "logs/logs.log", "solving\n");
    // Synced before, but to another environment.
    let mut before = setup.information(run_id);
    before["synced_runs"] = json!([{
        "run_id": "run-old",
        "synced_at": "2026-10-12T09:00:00.000000Z",
        "app_id": "routing-cloud",
    }]);
    setup.write_information(run_id, &before);

    // Input, output, logs, metrics and assets.
    let bucket = bucket(5).await;
    let server = MockServer::start(track_responses(&bucket, &[5])).await;
    let platform = support::platform(&server);
    let params = WorkspaceSyncParams {
        environment_id: Some("staging".to_owned()),
        ..WorkspaceSyncParams::default()
    };
    let result = setup.sync(&platform, params).await.unwrap();
    let record = synced(&result.runs[0].outcome);
    assert_eq!(record.environment_id.as_deref(), Some("staging"));

    let requests = server.requests();
    assert_eq!(requests.len(), 6, "{:?}", sent(&requests));
    let created = &requests[5];
    assert_eq!(
        created.target,
        "/v1/applications/routing-cloud/runs?instance_id=staging"
    );
    assert_eq!(
        body(created)["configuration"]["format"],
        json!({ "input": { "type": "multi-file" }, "output": { "type": "multi-file" } })
    );
    assert_eq!(
        body(created)["result"],
        json!({
            "output_upload_id": "upload-1",
            "error_upload_id": "upload-2",
            "status": "succeeded",
            "execution_duration": 1250,
            "metrics_upload_id": "upload-3",
            "assets_upload_id": "upload-4",
        })
    );
    let uploads: Vec<Vec<u8>> = bucket.requests().into_iter().map(|r| r.body).collect();
    assert_eq!(
        untar(&uploads[0]),
        [("stops.csv".to_owned(), "id\na\n".to_owned())]
    );
    assert_eq!(
        untar(&uploads[1]),
        [("routes.csv".to_owned(), "vehicle,stop\n".to_owned())]
    );
    let json = |bytes: &[u8]| serde_json::from_slice::<Value>(bytes).unwrap();
    assert_eq!(json(&uploads[3]), json!({ "cost": 12.5 }));
    assert_eq!(
        json(&uploads[4]),
        json!({ "assets": [{ "name": "map", "content_type": "json" }] })
    );

    // The new record comes after the earlier one.
    let records = setup.information(run_id)["synced_runs"].clone();
    let records = records.as_array().unwrap();
    assert_eq!(records[0], before["synced_runs"][0]);
    assert_eq!(
        without_time(&records[1]),
        json!({ "run_id": "run-0", "app_id": "routing-cloud", "instance_id": "staging" })
    );

    // Synced to that environment now, so the next sync to it sends nothing.
    let params = WorkspaceSyncParams {
        environment_id: Some("staging".to_owned()),
        ..WorkspaceSyncParams::default()
    };
    let again = setup.sync(&platform, params).await.unwrap();
    assert!(matches!(
        again.runs[0].outcome,
        RunSyncOutcome::AlreadySynced { .. }
    ));
    assert_eq!(server.requests().len(), 6);
}

#[tokio::test]
async fn reads_default_as_no_environment() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.json_run("local-done", "succeeded", 1);
    let bucket = bucket(6).await;
    let server = MockServer::start(default_then_track(&bucket, Some("production"), &[3, 3])).await;
    let platform = support::platform(&server);
    setup
        .sync(&platform, WorkspaceSyncParams::default())
        .await
        .unwrap();
    // `default` is the environment a sync without one records the run in,
    // so the run is not tracked there twice.
    let params = |environment_id: &str| WorkspaceSyncParams {
        environment_id: Some(environment_id.to_owned()),
        ..WorkspaceSyncParams::default()
    };
    let again = setup.sync(&platform, params("default")).await.unwrap();
    match &again.runs[0].outcome {
        RunSyncOutcome::AlreadySynced { synced_run, .. } => {
            assert_eq!(synced_run.run_id, "run-0");
            assert_eq!(synced_run.environment_id, None);
        }
        other => panic!("expected an already synced run, got {other:?}"),
    }
    assert_eq!(server.requests().len(), 5);
    // Another environment is not the default one, and is not resolved.
    let staging = setup.sync(&platform, params("staging")).await.unwrap();
    let record = synced(&staging.runs[0].outcome);
    assert_eq!(record.environment_id.as_deref(), Some("staging"));
    assert_eq!(server.requests().len(), 9);
    assert_eq!(
        server.requests()[8].target,
        "/v1/applications/routing-cloud/runs?instance_id=staging"
    );
}

#[tokio::test]
async fn syncs_only_the_runs_asked_for_in_their_order() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.json_run("local-a", "succeeded", 1);
    setup.json_run("local-b", "succeeded", 2);
    setup.json_run("local-c", "succeeded", 3);
    let bucket = bucket(6).await;
    let server = MockServer::start(default_then_track(&bucket, None, &[3, 3])).await;
    let platform = support::platform(&server);
    let params = WorkspaceSyncParams {
        run_ids: vec!["local-c".to_owned(), "local-a".to_owned()],
        environment_id: Some(String::new()),
    };
    let result = setup.sync(&platform, params).await.unwrap();
    let run_ids: Vec<&str> = result.runs.iter().map(|run| run.run_id.as_str()).collect();
    assert_eq!(run_ids, ["local-c", "local-a"]);
    // An empty environment is none: the default one, here the platform's
    // fallback, since the workspace has none.
    assert_eq!(synced(&result.runs[0].outcome).environment_id, None);
    assert_eq!(
        server.requests()[4].target,
        "/v1/applications/routing-cloud/runs"
    );
    assert!(setup.information("local-b").get("synced_runs").is_none());
}

#[tokio::test]
async fn checks_the_ids_before_any_request() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.json_run("local-done", "succeeded", 1);
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    for run_id in ["../local-done", "local-missing"] {
        let params = WorkspaceSyncParams {
            run_ids: vec!["local-done".to_owned(), run_id.to_owned()],
            ..WorkspaceSyncParams::default()
        };
        let err = setup.sync(&platform, params).await.unwrap_err();
        match (run_id, err) {
            ("local-missing", Error::NotFound { kind, id, .. }) => {
                assert_eq!((kind, id.as_str()), ("local run", "local-missing"));
            }
            ("../local-done", Error::Invalid { .. }) => {}
            (_, err) => panic!("{run_id}: {err:?}"),
        }
    }
    for platform_workspace_id in ["", ".."] {
        let err = setup
            .local
            .sync_workspace(
                &setup.workspace(),
                &platform,
                platform_workspace_id,
                WorkspaceSyncParams::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
    }
    assert!(server.requests().is_empty());
    assert!(setup.information("local-done").get("synced_runs").is_none());
    // A sync that fails its checks registers nothing, as a local run that
    // fails its checks does.
    let workspace = setup.workspace();
    assert!(!setup.local.workspace_registered(&workspace).await.unwrap());
}

#[tokio::test]
async fn skips_runs_that_track_run_rejects_and_syncs_the_next() {
    let setup = Setup::new(JSON_MANIFEST);
    // A failed run without a log file has neither output nor logs, which
    // the platform needs.
    setup.json_run("local-empty", "failed", 1);
    std::fs::remove_file(setup.run_dir("local-empty").join("logs/logs.log")).unwrap();
    // A run whose input is not a JSON object.
    setup.json_run("local-list", "succeeded", 2);
    setup.write("local-list", "inputs/input.json", "[1, 2]");
    setup.json_run("local-done", "succeeded", 3);
    // Without a log file, a succeeded run is synced without logs.
    std::fs::remove_file(setup.run_dir("local-done").join("logs/logs.log")).unwrap();
    let bucket = bucket(2).await;
    let server = MockServer::start(default_then_track(&bucket, None, &[2])).await;
    let platform = support::platform(&server);
    let result = setup
        .sync(&platform, WorkspaceSyncParams::default())
        .await
        .unwrap();

    let reason = skipped(&result.runs[0].outcome);
    assert!(reason.contains("output or its logs"), "{reason}");
    let reason = skipped(&result.runs[1].outcome);
    assert!(reason.contains("JSON object"), "{reason}");
    assert_eq!(synced(&result.runs[2].outcome).run_id, "run-0");
    let requests = server.requests();
    assert_eq!(requests.len(), 4, "{:?}", sent(&requests));
    assert!(
        body(&requests[3])["result"]
            .get("error_upload_id")
            .is_none()
    );
}

#[tokio::test]
async fn stops_at_a_request_that_fails() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.json_run("local-a", "succeeded", 1);
    setup.json_run("local-b", "succeeded", 2);
    // The first run syncs, and the second one's first upload URL is a 404,
    // as for a platform workspace that does not exist.
    let bucket = bucket(3).await;
    let mut responses = default_then_track(&bucket, None, &[3]);
    responses.push(MockResponse::json(404, json!({ "error": "not found" })));
    let server = MockServer::start(responses).await;
    let platform = support::platform(&server);
    let err = setup
        .sync(&platform, WorkspaceSyncParams::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Status { status: 404, .. }), "{err:?}");
    assert_eq!(server.requests().len(), 6);
    // The run synced before the error keeps its record, so the next sync
    // skips it.
    assert!(setup.information("local-a")["synced_runs"].is_array());
    assert!(setup.information("local-b").get("synced_runs").is_none());
}

#[tokio::test]
async fn stops_when_the_platform_workspace_does_not_exist() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.json_run("local-a", "succeeded", 1);
    let server = MockServer::start(vec![MockResponse::json(
        404,
        json!({ "error": "not found" }),
    )])
    .await;
    let platform = support::platform(&server);
    let err = setup
        .sync(&platform, WorkspaceSyncParams::default())
        .await
        .unwrap_err();
    // The default environment's read is the first request, so nothing is
    // uploaded.
    assert!(matches!(err, Error::Status { status: 404, .. }), "{err:?}");
    assert_eq!(
        sent(&server.requests()),
        ["GET /v1/applications/routing-cloud"]
    );
    assert!(setup.information("local-a").get("synced_runs").is_none());
}

#[tokio::test]
async fn syncs_a_registered_workspace_by_its_id() {
    let setup = Setup::new(JSON_MANIFEST);
    let registered = setup
        .local
        .register_workspace(WorkspaceRegisterParams {
            src: setup.src.clone(),
            workspace_id: Some("routing".to_owned()),
            description: None,
        })
        .await
        .unwrap();
    assert_eq!(registered.workspace_id, "routing");
    // A workspace without runs has nothing to sync.
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    let by_id = LocalWorkspaceRef {
        workspace_id: Some("routing".to_owned()),
        src: None,
    };
    let result = setup
        .local
        .sync_workspace(
            &by_id,
            &platform,
            "routing-cloud",
            WorkspaceSyncParams::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        json!({ "runs": [] })
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn serves_the_sync_tool() {
    let setup = Setup::new(JSON_MANIFEST);
    setup.json_run("local-done", "succeeded", 1);
    let bucket = bucket(3).await;
    let server = MockServer::start(track_responses(&bucket, &[3])).await;
    let cx = Context::new()
        .with_platform(support::platform(&server))
        .with_local(setup.local.clone());
    let args = json!({
        "src": setup.src,
        "platform_workspace_id": "routing-cloud",
        "run_ids": ["local-done"],
        "environment_id": "staging",
    });
    let result = support::call_tool(cx.clone(), "decision_local_workspace_sync", args)
        .await
        .unwrap();
    let run = &result["runs"][0];
    assert_eq!(run["run_id"], "local-done");
    assert_eq!(run["outcome"], "synced");
    assert_eq!(
        without_time(&run["synced_run"]),
        json!({ "run_id": "run-0", "app_id": "routing-cloud", "instance_id": "staging" })
    );
    assert_eq!(result["workspace_registered"], true);
    assert_eq!(
        server.requests()[3].target,
        "/v1/applications/routing-cloud/runs?instance_id=staging"
    );

    // It needs both the platform and local mode.
    let args = json!({ "src": setup.src, "platform_workspace_id": "routing-cloud" });
    for cx in [
        Context::new().with_local(setup.local.clone()),
        Context::new().with_platform(support::platform(&server)),
    ] {
        let err = support::call_tool(cx, "decision_local_workspace_sync", args.clone())
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::Failed(Error::Invalid { .. })),
            "{err}"
        );
    }
    // `platform_workspace_id` is required.
    let err = support::call_tool(
        cx,
        "decision_local_workspace_sync",
        json!({ "src": setup.src }),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArguments(_)), "{err}");
}
