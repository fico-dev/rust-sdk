//! Ensemble definitions, on `v1/applications/{workspace_id}/ensembles`.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::Platform;
use crate::decision::id::safe_id;
use crate::decision::path::api_path;
use crate::decision::{
    EnsembleDefinition, EvaluationRule, RuleObjective, RuleToleranceType, RunGroup,
};
use crate::tool::{InWorkspace, NoParams, Tool};
use crate::{Error, Result};

/// Tool arguments that address an ensemble definition, plus the operation's
/// parameters.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct EnsembleRef<P> {
    /// ID of the workspace.
    pub(crate) workspace_id: String,
    /// ID of the ensemble definition.
    pub(crate) ensemble_id: String,
    #[serde(flatten)]
    pub(crate) params: P,
}

/// Parameters for [`Platform::create_ensemble`].
///
/// `run_groups` and `rules` are required in tool arguments. The `Default`
/// value leaves them empty, which [`Platform::create_ensemble`] rejects, so
/// set both.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
pub struct EnsembleCreateParams {
    /// ID of the ensemble definition. Generated when not set.
    #[serde(default)]
    pub ensemble_id: Option<String>,
    /// Name of the ensemble definition. Defaults to its ID.
    #[serde(default)]
    pub name: Option<String>,
    /// Description of the ensemble definition. Defaults to its name.
    #[serde(default)]
    pub description: Option<String>,
    /// Groups of child runs that each ensemble run starts. Needs at least
    /// one.
    pub run_groups: Vec<RunGroup>,
    /// Rules that pick the result of an ensemble run from its child runs.
    /// Needs at least one.
    pub rules: Vec<EvaluationRule>,
}

/// Parameters for [`Platform::update_ensemble`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct EnsembleUpdateParams {
    /// New name of the ensemble definition.
    pub name: Option<String>,
    /// New description of the ensemble definition.
    pub description: Option<String>,
}

impl Platform {
    /// Creates an ensemble definition.
    ///
    /// An unset or empty `ensemble_id` is generated, an unset or empty
    /// `name` defaults to the ID, and an unset or empty `description`
    /// defaults to the name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`], before any request, if `run_groups` or
    /// `rules` is empty, a run group has an empty `group_id` or
    /// `environment_id`, a rule has an empty `rule_id` or `metrics_path` or
    /// an `Unknown` objective or tolerance type, or two rules have the same
    /// `rule_id`.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn create_ensemble(
        &self,
        workspace_id: &str,
        params: EnsembleCreateParams,
    ) -> Result<EnsembleDefinition> {
        check_ensemble(&params.run_groups, &params.rules)?;
        let ensemble_id = match params
            .ensemble_id
            .filter(|ensemble_id| !ensemble_id.is_empty())
        {
            Some(ensemble_id) => ensemble_id,
            None => safe_id("ensemble"),
        };
        let name = match params.name.filter(|name| !name.is_empty()) {
            Some(name) => name,
            None => ensemble_id.clone(),
        };
        let description = match params.description.filter(|d| !d.is_empty()) {
            Some(description) => description,
            None => name.clone(),
        };
        let body = json!({
            "id": ensemble_id,
            "run_groups": params.run_groups,
            "rules": params.rules,
            "name": name,
            "description": description,
        });
        let path = api_path(&["v1", "applications", workspace_id, "ensembles"])?;
        self.client.post(path).json(&body).send_json().await
    }

    /// Gets an ensemble definition.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Status`] with status 404 if the ensemble definition
    /// does not exist, and the [errors of every method](Platform#errors).
    pub async fn get_ensemble(
        &self,
        workspace_id: &str,
        ensemble_id: &str,
    ) -> Result<EnsembleDefinition> {
        let path = api_path(&["v1", "applications", workspace_id, "ensembles", ensemble_id])?;
        self.client.get(path).send_json().await
    }

    /// Lists the ensemble definitions of a workspace.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn list_ensembles(&self, workspace_id: &str) -> Result<Vec<EnsembleDefinition>> {
        let path = api_path(&["v1", "applications", workspace_id, "ensembles"])?;
        self.client.get(path).paginate().collect().await
    }

    /// Updates an ensemble definition.
    ///
    /// Fields that `params` leaves unset keep their values. Only the name
    /// and description can change: to change the run groups or rules,
    /// delete the ensemble definition and create it again.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `params` sets neither `name` nor
    /// `description`.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn update_ensemble(
        &self,
        workspace_id: &str,
        ensemble_id: &str,
        params: EnsembleUpdateParams,
    ) -> Result<EnsembleDefinition> {
        let mut body = Map::new();
        if let Some(name) = params.name {
            body.insert("name".to_owned(), Value::String(name));
        }
        if let Some(description) = params.description {
            body.insert("description".to_owned(), Value::String(description));
        }
        if body.is_empty() {
            return Err(Error::Invalid {
                reason: "an update needs `name` or `description`".to_owned(),
            });
        }
        let path = api_path(&["v1", "applications", workspace_id, "ensembles", ensemble_id])?;
        self.client.patch(path).json(&body).send_json().await
    }

    /// Deletes an ensemble definition.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn delete_ensemble(&self, workspace_id: &str, ensemble_id: &str) -> Result<()> {
        let path = api_path(&["v1", "applications", workspace_id, "ensembles", ensemble_id])?;
        self.client.delete(path).send().await?;
        Ok(())
    }
}

