//! Scenario test operations.

use fico_sdk::decision::platform::{ScenarioCreateParams, ScenarioGetParams, ScenarioUpdateParams};
use fico_sdk::decision::{ExperimentStatus, Scenario, ScenarioInput, ScenarioInputType};
use serde_json::Value;

use crate::environments::Environments;
use crate::suite::{Suite, polling};

pub async fn run(suite: &Suite, environments: &Environments, input_set_id: &str) {
    let platform = &suite.platform;
    let workspace_id = &suite.workspace_id;

    // We can create a scenario test from the input set.
    let params = ScenarioCreateParams {
        scenarios: vec![Scenario {
            scenario_input: ScenarioInput {
                scenario_input_type: ScenarioInputType::InputSet,
                scenario_input_data: Value::String(input_set_id.to_owned()),
            },
            environment_id: environments.first.clone(),
            ..Default::default()
        }],
        repetitions: Some(0),
        ..Default::default()
    };
    let scenario_id = platform
        .create_scenario(workspace_id, params)
        .await
        .unwrap()
        .scenario_id;

    // We can list scenario tests.
    let scenario_tests = platform.list_scenarios(workspace_id).await.unwrap();
    assert!(!scenario_tests.is_empty());
    assert!(scenario_tests.iter().any(|s| s.scenario_id == scenario_id));

    // We can get a scenario test.
    let scenario_test = platform
        .get_scenario(workspace_id, &scenario_id, ScenarioGetParams::default())
        .await
        .unwrap();
    assert_eq!(scenario_test.scenario_id, scenario_id);

    // We can get the metadata of a scenario test.
    let metadata = platform
        .get_scenario_metadata(workspace_id, &scenario_id)
        .await
        .unwrap();
    assert!(metadata.number_of_runs.is_some_and(|runs| runs > 0));

    // We can update a scenario test.
    let name = "Cotton-tailed Jumper";
    let description = "A bunny that loves to jump";
    let params = ScenarioUpdateParams {
        name: Some(name.to_owned()),
        description: Some(description.to_owned()),
    };
    let updated = platform
        .update_scenario(workspace_id, &scenario_id, params)
        .await
        .unwrap();
    assert_eq!(updated.name, name);
    assert_eq!(updated.description.as_deref(), Some(description));

    // The scenario test runs to completion.
    let params = ScenarioGetParams {
        polling: Some(polling()),
    };
    let scenario_test = platform
        .get_scenario(workspace_id, &scenario_id, params)
        .await
        .unwrap();
    assert_eq!(scenario_test.status, Some(ExperimentStatus::Completed));

    // We can delete a scenario test.
    platform
        .delete_scenario(workspace_id, &scenario_id, Default::default())
        .await
        .unwrap();
}
