//! Input sets, on `v1/applications/{workspace_id}/experiments/inputsets`.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{LATEST_ENVIRONMENT_ID, Platform};
use crate::decision::id::safe_id;
use crate::decision::path::api_path;
use crate::decision::polling::{Change, gone, poll_change};
use crate::decision::time::optional_timestamp;
use crate::decision::{InputSet, ManagedInput, PollingOptions};
use crate::tool::{InWorkspace, NoParams, Tool};
use crate::{Error, Result};

/// Tool arguments that address an input set, plus the operation's parameters.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct InputSetRef<P> {
    /// ID of the workspace.
    pub(crate) workspace_id: String,
    /// ID of the input set.
    pub(crate) input_set_id: String,
    #[serde(flatten)]
    pub(crate) params: P,
}

/// Parameters for [`Platform::create_input_set`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct InputSetCreateParams {
    /// ID of the input set. Generated when not set.
    pub input_set_id: Option<String>,
    /// Name of the input set. Defaults to its ID.
    pub name: Option<String>,
    /// Description of the input set.
    pub description: Option<String>,
    /// ID of the environment whose runs between `start_time` and `end_time`
    /// make up the input set. `default` names the workspace's default
    /// environment, or `latest` when the workspace has none. Not with
    /// `run_ids` or `inputs`.
    pub environment_id: Option<String>,
    /// Start of the time range of the environment's runs, in RFC 3339.
    /// Defaults to a day before `end_time`.
    pub start_time: Option<String>,
    /// End of the time range of the environment's runs, in RFC 3339.
    /// Defaults to now.
    pub end_time: Option<String>,
    /// Most runs or inputs the input set can hold. Unset or 0 means the
    /// platform's maximum.
    pub maximum_runs: Option<u32>,
    /// IDs of the runs whose inputs make up the input set. Not with
    /// `inputs`.
    pub run_ids: Option<Vec<String>>,
    /// Managed inputs that make up the input set. Each needs its ID.
    pub inputs: Option<Vec<ManagedInput>>,
}

/// Parameters for [`Platform::delete_input_set`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct InputSetDeleteParams {
    /// Polls until the input set is gone. Without it, returns once the
    /// platform accepts the delete, which it finishes later. With the
    /// defaults, polling has no time or try limit.
    pub polling: Option<PollingOptions>,
}

/// Parameters for [`Platform::update_input_set`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct InputSetUpdateParams {
    /// New name of the input set.
    pub name: Option<String>,
    /// New description of the input set.
    pub description: Option<String>,
    /// Managed inputs that replace the input set's current ones. An empty
    /// list removes them all.
    pub inputs: Option<Vec<ManagedInput>>,
}

