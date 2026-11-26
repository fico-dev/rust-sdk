//! Acceptance test operations.

use fico_sdk::decision::platform::{
    AcceptanceCreateParams, AcceptanceGetParams, AcceptanceUpdateParams,
};
use fico_sdk::decision::{
    Comparison, Metric, MetricParams, MetricTolerance, MetricToleranceType, MetricType,
    StatisticType,
};

use crate::environments::Environments;
use crate::suite::{Suite, polling};

pub async fn run(suite: &Suite, environments: &Environments, input_set_id: &str) {
    let platform = &suite.platform;
    let workspace_id = &suite.workspace_id;

    // We can create an acceptance test and wait for its result.
    let params = AcceptanceCreateParams {
        baseline_environment_id: environments.first.clone(),
        candidate_environment_id: environments.second.clone(),
        metrics: vec![Metric {
            field: "result.value".to_owned(),
            metric_type: MetricType::DirectComparison,
            params: MetricParams {
                operator: Comparison::EqualTo,
                tolerance: MetricTolerance {
                    tolerance_type: MetricToleranceType::Absolute,
                    value: 0.01,
                },
            },
            statistic: StatisticType::Mean,
        }],
        input_set_id: input_set_id.to_owned(),
        ..Default::default()
    };
    let created = platform
        .create_acceptance(workspace_id, params)
        .await
        .unwrap();
    let params = AcceptanceGetParams {
        polling: Some(polling()),
    };
    let acceptance = platform
        .get_acceptance(workspace_id, &created.acceptance_id, params)
        .await
        .unwrap();
    let acceptance_id = acceptance.acceptance_id;

    // We can list acceptance tests.
    let acceptances = platform.list_acceptances(workspace_id).await.unwrap();
    assert!(!acceptances.is_empty());
    assert!(acceptances.iter().any(|a| a.acceptance_id == acceptance_id));

    // We can get an acceptance test.
    let acceptance = platform
        .get_acceptance(workspace_id, &acceptance_id, AcceptanceGetParams::default())
        .await
        .unwrap();
    assert_eq!(acceptance.acceptance_id, acceptance_id);

    // We can update an acceptance test.
    let name = "Hoppy Acceptance & Light";
    let description = "An acceptance test for hoppy bunnies";
    let params = AcceptanceUpdateParams {
        name: Some(name.to_owned()),
        description: Some(description.to_owned()),
    };
    let acceptance = platform
        .update_acceptance(workspace_id, &acceptance_id, params)
        .await
        .unwrap();
    assert_eq!(acceptance.name, name);
    assert_eq!(acceptance.description, description);

    // We can delete an acceptance test.
    platform
        .delete_acceptance(workspace_id, &acceptance_id, Default::default())
        .await
        .unwrap();
}
