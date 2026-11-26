//! Tracked runs: runs that ran outside the platform, recorded with their
//! input, output and logs, from values and from files; a failed one; one in
//! the default environment; a clone of one, which runs on the platform; and a
//! deleted one.

use std::fs;

use fico_sdk::Error;
use fico_sdk::decision::platform::{RunCloneParams, RunLogsGetParams, RunTrackParams};
use fico_sdk::decision::{RunStatus, RunType, TrackedRun, TrackedRunStatus};
use serde_json::json;

use crate::environments::Environments;
use crate::suite::{Content, Input, Suite, join_all};

pub async fn run(suite: &Suite, environments: &Environments) {
    let platform = &suite.platform;
    let workspace_id = &suite.workspace_id;
    let input = suite.input();
    let logs = "Hello, world\n";

    // The run, as it ran outside the platform.
    let ran = match &input {
        Input::Json(input) => TrackedRun {
            input: Some(input.clone()),
            output: Some(json!({
                "solution": { "message": "Hello, world" },
                "metrics": { "value": 1.23 },
            })),
            logs: Some(logs.to_owned()),
            duration: Some(1_000),
            ..Default::default()
        },
        Input::Dir(dir) => {
            let output = suite.dir("tracked-output");
            fs::create_dir_all(output.join("solutions")).unwrap();
            fs::write(
                output.join("solutions/assignments.csv"),
                "knapsack_id,item_id\n",
            )
            .unwrap();
            TrackedRun {
                input_dir_path: Some(dir.clone()),
                output_dir_path: Some(output),
                logs: Some(logs.to_owned()),
                metrics: json!({ "value": 523.0 }).as_object().cloned(),
                duration: Some(1_000),
                ..Default::default()
            }
        }
    };
    let failed = TrackedRun {
        status: TrackedRunStatus::Failed,
        error: Some("infeasible".to_owned()),
        output: None,
        output_dir_path: None,
        metrics: None,
        ..ran.clone()
    };
    let mut tracked = vec![
        (ran.clone(), None, RunStatus::Succeeded),
        (failed, None, RunStatus::Failed),
        (ran.clone(), Some("default"), RunStatus::Succeeded),
    ];
    if suite.content == Content::Json {
        // The same run, from files.
        let dir = suite.dir("tracked-files");
        let output = dir.join("output.json");
        fs::write(&output, serde_json::to_vec(&ran.output).unwrap()).unwrap();
        let logs_file = dir.join("logs.txt");
        fs::write(&logs_file, logs).unwrap();
        let from_files = TrackedRun {
            input_file: Some(suite.input_file()),
            output_file: Some(output),
            logs_file: Some(logs_file),
            duration: Some(1_000),
            ..Default::default()
        };
        tracked.push((from_files, None, RunStatus::Succeeded));
    }

    // We can track the runs.
    let mut run_ids = Vec::new();
    for (tracked_run, environment_id, _) in &tracked {
        let params = RunTrackParams {
            tracked_run: tracked_run.clone(),
            environment_id: environment_id.map(str::to_owned),
            ..Default::default()
        };
        let created = platform.track_run(workspace_id, params).await.unwrap();
        run_ids.push(created.run_id);
    }

    // A tracked run is external, with its status, its input and its logs.
    let results = join_all(run_ids.iter().map(|run_id| suite.wait(run_id))).await;
    for ((result, _), (_, environment_id, status)) in results.iter().zip(&tracked) {
        let run_id = &result.information.id;
        let metadata = &result.information.metadata;
        assert_eq!(metadata.status, *status, "{run_id}");
        let run_type = metadata.run_type.as_ref().and_then(|r| r.run_type);
        assert_eq!(run_type, Some(RunType::External), "{run_id}");
        if environment_id.is_some() {
            assert_eq!(metadata.environment_id, environments.first, "{run_id}");
        }
        suite.assert_input(run_id, &input).await;
        let tracked_logs = platform
            .get_run_logs(workspace_id, run_id, RunLogsGetParams::default())
            .await
            .unwrap();
        assert!(tracked_logs.log.contains("Hello, world"), "{run_id}");
    }

    // We can clone a tracked run: the clone runs on the platform, with the
    // tracked input.
    let created = platform
        .clone_run(workspace_id, &run_ids[0], RunCloneParams::default())
        .await
        .unwrap();
    suite.assert_succeeded(&created.run_id).await;
    suite.assert_input(&created.run_id, &input).await;

    // We can delete a run, and then it is not found. The run is a tracked
    // run in `latest`: the platform refuses to delete a run that an
    // experiment holds, and the experiments, which run at the same time,
    // hold runs of the suite's other environments.
    platform
        .delete_run(workspace_id, &run_ids[0], Default::default())
        .await
        .unwrap();
    let error = platform
        .get_run_information(workspace_id, &run_ids[0])
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::Status { status: 404, .. }),
        "{error}"
    );
}