impl Platform {
    /// Creates an input set: a named group of inputs for experiments.
    ///
    /// The inputs come from one source: the runs of `environment_id`
    /// between `start_time` and `end_time`, the runs in `run_ids`, or the
    /// managed inputs in `inputs`. An unset or empty `input_set_id` is
    /// generated, an unset or empty `name` defaults to the ID, and an empty
    /// `start_time` or `end_time` counts as unset.
    ///
    /// An input set holds at most 20 `run_ids`. Managed inputs and the runs
    /// of a time range are limited by `maximum_runs`, and by the platform's
    /// maximum, which is 1,000 in the cloud's deployments.
    ///
    /// For the environment
    /// [`DEFAULT_ENVIRONMENT_ID`](super::DEFAULT_ENVIRONMENT_ID), the
    /// workspace is read first, for its default environment.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`], before any request, if `start_time` or
    /// `end_time` is not an RFC 3339 timestamp, if `params` sets no source
    /// or more than one (a time range or `environment_id`, `run_ids`, and
    /// `inputs`), if `run_ids` has more than 20 items, or if `inputs` or
    /// `maximum_runs` is over 1,000.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn create_input_set(
        &self,
        workspace_id: &str,
        params: InputSetCreateParams,
    ) -> Result<InputSet> {
        let start_time = optional_timestamp("start_time", params.start_time)?;
        let end_time = optional_timestamp("end_time", params.end_time)?;
        let time_range = start_time.is_some()
            || end_time.is_some()
            || params
                .environment_id
                .as_deref()
                .is_some_and(|id| !id.is_empty());
        check_sources(
            time_range,
            params.run_ids.as_deref(),
            params.inputs.as_deref(),
            params.maximum_runs,
        )?;
        let input_set_id = params
            .input_set_id
            .filter(|input_set_id| !input_set_id.is_empty())
            .unwrap_or_else(|| safe_id("input-set"));
        let name = params
            .name
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| input_set_id.clone());
        let mut body = json!({ "id": input_set_id, "name": name });
        if let Some(description) = params.description {
            body["description"] = description.into();
        }
        if let Some(environment_id) = params.environment_id {
            // An input set needs an environment that exists, so the
            // platform's fallback, which isn't one, is `latest` here.
            let environment_id = self
                .resolve_environment(workspace_id, &environment_id)
                .await?
                .unwrap_or_else(|| LATEST_ENVIRONMENT_ID.to_owned());
            body["instance_id"] = environment_id.into();
        }
        if let Some(start_time) = start_time {
            body["start_time"] = start_time.into();
        }
        if let Some(end_time) = end_time {
            body["end_time"] = end_time.into();
        }
        if let Some(maximum_runs) = params.maximum_runs {
            body["maximum_runs"] = maximum_runs.into();
        }
        if let Some(run_ids) = params.run_ids {
            body["run_ids"] = run_ids.into();
        }
        if let Some(inputs) = params.inputs {
            body["inputs"] = json!(inputs);
        }
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "inputsets",
        ])?;
        self.client.post(path).json(&body).send_json().await
    }

    /// Gets an input set.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Status`] with status 404 if the input set does not
    /// exist, and the [errors of every method](Platform#errors).
    pub async fn get_input_set(&self, workspace_id: &str, input_set_id: &str) -> Result<InputSet> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "inputsets",
            input_set_id,
        ])?;
        self.client.get(path).send_json().await
    }

    /// Lists the input sets of a workspace.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn list_input_sets(&self, workspace_id: &str) -> Result<Vec<InputSet>> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "inputsets",
        ])?;
        self.client.get(path).paginate().collect().await
    }

    /// Updates an input set.
    ///
    /// Fields that `params` leaves unset keep their values. `inputs`
    /// replaces the managed inputs, so an empty list removes them.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `params` sets no field.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn update_input_set(
        &self,
        workspace_id: &str,
        input_set_id: &str,
        params: InputSetUpdateParams,
    ) -> Result<InputSet> {
        if params.name.is_none() && params.description.is_none() && params.inputs.is_none() {
            return Err(Error::Invalid {
                reason: "nothing to update: set `name`, `description` or `inputs`".to_owned(),
            });
        }
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "inputsets",
            input_set_id,
        ])?;
        // The whole input set goes back, so fields this SDK does not model
        // keep their values too.
        let mut input_set: Map<String, Value> = self.client.get(&path).send_json().await?;
        if let Some(name) = params.name {
            input_set.insert("name".to_owned(), name.into());
        }
        if let Some(description) = params.description {
            input_set.insert("description".to_owned(), description.into());
        }
        if let Some(inputs) = params.inputs {
            input_set.insert("inputs".to_owned(), json!(inputs));
        }
        self.client.put(path).json(&input_set).send_json().await
    }

    /// Deletes an input set.
    ///
    /// The platform deletes it in the background, and not at all while an
    /// experiment uses it, without saying so: with `params.polling`, this
    /// waits until a GET of it is a 404, so set a limit.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Wait`] if polling runs out of time or tries.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn delete_input_set(
        &self,
        workspace_id: &str,
        input_set_id: &str,
        params: InputSetDeleteParams,
    ) -> Result<()> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "inputsets",
            input_set_id,
        ])?;
        self.client.delete(path).send().await?;
        if let Some(options) = params.polling {
            poll_change(&options, || async {
                let result = self.get_input_set(workspace_id, input_set_id).await;
                gone(result, |_| Change::Pending)
            })
            .await?;
        }
        Ok(())
    }
}