/// The checks of a new ensemble definition that the platform makes after
/// a request, or not at all: rule IDs are unique, as the docs say, though
/// the platform checks only the indices.
fn check_ensemble(run_groups: &[RunGroup], rules: &[EvaluationRule]) -> Result<()> {
    let invalid = |reason: String| Err(Error::Invalid { reason });
    if run_groups.is_empty() {
        return invalid("`run_groups` needs at least one run group".to_owned());
    }
    if rules.is_empty() {
        return invalid("`rules` needs at least one rule".to_owned());
    }
    for (i, group) in run_groups.iter().enumerate() {
        if group.group_id.is_empty() {
            return invalid(format!("`run_groups[{i}]` needs a `group_id` (`id`)"));
        }
        if group.environment_id.is_empty() {
            return invalid(format!(
                "`run_groups[{i}]` needs an `environment_id` (`instance_id`)"
            ));
        }
    }
    let mut rule_ids = BTreeSet::new();
    for (i, rule) in rules.iter().enumerate() {
        if rule.rule_id.is_empty() {
            return invalid(format!("`rules[{i}]` needs a `rule_id` (`id`)"));
        }
        if rule.metrics_path.is_empty() {
            return invalid(format!(
                "`rules[{i}]` needs a `metrics_path` (`statistics_path`)"
            ));
        }
        if rule.objective == RuleObjective::Unknown {
            return invalid(format!(
                "`rules[{i}].objective` must be `maximize` or `minimize`"
            ));
        }
        if rule.tolerance.tolerance_type == RuleToleranceType::Unknown {
            return invalid(format!(
                "`rules[{i}].tolerance.type` must be `absolute` or `relative`"
            ));
        }
        if !rule_ids.insert(rule.rule_id.as_str()) {
            return invalid(format!("more than one rule has the ID `{}`", rule.rule_id));
        }
    }
    Ok(())
}

pub(crate) fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "decision_platform_ensemble_create",
            "Creates an ensemble definition.",
            |cx, a: InWorkspace<EnsembleCreateParams>| async move {
                cx.platform()?
                    .create_ensemble(&a.workspace_id, a.params)
                    .await
            },
        ),
        Tool::new(
            "decision_platform_ensemble_get",
            "Gets an ensemble definition.",
            |cx, a: EnsembleRef<NoParams>| async move {
                cx.platform()?
                    .get_ensemble(&a.workspace_id, &a.ensemble_id)
                    .await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_ensemble_list",
            "Lists the ensemble definitions of a workspace.",
            |cx, a: InWorkspace<NoParams>| async move {
                cx.platform()?.list_ensembles(&a.workspace_id).await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_ensemble_update",
            "Updates an ensemble definition.",
            |cx, a: EnsembleRef<EnsembleUpdateParams>| async move {
                cx.platform()?
                    .update_ensemble(&a.workspace_id, &a.ensemble_id, a.params)
                    .await
            },
        )
        .destructive(),
        Tool::new(
            "decision_platform_ensemble_delete",
            "Deletes an ensemble definition.",
            |cx, a: EnsembleRef<NoParams>| async move {
                cx.platform()?
                    .delete_ensemble(&a.workspace_id, &a.ensemble_id)
                    .await
            },
        )
        .destructive(),
    ]
}
