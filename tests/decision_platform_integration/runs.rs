//! Runs started in different ways, then their input, output, logs and
//! information, the run list, a comparison, the logs of a run followed while
//! it runs, and runs with the SDK's other inputs.

use fico_sdk::decision::RunStatus;
use fico_sdk::decision::platform::{
    ManagedInputCreateParams, RunCompareParams, RunCreateParams, RunListParams, RunLogsFollowParams,
};
use serde_json::{Value, json};

use crate::environments::Environments;
use crate::suite::{Content, Input, Suite, join_all, polling};

/// The runs that the later stages use.
pub struct Runs {
    /// The 7 runs started in different ways, which the input set holds.
    pub run_ids: Vec<String>,
    /// Those of the 7 runs that succeeded.
    pub succeeded: Vec<String>,
    /// The first run, on the fixture's input with the default options.
    pub first: String,
}

pub async fn run(suite: &Suite, environments: &Environments) -> Runs {
    let platform = &suite.platform;
    let workspace_id = &suite.workspace_id;
    let input = suite.input();

    // Prepare the uploads and the managed input for the runs.
    let upload_for_run = platform.create_upload(workspace_id).await.unwrap();
    platform
        .upload_data(suite.upload(upload_for_run.upload_url))
        .await
        .unwrap();
    let upload_for_managed_input = platform.create_upload(workspace_id).await.unwrap();
    platform
        .upload_data(suite.upload(upload_for_managed_input.upload_url))
        .await
        .unwrap();
    let params = ManagedInputCreateParams {
        upload_id: Some(upload_for_managed_input.upload_id),
        content_format: suite.content_format(),
        ..Default::default()
    };
    let managed_input = platform
        .create_managed_input(workspace_id, params)
        .await
        .unwrap();

    // Start runs in different ways.
    let on = |environment_id: &str| RunCreateParams {
        environment_id: Some(environment_id.to_owned()),
        ..input.create()
    };
    let starts = [
        input.create(),
        on(&environments.first),
        on(&environments.second),
        on("latest"),
        RunCreateParams {
            options: suite.options(),
            ..input.create()
        },
        RunCreateParams {
            upload_id: Some(upload_for_run.upload_id),
            ..Default::default()
        },
        RunCreateParams {
            managed_input_id: Some(managed_input.managed_input_id),
            ..Default::default()
        },
    ];
    let mut run_ids = Vec::new();
    for params in starts {
        let created = platform.create_run(workspace_id, params).await.unwrap();
        run_ids.push(created.run_id);
    }

    // And with the SDK's other inputs: a file, and for JSON, an input over
    // the 5 MiB that the platform takes inline, which the SDK uploads.
    let mut others = vec![(
        RunCreateParams {
            input_file: Some(suite.input_file()),
            ..Default::default()
        },
        input.clone(),
    )];
    if suite.content == Content::Json {
        let mut large = input_json(&input);
        large["padding"] = Value::String("x".repeat(6 << 20));
        let large = Input::Json(large);
        others.push((large.create(), large));
    }
    let mut checks: Vec<(String, Input)> = run_ids
        .iter()
        .map(|run_id| (run_id.clone(), input.clone()))
        .collect();
    for (params, input) in others {
        let created = platform.create_run(workspace_id, params).await.unwrap();
        checks.push((created.run_id, input));
    }

    // We can follow the first run's logs while it runs, and perform
    // different checks on the runs.
    let followed = platform.follow_run_logs(
        workspace_id,
        &run_ids[0],
        RunLogsFollowParams { polling: polling() },
        |_| {},
    );
    let (followed, results) = tokio::join!(
        followed,
        join_all(
            checks
                .iter()
                .map(|(run_id, input)| suite.assert_run(run_id, input))
        ),
    );
    assert!(!followed.unwrap().is_empty(), "no logs were followed");
    let succeeded = results[..run_ids.len()]
        .iter()
        .filter(|result| result.information.metadata.status == RunStatus::Succeeded)
        .map(|result| result.information.id.clone())
        .collect();

    // We can list runs.
    let runs = platform
        .list_runs(workspace_id, RunListParams::default())
        .await
        .unwrap();
    assert!(runs.len() >= run_ids.len());
    for (run_id, _) in &checks {
        assert!(
            runs.iter().any(|r| &r.id == run_id),
            "{run_id} is not listed"
        );
    }

    // We can list runs with a particular status.
    let params = RunListParams {
        status: Some(RunStatus::Succeeded),
        ..Default::default()
    };
    let runs = platform.list_runs(workspace_id, params).await.unwrap();
    assert!(!runs.is_empty());

    // We can compare the run with the default options with the one that
    // set another value.
    let params = RunCompareParams {
        run_ids: vec![run_ids[0].clone(), run_ids[4].clone()],
    };
    let comparison = platform.compare_runs(workspace_id, params).await.unwrap();
    assert_eq!(comparison.run_ids, [run_ids[0].clone(), run_ids[4].clone()]);
    let (name, _) = suite.option();
    let options = comparison.options.unwrap_or_default();
    let option = options
        .get(name)
        .unwrap_or_else(|| panic!("`{name}` is not compared: {options:?}"));
    assert!(option.has_differences, "{option:?}");

    Runs {
        first: run_ids[0].clone(),
        run_ids,
        succeeded,
    }
}

/// The JSON object of `input`.
fn input_json(input: &Input) -> Value {
    match input {
        Input::Json(input) => input.clone(),
        Input::Dir(_) => json!({}),
    }
}