/// Most `run_ids` an input set takes, which every deployment shares.
const MAX_RUN_IDS: usize = 20;
/// Most inputs an input set can hold: the largest `MaxInputSetSize` of the
/// platform's deployments. Those that allow fewer reject a smaller input set
/// only when it is created.
const MAX_INPUTS: usize = 1_000;

/// Checks the sources of a new input set: exactly one of a time range of an
/// environment's runs, `run_ids` and `inputs`, at most [`MAX_RUN_IDS`]
/// `run_ids`, and at most [`MAX_INPUTS`] `inputs` or `maximum_runs`.
fn check_sources(
    time_range: bool,
    run_ids: Option<&[String]>,
    inputs: Option<&[ManagedInput]>,
    maximum_runs: Option<u32>,
) -> Result<()> {
    let invalid = |reason: String| Err(Error::Invalid { reason });
    let run_ids = run_ids.unwrap_or_default();
    let inputs = inputs.unwrap_or_default();
    let sources = [time_range, !run_ids.is_empty(), !inputs.is_empty()];
    match sources.iter().filter(|&&set| set).count() {
        0 => {
            return invalid(
                "set one source: `environment_id` (with a time range), `run_ids` or `inputs`"
                    .to_owned(),
            );
        }
        1 => {}
        _ => {
            return invalid(
                "set only one source: `environment_id` (with a time range), `run_ids` or \
                 `inputs`"
                    .to_owned(),
            );
        }
    }
    if run_ids.len() > MAX_RUN_IDS {
        return invalid(format!(
            "`run_ids` has {} items: an input set takes at most {MAX_RUN_IDS}",
            run_ids.len()
        ));
    }
    if inputs.len() > MAX_INPUTS {
        return invalid(format!(
            "`inputs` has {} items: an input set holds at most {MAX_INPUTS}",
            inputs.len()
        ));
    }
    if let Some(maximum_runs) = maximum_runs.filter(|&max| max as usize > MAX_INPUTS) {
        return invalid(format!(
            "`maximum_runs` is {maximum_runs}: an input set holds at most {MAX_INPUTS}"
        ));
    }
    Ok(())
}

pub(crate) fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "decision_platform_input_set_create",
            "Creates an input set: a named group of inputs for experiments.",
            |cx, a: InWorkspace<InputSetCreateParams>| async move {
                cx.platform()?
                    .create_input_set(&a.workspace_id, a.params)
                    .await
            },
        ),
        Tool::new(
            "decision_platform_input_set_get",
            "Gets an input set.",
            |cx, a: InputSetRef<NoParams>| async move {
                cx.platform()?
                    .get_input_set(&a.workspace_id, &a.input_set_id)
                    .await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_input_set_list",
            "Lists the input sets of a workspace.",
            |cx, a: InWorkspace<NoParams>| async move {
                cx.platform()?.list_input_sets(&a.workspace_id).await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_input_set_update",
            "Updates an input set.",
            |cx, a: InputSetRef<InputSetUpdateParams>| async move {
                cx.platform()?
                    .update_input_set(&a.workspace_id, &a.input_set_id, a.params)
                    .await
            },
        )
        .destructive(),
        Tool::new(
            "decision_platform_input_set_delete",
            "Deletes an input set.",
            |cx, a: InputSetRef<InputSetDeleteParams>| async move {
                cx.platform()?
                    .delete_input_set(&a.workspace_id, &a.input_set_id, a.params)
                    .await
            },
        )
        .destructive(),
    ]
}
