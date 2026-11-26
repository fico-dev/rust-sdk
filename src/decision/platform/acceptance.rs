//! Acceptance tests, on
//! `v1/applications/{workspace_id}/experiments/acceptance`.
//!
//! The platform runs every acceptance test's runs in a batch experiment
//! with the test's ID. The SDK does not expose batch experiments, so
//! [`Platform::create_acceptance`] creates it here, privately.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use super::Platform;
use crate::decision::id::safe_id;
use crate::decision::path::api_path;
use crate::decision::polling::{deleting, gone, poll, poll_change};
use crate::decision::{
    AcceptanceTest, Comparison, ExperimentRun, ExperimentStatus, Metric, MetricToleranceType,
    MetricType, PollingOptions, StatisticType,
};
use crate::tool::{InWorkspace, NoParams, Tool};
use crate::{Error, Result};

/// Tool arguments that address an acceptance test, plus the operation's
/// parameters.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct AcceptanceRef<P> {
    /// ID of the workspace.
    pub(crate) workspace_id: String,
    /// ID of the acceptance test.
    pub(crate) acceptance_id: String,
    #[serde(flatten)]
    pub(crate) params: P,
}

/// Parameters for [`Platform::create_acceptance`].
///
/// `baseline_environment_id`, `candidate_environment_id`, `input_set_id`
/// and `metrics` are required in tool arguments. The `Default` value leaves
/// them empty, which [`Platform::create_acceptance`] rejects, so set all
/// four.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
pub struct AcceptanceCreateParams {
    /// ID of the acceptance test. Generated when not set.
    #[serde(default)]
    pub acceptance_id: Option<String>,
    /// Name of the acceptance test. Defaults to its ID.
    #[serde(default)]
    pub name: Option<String>,
    /// Description of the acceptance test.
    #[serde(default)]
    pub description: Option<String>,
    /// ID of the environment to compare the candidate with.
    pub baseline_environment_id: String,
    /// ID of the environment to test.
    pub candidate_environment_id: String,
    /// ID of the input set to test on. Both environments run every input
    /// in it.
    pub input_set_id: String,
    /// The metrics that decide whether the candidate passes. Needs at least
    /// one.
    pub metrics: Vec<Metric>,
}

/// Parameters for [`Platform::get_acceptance`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct AcceptanceGetParams {
    /// Polls until the acceptance test reaches a terminal state, then returns
    /// it. Without it, returns the acceptance test's current state. With the
    /// defaults, polling has no time or try limit.
    pub polling: Option<PollingOptions>,
}

/// Parameters for [`Platform::update_acceptance`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct AcceptanceUpdateParams {
    /// New name of the acceptance test.
    pub name: Option<String>,
    /// New description of the acceptance test.
    pub description: Option<String>,
}

/// Parameters for [`Platform::delete_acceptance`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct AcceptanceDeleteParams {
    /// Polls until the acceptance test's batch experiment is gone. Without
    /// it, returns once the platform accepts its delete, which it finishes
    /// later. With the defaults, polling has no time or try limit.
    pub polling: Option<PollingOptions>,
}

/// The status of a batch experiment, from its GET.
#[derive(Deserialize)]
struct BatchStatus {
    #[serde(default)]
    status: Option<ExperimentStatus>,
}

/// What the platform returns when it creates a batch experiment.
#[derive(Deserialize)]
struct BatchCreated {
    /// ID of the batch experiment (API: `id`).
    #[serde(rename = "id")]
    batch_id: String,
}

