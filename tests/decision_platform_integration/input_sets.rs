//! Input set operations.

use fico_sdk::decision::platform::{InputSetCreateParams, InputSetUpdateParams};

use crate::environments::Environments;
use crate::runs::Runs;
use crate::suite::Suite;
use crate::support::{rfc3339, unix_now};

/// Returns the input set that the experiments use.
pub async fn run(suite: &Suite, environments: &Environments, runs: &Runs) -> String {
    let platform = &suite.platform;
    let workspace_id = &suite.workspace_id;
    let now = unix_now();

    // We can create an input set from different methodologies.
    let params = InputSetCreateParams {
        run_ids: Some(runs.run_ids.clone()),
        ..Default::default()
    };
    let first = platform
        .create_input_set(workspace_id, params)
        .await
        .unwrap();
    let params = InputSetCreateParams {
        environment_id: Some(environments.first.clone()),
        maximum_runs: Some(20),
        end_time: Some(rfc3339(now)),
        start_time: Some(rfc3339(now - 24 * 60 * 60)),
        ..Default::default()
    };
    let second = platform
        .create_input_set(workspace_id, params)
        .await
        .unwrap();

    // We can get an input set.
    for input_set in [&first, &second] {
        let got = platform
            .get_input_set(workspace_id, &input_set.input_set_id)
            .await
            .unwrap();
        assert_eq!(got.input_set_id, input_set.input_set_id);
    }

    // We can list input sets.
    let input_sets = platform.list_input_sets(workspace_id).await.unwrap();
    assert!(input_sets.len() >= 2);
    for input_set in [&first, &second] {
        assert!(
            input_sets
                .iter()
                .any(|s| s.input_set_id == input_set.input_set_id)
        );
    }

    // We can update an input set.
    let name = "Bunny Input Set";
    let description = "Input set for all bunny runs";
    let params = InputSetUpdateParams {
        name: Some(name.to_owned()),
        description: Some(description.to_owned()),
        ..Default::default()
    };
    let updated = platform
        .update_input_set(workspace_id, &first.input_set_id, params)
        .await
        .unwrap();
    assert_eq!(updated.name, name);
    assert_eq!(updated.description, description);

    // We can delete an input set.
    platform
        .delete_input_set(workspace_id, &second.input_set_id, Default::default())
        .await
        .unwrap();

    first.input_set_id
}
