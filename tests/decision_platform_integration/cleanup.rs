//! Deletes what the suite made, and the workspace.

use std::time::Duration;

use fico_sdk::decision::platform::{
    EnvironmentUpdateParams, ShadowStopParams, SwitchbackStopParams, WorkspaceUpdateParams,
};
use fico_sdk::decision::{ExperimentStatus, StopIntent};

use crate::environments::Environments;
use crate::suite::Suite;

pub async fn run(suite: &Suite, build_id: &str, environments: &Environments, input_set_id: &str) {
    let platform = &suite.platform;
    let workspace_id = &suite.workspace_id;

    // We clean up the build.
    platform.delete_build(workspace_id, build_id).await.unwrap();

    // Reset the default environment, so that the environments can be
    // deleted.
    platform
        .update_workspace(workspace_id, latest_by_default())
        .await
        .unwrap();
    for environment_id in [&environments.first, &environments.second] {
        platform
            .delete_environment(workspace_id, environment_id)
            .await
            .unwrap();
    }

    // Clean up the input set.
    platform
        .delete_input_set(workspace_id, input_set_id, Default::default())
        .await
        .unwrap();

    // Give the deletions that are still going on time to finish.
    tokio::time::sleep(Duration::from_secs(1)).await;

    // We can delete the workspace.
    platform
        .delete_workspace(workspace_id, Default::default())
        .await
        .unwrap();
}

/// After a stage failed: deletes the workspace, best effort. First it
/// cancels and deletes the shadow and switchback tests that a dropped stage
/// left started, since the platform refuses to delete an environment that
/// one is attached to; then it deletes the environments, which may be
/// locked or the default; then the workspace, and waits until it is gone,
/// since the platform deletes it in the background. Prints what fails, and
/// the workspace's ID if it is left.
pub async fn after_failure(suite: &Suite) {
    let platform = &suite.platform;
    let workspace_id = &suite.workspace_id;
    let report =
        |what: &str, error: fico_sdk::Error| suite.log(&format!("cleanup: {what}: {error}"));
    let cancel = || StopIntent::Cancel;
    match platform.list_shadows(workspace_id).await {
        Ok(shadows) => {
            for shadow_id in shadows.into_iter().filter_map(|shadow| {
                let started = shadow.status == Some(ExperimentStatus::Started);
                shadow.shadow_id.filter(|_| started)
            }) {
                let params = ShadowStopParams {
                    intent: cancel(),
                    polling: None,
                };
                if let Err(error) = platform.stop_shadow(workspace_id, &shadow_id, params).await {
                    report(&format!("stop {shadow_id}"), error);
                }
            }
        }
        Err(error) => report("list the shadow tests", error),
    }
    match platform.list_switchbacks(workspace_id).await {
        Ok(switchbacks) => {
            for switchback_id in switchbacks.into_iter().filter_map(|switchback| {
                let started = switchback.status == Some(ExperimentStatus::Started);
                switchback.switchback_id.filter(|_| started)
            }) {
                let params = SwitchbackStopParams {
                    intent: cancel(),
                    polling: None,
                };
                if let Err(error) = platform
                    .stop_switchback(workspace_id, &switchback_id, params)
                    .await
                {
                    report(&format!("stop {switchback_id}"), error);
                }
            }
        }
        Err(error) => report("list the switchback tests", error),
    }
    if let Err(error) = platform
        .update_workspace(workspace_id, latest_by_default())
        .await
    {
        report("reset the default environment", error);
    }
    match platform.list_environments(workspace_id).await {
        Ok(environments) => {
            // `latest` is the platform's own.
            let environments = environments
                .into_iter()
                .filter(|environment| environment.environment_id != "latest");
            for environment in environments {
                let environment_id = &environment.environment_id;
                if environment.locked {
                    let params = EnvironmentUpdateParams {
                        locked: Some(false),
                        ..Default::default()
                    };
                    if let Err(error) = platform
                        .update_environment(workspace_id, environment_id, params)
                        .await
                    {
                        report(&format!("unlock {environment_id}"), error);
                    }
                }
                delete_environment(suite, environment_id).await;
            }
        }
        Err(error) => report("list the environments", error),
    }
    if let Err(error) = platform
        .delete_workspace(workspace_id, Default::default())
        .await
    {
        return report(&format!("delete {workspace_id}, which is left"), error);
    }
    // The platform answers once the deletion is queued.
    for _ in 0..30 {
        if let Ok(false) = platform.workspace_exists(workspace_id).await {
            return suite.log(&format!("cleanup: deleted {workspace_id}"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    suite.log(&format!("cleanup: {workspace_id} is left"));
}

/// Deletes the environment `environment_id`, trying again for a while: a
/// shadow or switchback test that was just stopped is detached from it in
/// the background.
async fn delete_environment(suite: &Suite, environment_id: &str) {
    let platform = &suite.platform;
    let mut tries = 0;
    loop {
        match platform
            .delete_environment(&suite.workspace_id, environment_id)
            .await
        {
            Ok(()) => return,
            Err(error) => {
                tries += 1;
                if tries == 10 {
                    let message = format!("cleanup: delete {environment_id}: {error}");
                    return suite.log(&message);
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

/// An update that makes `latest` the workspace's default environment.
fn latest_by_default() -> WorkspaceUpdateParams {
    WorkspaceUpdateParams {
        default_environment_id: Some("latest".to_owned()),
        ..Default::default()
    }
}
