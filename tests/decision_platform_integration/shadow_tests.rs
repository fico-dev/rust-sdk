//! Shadow test operations.

use fico_sdk::decision::platform::{
    RunCreateParams, ShadowCreateParams, ShadowStopParams, ShadowUpdateParams,
};
use fico_sdk::decision::{ExperimentStatus, StopIntent, TerminationEvents, TestComparison};

use crate::environments::Environments;
use crate::suite::{Suite, join_all};

pub async fn run(suite: &Suite, environments: &Environments) {
    let platform = &suite.platform;
    let workspace_id = &suite.workspace_id;
    let status = |shadow_id: String| async move {
        platform
            .get_shadow_metadata(workspace_id, &shadow_id)
            .await
            .unwrap()
            .status
    };

    // We can create a shadow test in draft mode. Its baseline is the second
    // environment: the other stages start their runs at the same time,
    // mostly on the first one, the default, and the test would reach its 4
    // runs and end before it is stopped.
    let (baseline, candidate) = (&environments.second, &environments.first);
    let params = ShadowCreateParams {
        shadow_id: None,
        name: None,
        description: None,
        comparisons: vec![TestComparison {
            baseline_environment_id: baseline.clone(),
            candidate_environment_ids: vec![candidate.clone()],
        }],
        termination_events: TerminationEvents {
            maximum_runs: 4,
            ..Default::default()
        },
        start_events: None,
    };
    let shadow = platform.create_shadow(workspace_id, params).await.unwrap();
    let shadow_id = shadow.shadow_id.expect("the shadow test has an ID");
    assert_eq!(
        status(shadow_id.clone()).await,
        Some(ExperimentStatus::Draft)
    );

    // We can get a shadow test.
    let shadow = platform.get_shadow(workspace_id, &shadow_id).await.unwrap();
    assert_eq!(shadow.shadow_id.as_ref(), Some(&shadow_id));

    // We can list shadow tests.
    let shadows = platform.list_shadows(workspace_id).await.unwrap();
    assert!(!shadows.is_empty());
    assert!(
        shadows
            .iter()
            .any(|s| s.shadow_id.as_ref() == Some(&shadow_id))
    );

    // We can update a shadow test.
    let name = "Hoppy Shadow & Light";
    let description = "A shadow test for hoppy bunnies";
    let params = ShadowUpdateParams {
        name: Some(name.to_owned()),
        description: Some(description.to_owned()),
    };
    let shadow = platform
        .update_shadow(workspace_id, &shadow_id, params)
        .await
        .unwrap();
    assert_eq!(shadow.name.as_deref(), Some(name));
    assert_eq!(shadow.description.as_deref(), Some(description));

    // We can start a shadow test.
    platform
        .start_shadow(workspace_id, &shadow_id)
        .await
        .unwrap();
    assert_eq!(
        status(shadow_id.clone()).await,
        Some(ExperimentStatus::Started)
    );

    // Do some runs to let the shadow test collect data.
    experiment_runs(suite, baseline).await;

    // Stop the shadow test.
    let params = ShadowStopParams {
        intent: StopIntent::Complete,
        polling: None,
    };
    platform
        .stop_shadow(workspace_id, &shadow_id, params)
        .await
        .unwrap();
    let stopped = status(shadow_id.clone()).await;
    assert!(
        matches!(
            stopped,
            // Stopping is asynchronous: the test may still be stopping.
            Some(ExperimentStatus::Completed | ExperimentStatus::Stopping)
        ),
        "{stopped:?}"
    );

    // The shadow test registered the runs.
    let shadow = platform.get_shadow(workspace_id, &shadow_id).await.unwrap();
    assert!(shadow.runs.is_some_and(|runs| !runs.is_empty()));

    // We can delete a shadow test.
    platform
        .delete_shadow(workspace_id, &shadow_id, Default::default())
        .await
        .unwrap();
}

/// Starts 2 runs on the environment `environment_id`, for an experiment to
/// collect, and waits for them to succeed.
pub async fn experiment_runs(suite: &Suite, environment_id: &str) {
    let mut run_ids = Vec::new();
    for _ in 0..2 {
        let params = RunCreateParams {
            environment_id: Some(environment_id.to_owned()),
            ..suite.input().create()
        };
        let created = suite
            .platform
            .create_run(&suite.workspace_id, params)
            .await
            .unwrap();
        run_ids.push(created.run_id);
    }
    join_all(run_ids.iter().map(|run_id| suite.assert_succeeded(run_id))).await;
}
