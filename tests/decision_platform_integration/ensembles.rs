//! Ensemble definition operations and an ensemble run.

use fico_sdk::decision::platform::{EnsembleCreateParams, EnsembleUpdateParams, RunCreateParams};
use fico_sdk::decision::{
    EvaluationRule, RuleObjective, RuleTolerance, RuleToleranceType, RunConfiguration, RunGroup,
    RunType, RunTypeConfiguration,
};

use crate::environments::Environments;
use crate::suite::Suite;

pub async fn run(suite: &Suite, environments: &Environments) {
    let platform = &suite.platform;
    let workspace_id = &suite.workspace_id;

    // We can create an ensemble definition.
    let params = EnsembleCreateParams {
        run_groups: vec![RunGroup {
            group_id: "run-group-1".to_owned(),
            environment_id: environments.first.clone(),
            repetitions: Some(2),
            ..Default::default()
        }],
        rules: vec![EvaluationRule {
            rule_id: "eval-rule-1".to_owned(),
            metrics_path: suite.metrics_path().to_owned(),
            objective: RuleObjective::Maximize,
            tolerance: RuleTolerance {
                value: 0.01,
                tolerance_type: RuleToleranceType::Absolute,
            },
            index: 0,
        }],
        ..Default::default()
    };
    let definition = platform
        .create_ensemble(workspace_id, params)
        .await
        .unwrap();
    let ensemble_id = definition.ensemble_id;

    // We can get an ensemble definition.
    let definition = platform
        .get_ensemble(workspace_id, &ensemble_id)
        .await
        .unwrap();
    assert_eq!(definition.ensemble_id, ensemble_id);

    // We can list ensemble definitions.
    let definitions = platform.list_ensembles(workspace_id).await.unwrap();
    assert!(!definitions.is_empty());
    assert!(definitions.iter().any(|d| d.ensemble_id == ensemble_id));

    // We can update an ensemble definition.
    let name = "Bunny Ensemble";
    let description = "Ensemble for bunny runs";
    let params = EnsembleUpdateParams {
        name: Some(name.to_owned()),
        description: Some(description.to_owned()),
    };
    let definition = platform
        .update_ensemble(workspace_id, &ensemble_id, params)
        .await
        .unwrap();
    assert_eq!(definition.name, name);
    assert_eq!(definition.description, description);

    // We can start an ensemble run.
    let params = RunCreateParams {
        configuration: Some(RunConfiguration {
            run_type: Some(RunTypeConfiguration {
                run_type: Some(RunType::Ensemble),
                definition_id: Some(ensemble_id.clone()),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..suite.input().create()
    };
    let created = platform.create_run(workspace_id, params).await.unwrap();
    let result = suite.assert_succeeded(&created.run_id).await;
    let run_type = result.information.metadata.run_type.as_ref().unwrap();
    assert_eq!(run_type.run_type, Some(RunType::Ensemble));
    assert_eq!(run_type.definition_id.as_ref(), Some(&ensemble_id));
    assert!(result.ensemble.is_some());

    // We can delete an ensemble definition.
    platform
        .delete_ensemble(workspace_id, &ensemble_id)
        .await
        .unwrap();
}
