//! Environment operations.

use fico_sdk::decision::platform::{
    EnvironmentCreateParams, EnvironmentUpdateParams, WorkspaceUpdateParams,
};

use crate::suite::Suite;

/// The two environments that the later stages use, on the same build.
pub struct Environments {
    /// The workspace's default environment, the baseline of experiments.
    pub first: String,
    /// The candidate of experiments.
    pub second: String,
}

pub async fn run(suite: &Suite, build_id: &str) -> Environments {
    let platform = &suite.platform;
    let workspace_id = &suite.workspace_id;
    let update = |params: EnvironmentUpdateParams, environment_id: String| async move {
        platform
            .update_environment(workspace_id, &environment_id, params)
            .await
            .unwrap()
    };

    // We can create some environments.
    let mut created = Vec::new();
    for _ in 0..3 {
        let environment = platform
            .create_environment(workspace_id, build_id, EnvironmentCreateParams::default())
            .await
            .unwrap();
        created.push(environment.environment_id);
    }
    let [first, second, third] = <[String; 3]>::try_from(created).unwrap();

    // We can list environments.
    let environments = platform.list_environments(workspace_id).await.unwrap();
    assert!(environments.len() >= 3);
    for environment_id in [&first, &second, &third] {
        assert!(
            environments
                .iter()
                .any(|e| &e.environment_id == environment_id)
        );
    }

    // We can get the environment and it exists.
    let environment = platform
        .get_environment(workspace_id, &first)
        .await
        .unwrap();
    assert_eq!(environment.environment_id, first);
    assert!(
        platform
            .environment_exists(workspace_id, &first)
            .await
            .unwrap()
    );

    // We can update the environment.
    let name = "Jumping McJumpface";
    let description = "Loves to hop around";
    let params = EnvironmentUpdateParams {
        name: Some(name.to_owned()),
        description: Some(description.to_owned()),
        ..Default::default()
    };
    let environment = update(params, first.clone()).await;
    assert_eq!(environment.name, name);
    assert_eq!(environment.description, description);

    // We can lock the environment, and unlock it so that the suite can go
    // on.
    let lock = |locked| EnvironmentUpdateParams {
        locked: Some(locked),
        ..Default::default()
    };
    assert!(update(lock(true), first.clone()).await.locked);
    assert!(!update(lock(false), first.clone()).await.locked);

    // We can delete an environment.
    platform
        .delete_environment(workspace_id, &third)
        .await
        .unwrap();
    assert!(
        !platform
            .environment_exists(workspace_id, &third)
            .await
            .unwrap()
    );

    // We can set the default environment of a workspace.
    let params = WorkspaceUpdateParams {
        default_environment_id: Some(first.clone()),
        ..Default::default()
    };
    platform
        .update_workspace(workspace_id, params)
        .await
        .unwrap();

    Environments { first, second }
}
