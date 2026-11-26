//! Workspace operations.

use fico_sdk::decision::platform::{WorkspaceCreateParams, WorkspaceUpdateParams};

use crate::suite::Suite;

pub async fn run(suite: &Suite) {
    let platform = &suite.platform;
    let workspace_id = &suite.workspace_id;

    // We can create the workspace.
    let params = WorkspaceCreateParams {
        workspace_id: Some(workspace_id.clone()),
        ..Default::default()
    };
    let workspace = platform.create_workspace(params).await.unwrap();
    assert_eq!(&workspace.workspace_id, workspace_id);

    // We can get the workspace, and it exists.
    let workspace = platform.get_workspace(workspace_id).await.unwrap();
    assert_eq!(&workspace.workspace_id, workspace_id);
    assert!(platform.workspace_exists(workspace_id).await.unwrap());

    // We can update the workspace.
    let name = "Fluffy cotton tail";
    let description = "A burrow of bunnies";
    let params = WorkspaceUpdateParams {
        name: Some(name.to_owned()),
        description: Some(description.to_owned()),
        ..Default::default()
    };
    let workspace = platform
        .update_workspace(workspace_id, params)
        .await
        .unwrap();
    assert_eq!(workspace.name.as_deref(), Some(name));
    assert_eq!(workspace.description.as_deref(), Some(description));

    // We can list workspaces.
    let workspaces = platform.list_workspaces().await.unwrap();
    assert!(!workspaces.is_empty());
    assert!(workspaces.iter().any(|w| &w.workspace_id == workspace_id));

    // Creating it again with `exist_ok` returns it.
    let params = WorkspaceCreateParams {
        workspace_id: Some(workspace_id.clone()),
        exist_ok: true,
        ..Default::default()
    };
    let workspace = platform.create_workspace(params).await.unwrap();
    assert_eq!(workspace.name.as_deref(), Some(name));
}
