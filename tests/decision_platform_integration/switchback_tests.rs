//! Switchback test operations.

use fico_sdk::decision::platform::{
    SwitchbackCreateParams, SwitchbackStopParams, SwitchbackUpdateParams,
};
use fico_sdk::decision::{ExperimentStatus, StopIntent, TestComparisonSingle};

use crate::environments::Environments;
use crate::shadow_tests::experiment_runs;
use crate::suite::Suite;

pub async fn run(suite: &Suite, environments: &Environments) {
    let platform = &suite.platform;
    let workspace_id = &suite.workspace_id;
    let status = |switchback_id: String| async move {
        platform
            .get_switchback_metadata(workspace_id, &switchback_id)
            .await
            .unwrap()
            .status
    };

    // We can create a switchback test in draft mode.
    let params = SwitchbackCreateParams {
        switchback_id: None,
        name: None,
        description: None,
        comparison: TestComparisonSingle {
            baseline_environment_id: environments.first.clone(),
            candidate_environment_id: environments.second.clone(),
        },
        unit_duration_minutes: 1.0,
        units: 1,
        start: None,
    };
    let switchback = platform
        .create_switchback(workspace_id, params)
        .await
        .unwrap();
    let switchback_id = switchback
        .switchback_id
        .expect("the switchback test has an ID");
    assert_eq!(
        status(switchback_id.clone()).await,
        Some(ExperimentStatus::Draft)
    );

    // We can get a switchback test.
    let switchback = platform
        .get_switchback(workspace_id, &switchback_id)
        .await
        .unwrap();
    assert_eq!(switchback.switchback_id.as_ref(), Some(&switchback_id));

    // We can list switchback tests.
    let switchbacks = platform.list_switchbacks(workspace_id).await.unwrap();
    assert!(!switchbacks.is_empty());
    assert!(
        switchbacks
            .iter()
            .any(|s| s.switchback_id.as_ref() == Some(&switchback_id))
    );

    // We can update a switchback test.
    let name = "Hoppy switchback & Light";
    let description = "A switchback test for hoppy bunnies";
    let params = SwitchbackUpdateParams {
        name: Some(name.to_owned()),
        description: Some(description.to_owned()),
    };
    let switchback = platform
        .update_switchback(workspace_id, &switchback_id, params)
        .await
        .unwrap();
    assert_eq!(switchback.name.as_deref(), Some(name));
    assert_eq!(switchback.description.as_deref(), Some(description));

    // We can start a switchback test.
    platform
        .start_switchback(workspace_id, &switchback_id)
        .await
        .unwrap();
    assert_eq!(
        status(switchback_id.clone()).await,
        Some(ExperimentStatus::Started)
    );

    // Do some runs to let the switchback test collect data.
    experiment_runs(suite, &environments.first).await;

    // Stop the switchback test.
    let params = SwitchbackStopParams {
        intent: StopIntent::Complete,
        polling: None,
    };
    platform
        .stop_switchback(workspace_id, &switchback_id, params)
        .await
        .unwrap();
    let stopped = status(switchback_id.clone()).await;
    assert!(
        matches!(
            stopped,
            // Stopping is asynchronous: the test may still be stopping.
            Some(ExperimentStatus::Completed | ExperimentStatus::Stopping)
        ),
        "{stopped:?}"
    );

    // The switchback test registered the runs.
    let switchback = platform
        .get_switchback(workspace_id, &switchback_id)
        .await
        .unwrap();
    assert!(switchback.runs.is_some_and(|runs| !runs.is_empty()));

    // We can delete a switchback test.
    platform
        .delete_switchback(workspace_id, &switchback_id, Default::default())
        .await
        .unwrap();
}