impl Platform {
    /// Creates an acceptance test that compares a candidate environment with a baseline.
    ///
    /// It reads the input set, then creates the batch experiment that runs
    /// every input of it on both environments, with the acceptance test's
    /// ID, then the acceptance test on it. An unset or empty
    /// `acceptance_id` is generated, and an unset or empty `name` defaults
    /// to the ID. Follow the test with [`Platform::get_acceptance`] and
    /// `polling`.
    ///
    /// If creating the acceptance test fails, its batch experiment stays,
    /// with the test's ID, so creating the test again with the same
    /// `acceptance_id` fails. Retry without `acceptance_id`, or with a new
    /// one.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] before any request if `input_set_id`,
    /// `baseline_environment_id`, `candidate_environment_id` or `metrics` is
    /// empty, or a metric has an empty `field` or an `Unknown` value, and
    /// after the batch experiment is created if the platform gives it
    /// another ID than the acceptance test's.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn create_acceptance(
        &self,
        workspace_id: &str,
        params: AcceptanceCreateParams,
    ) -> Result<AcceptanceTest> {
        // Without an input set, the test needs an existing batch experiment
        // of the same ID, which the SDK cannot create.
        if params.input_set_id.is_empty() {
            return Err(Error::Invalid {
                reason: "`input_set_id` must be set".to_owned(),
            });
        }
        // The platform would reject these only after the batch experiment
        // exists, and leave it behind.
        for (name, value) in [
            ("baseline_environment_id", &params.baseline_environment_id),
            ("candidate_environment_id", &params.candidate_environment_id),
        ] {
            if value.is_empty() {
                return Err(Error::Invalid {
                    reason: format!("`{name}` must be set"),
                });
            }
        }
        if params.metrics.is_empty() {
            return Err(Error::Invalid {
                reason: "`metrics` needs at least one metric".to_owned(),
            });
        }
        check_metrics(&params.metrics)?;
        let acceptance_id = match params
            .acceptance_id
            .filter(|acceptance_id| !acceptance_id.is_empty())
        {
            Some(acceptance_id) => acceptance_id,
            None => safe_id("acceptance"),
        };
        let name = match params.name.filter(|name| !name.is_empty()) {
            Some(name) => name,
            None => acceptance_id.clone(),
        };
        let input_set = self
            .get_input_set(workspace_id, &params.input_set_id)
            .await?;
        // The input set's run inputs, or else its managed inputs, for the
        // candidate, then for the baseline.
        let input_ids: Vec<String> = if input_set.input_ids.is_empty() {
            input_set
                .inputs
                .into_iter()
                .map(|input| input.managed_input_id)
                .collect()
        } else {
            input_set.input_ids
        };
        let mut runs = Vec::with_capacity(2 * input_ids.len());
        for environment_id in [
            &params.candidate_environment_id,
            &params.baseline_environment_id,
        ] {
            for input_id in &input_ids {
                runs.push(ExperimentRun {
                    input_id: input_id.clone(),
                    environment_id: Some(environment_id.clone()),
                    input_set_id: Some(input_set.input_set_id.clone()),
                    ..ExperimentRun::default()
                });
            }
        }
        let mut batch = json!({
            "id": acceptance_id,
            "name": name,
            "runs": runs,
            "type": "batch",
        });
        if let Some(description) = &params.description {
            batch["description"] = description.clone().into();
        }
        let path = api_path(&["v1", "applications", workspace_id, "experiments", "batch"])?;
        let created: BatchCreated = self.client.post(path).json(&batch).send_json().await?;
        if created.batch_id != acceptance_id {
            return Err(Error::Invalid {
                reason: format!(
                    "the platform created the acceptance test's batch experiment as `{}`, \
                     not `{acceptance_id}`",
                    created.batch_id
                ),
            });
        }
        let mut body = json!({
            "id": acceptance_id,
            "name": name,
            "experiment_id": created.batch_id,
            "control": { "instance_id": params.baseline_environment_id },
            "candidate": { "instance_id": params.candidate_environment_id },
            "metrics": params.metrics,
        });
        if let Some(description) = params.description {
            body["description"] = description.into();
        }
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "acceptance",
        ])?;
        self.client.post(path).json(&body).send_json().await
    }

    /// Gets an acceptance test, including its results and its runs.
    ///
    /// The runs come from a second request, to the batch experiment that ran
    /// them, which pages through all of them. With `params.polling`, waits
    /// until the acceptance test reaches a terminal state, and returns the
    /// state that ended polling, with its runs. Polling has no limit unless
    /// `max_duration` or `max_tries` is set, and a status this SDK does not
    /// know is not terminal, so if the platform adds a final status, polling
    /// waits until a limit or the caller's own timeout stops it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Wait`] if polling runs out of time or tries.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn get_acceptance(
        &self,
        workspace_id: &str,
        acceptance_id: &str,
        params: AcceptanceGetParams,
    ) -> Result<AcceptanceTest> {
        let mut test = self
            .get_acceptance_without_runs(workspace_id, acceptance_id, params)
            .await?;
        let batch_id = test
            .experiment_id
            .clone()
            .filter(|batch_id| !batch_id.is_empty())
            .unwrap_or_else(|| test.acceptance_id.clone());
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "batch",
            &batch_id,
            "runs",
        ])?;
        let runs = self
            .client
            .get(path)
            .paginate()
            .items_key("runs")
            .collect()
            .await?;
        test.runs = Some(runs);
        Ok(test)
    }

    /// [`Platform::get_acceptance`] without the test's runs, so without the
    /// requests that read them.
    pub(crate) async fn get_acceptance_without_runs(
        &self,
        workspace_id: &str,
        acceptance_id: &str,
        params: AcceptanceGetParams,
    ) -> Result<AcceptanceTest> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "acceptance",
            acceptance_id,
        ])?;
        match params.polling {
            None => self.client.get(path).send_json().await,
            // The last poll is already terminal, so its test is returned
            // without another get.
            Some(options) => {
                poll(&options, || async {
                    let test: AcceptanceTest = self.client.get(&path).send_json().await?;
                    let done = test.status.is_some_and(|status| status.is_terminal());
                    Ok(done.then_some(test))
                })
                .await
            }
        }
    }

    /// Lists the acceptance tests of a workspace, without their runs.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn list_acceptances(&self, workspace_id: &str) -> Result<Vec<AcceptanceTest>> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "acceptance",
        ])?;
        self.client.get(path).paginate().collect().await
    }

    /// Updates an acceptance test.
    ///
    /// Only the fields that `params` sets change. The platform returns the
    /// test's ID, name, description and timestamps.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `params` sets neither field, or sets
    /// both to empty strings.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn update_acceptance(
        &self,
        workspace_id: &str,
        acceptance_id: &str,
        params: AcceptanceUpdateParams,
    ) -> Result<AcceptanceTest> {
        let unset = |field: &Option<String>| field.as_deref().is_none_or(str::is_empty);
        // An empty string alone is no change.
        if unset(&params.name) && unset(&params.description) {
            return Err(Error::Invalid {
                reason: "nothing to update: set `name` or `description`".to_owned(),
            });
        }
        let mut body = json!({});
        if let Some(name) = params.name {
            body["name"] = name.into();
        }
        if let Some(description) = params.description {
            body["description"] = description.into();
        }
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "acceptance",
            acceptance_id,
        ])?;
        self.client.patch(path).json(&body).send_json().await
    }

    /// Deletes an acceptance test, and the batch experiment that ran its
    /// runs, with them.
    ///
    /// The test goes first, since the platform refuses to delete a batch
    /// experiment that a test points at. A batch experiment that another
    /// acceptance test also uses stays. The platform deletes the batch
    /// experiment in the background: with `params.polling`, this waits until
    /// a GET of it is a 404.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Status`] with status 404 if the acceptance test does
    /// not exist. If the test is deleted but the request that deletes its
    /// batch experiment fails, returns that request's error, whose URL names
    /// the batch experiment: a second call can't retry it, since the test is
    /// gone. Returns [`Error::Wait`] if polling runs out of time or tries, or
    /// if the platform reports that the batch experiment's delete failed.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn delete_acceptance(
        &self,
        workspace_id: &str,
        acceptance_id: &str,
        params: AcceptanceDeleteParams,
    ) -> Result<()> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "acceptance",
            acceptance_id,
        ])?;
        // A list, not a get: the get is a 404 when the batch experiment is
        // gone, and the list has every test's batch experiment.
        let tests = self.list_acceptances(workspace_id).await?;
        let batch_of = |test: &AcceptanceTest| {
            Some(
                test.experiment_id
                    .clone()
                    .unwrap_or_else(|| test.acceptance_id.clone()),
            )
            .filter(|batch_id| !batch_id.is_empty())
        };
        let batch_id = tests
            .iter()
            .find(|test| test.acceptance_id == acceptance_id)
            .and_then(batch_of);
        let shared = batch_id.as_ref().is_some_and(|batch_id| {
            tests.iter().any(|test| {
                test.acceptance_id != acceptance_id && batch_of(test).as_ref() == Some(batch_id)
            })
        });
        // A test that the list doesn't have gets the platform's 404 here.
        self.client.delete(path).send().await?;
        let Some(batch_id) = batch_id.filter(|_| !shared) else {
            return Ok(());
        };
        let batch = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "batch",
            &batch_id,
        ])?;
        // An error here comes after the test is gone, and its URL names the
        // batch experiment that stays.
        match self.client.delete(&batch).send().await {
            Ok(_) | Err(Error::Status { status: 404, .. }) => {}
            Err(error) => return Err(error),
        }
        if let Some(options) = params.polling {
            poll_change(&options, || async {
                let result = self.client.get(&batch).send_json::<BatchStatus>().await;
                gone(result, |batch| deleting(batch.status, "batch experiment"))
            })
            .await?;
        }
        Ok(())
    }
}

