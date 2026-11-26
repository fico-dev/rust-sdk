//! Managed input operations.

use fico_sdk::decision::platform::{ManagedInputCreateParams, ManagedInputUpdateParams};

use crate::runs::Runs;
use crate::suite::Suite;

pub async fn run(suite: &Suite, runs: &Runs) {
    let platform = &suite.platform;
    let workspace_id = &suite.workspace_id;

    // We can create managed inputs from runs.
    let mut created = Vec::new();
    for run_id in &runs.succeeded {
        let params = ManagedInputCreateParams {
            run_id: Some(run_id.clone()),
            content_format: suite.content_format(),
            ..Default::default()
        };
        let managed_input = platform
            .create_managed_input(workspace_id, params)
            .await
            .unwrap();
        created.push(managed_input.managed_input_id);
    }

    // We can list managed inputs.
    let managed_inputs = platform.list_managed_inputs(workspace_id).await.unwrap();
    assert!(managed_inputs.len() >= 2);
    for managed_input_id in &created {
        assert!(
            managed_inputs
                .iter()
                .any(|m| &m.managed_input_id == managed_input_id)
        );
    }

    // We can get a managed input.
    let managed_input = platform
        .get_managed_input(workspace_id, &created[0])
        .await
        .unwrap();
    assert_eq!(managed_input.managed_input_id, created[0]);

    // We can update a managed input.
    let name = "A soft and tender input";
    let description = "Input fit for a bunny's delicate palate";
    let params = ManagedInputUpdateParams {
        name: Some(name.to_owned()),
        description: Some(description.to_owned()),
    };
    let managed_input = platform
        .update_managed_input(workspace_id, &created[0], params)
        .await
        .unwrap();
    assert_eq!(managed_input.name.as_deref(), Some(name));
    assert_eq!(managed_input.description.as_deref(), Some(description));

    // We can delete managed inputs.
    let managed_inputs = platform.list_managed_inputs(workspace_id).await.unwrap();
    for managed_input in managed_inputs {
        platform
            .delete_managed_input(workspace_id, &managed_input.managed_input_id)
            .await
            .unwrap();
    }
}
