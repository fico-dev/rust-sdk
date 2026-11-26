//! Build operations.

use fico_sdk::decision::platform::{BuildCreateParams, BuildUpdateParams};

use crate::suite::Suite;

/// Returns the build that the environments use.
pub async fn run(suite: &Suite) -> String {
    let platform = &suite.platform;
    let workspace_id = &suite.workspace_id;

    // We can create a couple of new builds.
    let first = platform
        .create_build(workspace_id, BuildCreateParams::default())
        .await
        .unwrap();
    let second = platform
        .create_build(workspace_id, BuildCreateParams::default())
        .await
        .unwrap();

    // We can list builds.
    let builds = platform.list_builds(workspace_id).await.unwrap();
    assert!(builds.len() >= 2);
    for build in [&first, &second] {
        assert!(builds.iter().any(|b| b.build_id == build.build_id));
    }

    // We can get a build and it exists.
    let build = platform
        .get_build(workspace_id, &first.build_id)
        .await
        .unwrap();
    assert_eq!(build.build_id, first.build_id);
    assert!(
        platform
            .build_exists(workspace_id, &first.build_id)
            .await
            .unwrap()
    );

    // We can update a build.
    let name = "Carrots Galore";
    let description = "A never ending supply of carrots";
    let params = BuildUpdateParams {
        name: Some(name.to_owned()),
        description: Some(description.to_owned()),
    };
    let build = platform
        .update_build(workspace_id, &first.build_id, params)
        .await
        .unwrap();
    assert_eq!(build.name, name);
    assert_eq!(build.description, description);

    // We can delete a build.
    platform
        .delete_build(workspace_id, &second.build_id)
        .await
        .unwrap();
    assert!(
        !platform
            .build_exists(workspace_id, &second.build_id)
            .await
            .unwrap()
    );

    first.build_id
}