/// Checks the metrics of a new acceptance test: each names a field, and
/// none has a value this SDK reads as `Unknown`, which the platform would
/// reject only after the batch experiment exists.
fn check_metrics(metrics: &[Metric]) -> Result<()> {
    for (i, metric) in metrics.iter().enumerate() {
        let unknown = if metric.field.is_empty() {
            Some("field")
        } else if metric.metric_type == MetricType::Unknown {
            Some("metric_type")
        } else if metric.statistic == StatisticType::Unknown {
            Some("statistic")
        } else if metric.params.operator == Comparison::Unknown {
            Some("params.operator")
        } else if metric.params.tolerance.tolerance_type == MetricToleranceType::Unknown {
            Some("params.tolerance.type")
        } else {
            None
        };
        if let Some(name) = unknown {
            return Err(Error::Invalid {
                reason: format!("`metrics[{i}].{name}` must be set to a known value"),
            });
        }
    }
    Ok(())
}

pub(crate) fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "decision_platform_acceptance_create",
            "Creates an acceptance test that compares a candidate environment with a baseline.",
            |cx, a: InWorkspace<AcceptanceCreateParams>| async move {
                cx.platform()?
                    .create_acceptance(&a.workspace_id, a.params)
                    .await
            },
        ),
        Tool::new(
            "decision_platform_acceptance_get",
            "Gets an acceptance test, including its results.",
            |cx, a: AcceptanceRef<AcceptanceGetParams>| async move {
                cx.platform()?
                    .get_acceptance(&a.workspace_id, &a.acceptance_id, a.params)
                    .await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_acceptance_list",
            "Lists the acceptance tests of a workspace.",
            |cx, a: InWorkspace<NoParams>| async move {
                cx.platform()?.list_acceptances(&a.workspace_id).await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_acceptance_update",
            "Updates an acceptance test.",
            |cx, a: AcceptanceRef<AcceptanceUpdateParams>| async move {
                cx.platform()?
                    .update_acceptance(&a.workspace_id, &a.acceptance_id, a.params)
                    .await
            },
        )
        .destructive(),
        Tool::new(
            "decision_platform_acceptance_delete",
            "Deletes an acceptance test.",
            |cx, a: AcceptanceRef<AcceptanceDeleteParams>| async move {
                cx.platform()?
                    .delete_acceptance(&a.workspace_id, &a.acceptance_id, a.params)
                    .await
            },
        )
        .destructive(),
    ]
}
