//! Switchback tests, on
//! `v1/applications/{workspace_id}/experiments/switchback`.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use super::Platform;
use crate::decision::id::safe_id;
use crate::decision::path::api_path;
use crate::decision::polling::{Change, deleting, gone, poll_change};
use crate::decision::time::optional_timestamp;
use crate::decision::{
    PollingOptions, Run, StopIntent, SwitchbackTest, SwitchbackTestMetadata, TestComparisonSingle,
};
use crate::tool::{InWorkspace, NoParams, Tool};
use crate::{Error, Result};

/// Tool arguments that address a switchback test, plus the operation's
/// parameters.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct SwitchbackRef<P> {
    /// ID of the workspace.
    pub(crate) workspace_id: String,
    /// ID of the switchback test.
    pub(crate) switchback_id: String,
    #[serde(flatten)]
    pub(crate) params: P,
}

/// Parameters for [`Platform::create_switchback`].
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct SwitchbackCreateParams {
    /// ID of the switchback test. Generated when not set.
    pub switchback_id: Option<String>,
    /// Name of the switchback test. Defaults to its ID.
    pub name: Option<String>,
    /// Description of the switchback test.
    pub description: Option<String>,
    /// The baseline environment and the candidate environment to alternate
    /// between.
    pub comparison: TestComparisonSingle,
    /// How long each unit of the plan lasts, in minutes, from 1 to 10080
    /// (a week).
    #[schemars(range(min = 1, max = 10080))]
    pub unit_duration_minutes: f64,
    /// Number of units in the plan, from 1 to 1000. Each unit runs the
    /// baseline or the candidate environment, picked at random.
    #[schemars(range(min = 1, max = 1000))]
    pub units: u32,
    /// When the plan's first unit begins, as an RFC 3339 timestamp in the
    /// future, e.g. `2026-10-13T15:00:00Z`. It does not start the test:
    /// start the test before then. Without one, the plan begins when the
    /// test starts.
    pub start: Option<String>,
}

/// Parameters for [`Platform::stop_switchback`].
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct SwitchbackStopParams {
    /// Whether to mark the switchback test as completed or as canceled.
    pub intent: StopIntent,
    /// Polls until the switchback test has stopped. Without it, returns once
    /// the platform accepts the stop, which it finishes later. With the
    /// defaults, polling has no time or try limit.
    #[serde(default)]
    pub polling: Option<PollingOptions>,
}

/// Parameters for [`Platform::delete_switchback`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SwitchbackDeleteParams {
    /// Polls until the switchback test is gone. Without it, returns once the
    /// platform accepts the delete, which it finishes later. With the
    /// defaults, polling has no time or try limit.
    pub polling: Option<PollingOptions>,
}

/// Parameters for [`Platform::update_switchback`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SwitchbackUpdateParams {
    /// New name of the switchback test.
    pub name: Option<String>,
    /// New description of the switchback test.
    pub description: Option<String>,
}

impl Platform {
    /// Creates a switchback test, as a draft.
    ///
    /// Start it with [`Platform::start_switchback`]; `start` only delays when
    /// its plan begins. An unset or empty `switchback_id` is generated, an
    /// unset or empty `name` defaults to the ID, and an empty `start` counts
    /// as unset.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `unit_duration_minutes` is not between 1
    /// and 10080, `units` is not between 1 and 1000, or `start` is not an
    /// RFC 3339 timestamp.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn create_switchback(
        &self,
        workspace_id: &str,
        params: SwitchbackCreateParams,
    ) -> Result<SwitchbackTest> {
        // `contains` also rejects NaN.
        if !(1.0..=10_080.0).contains(&params.unit_duration_minutes) {
            return Err(Error::Invalid {
                reason: "`unit_duration_minutes` must be between 1 and 10080".to_owned(),
            });
        }
        if !(1..=1000).contains(&params.units) {
            return Err(Error::Invalid {
                reason: "`units` must be between 1 and 1000".to_owned(),
            });
        }
        let start = optional_timestamp("start", params.start)?;
        let id = match params.switchback_id.filter(|id| !id.is_empty()) {
            Some(id) => id,
            None => safe_id("switchback"),
        };
        let name = match params.name.filter(|name| !name.is_empty()) {
            Some(name) => name,
            None => id.clone(),
        };
        let mut plan = json!({
            "unit_duration_minutes": params.unit_duration_minutes,
            "units": params.units,
        });
        if let Some(start) = start {
            plan["start"] = start.into();
        }
        let mut body = json!({
            "id": id,
            "name": name,
            "comparison": params.comparison,
            "generate_random_plan": plan,
        });
        if let Some(description) = params.description {
            body["description"] = description.into();
        }
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "switchback",
        ])?;
        self.client.post(path).json(&body).send_json().await
    }

    /// Gets a switchback test, including its runs.
    ///
    /// Pages through all of its runs.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Status`] with status 404 if the switchback test does
    /// not exist, and the [errors of every method](Platform#errors).
    pub async fn get_switchback(
        &self,
        workspace_id: &str,
        switchback_id: &str,
    ) -> Result<SwitchbackTest> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "switchback",
            switchback_id,
        ])?;
        let mut test: SwitchbackTest = self.client.get(path).send_json().await?;
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "switchback",
            switchback_id,
            "runs",
        ])?;
        // A page holds at most 200 runs, so every page is read.
        let runs: Vec<Run> = self
            .client
            .get(path)
            .paginate()
            .items_key("runs")
            .collect()
            .await?;
        test.runs = Some(runs);
        Ok(test)
    }

    /// Lists the switchback tests of a workspace.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn list_switchbacks(&self, workspace_id: &str) -> Result<Vec<SwitchbackTest>> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "switchback",
        ])?;
        self.client.get(path).paginate().collect().await
    }

    /// Gets the metadata of a switchback test, without its runs.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Status`] with status 404 if the switchback test does
    /// not exist, and the [errors of every method](Platform#errors).
    pub async fn get_switchback_metadata(
        &self,
        workspace_id: &str,
        switchback_id: &str,
    ) -> Result<SwitchbackTestMetadata> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "switchback",
            switchback_id,
            "metadata",
        ])?;
        self.client.get(path).send_json().await
    }

    /// Starts a switchback test.
    ///
    /// The test must be a draft. Starting a test that already started does
    /// nothing.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn start_switchback(&self, workspace_id: &str, switchback_id: &str) -> Result<()> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "switchback",
            switchback_id,
            "start",
        ])?;
        self.client.put(path).send().await?;
        Ok(())
    }

    /// Stops a switchback test.
    ///
    /// The test must have started. The platform stops it in the background:
    /// with `params.polling`, this waits until its status is terminal. Its
    /// metadata then has the status it ended with.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Wait`] if polling runs out of time or tries.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn stop_switchback(
        &self,
        workspace_id: &str,
        switchback_id: &str,
        params: SwitchbackStopParams,
    ) -> Result<()> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "switchback",
            switchback_id,
            "stop",
        ])?;
        let body = json!({ "intent": params.intent });
        self.client.put(path).json(&body).send().await?;
        if let Some(options) = params.polling {
            poll_change(&options, || async {
                let metadata = self
                    .get_switchback_metadata(workspace_id, switchback_id)
                    .await?;
                Ok(match metadata.status.is_some_and(|s| s.is_terminal()) {
                    true => Change::Done,
                    false => Change::Pending,
                })
            })
            .await?;
        }
        Ok(())
    }

    /// Updates a switchback test.
    ///
    /// Only the fields that `params` sets change. The platform returns the
    /// test's ID, name, description and timestamps.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `params` sets no field.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn update_switchback(
        &self,
        workspace_id: &str,
        switchback_id: &str,
        params: SwitchbackUpdateParams,
    ) -> Result<SwitchbackTest> {
        if params.name.is_none() && params.description.is_none() {
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
            "switchback",
            switchback_id,
        ])?;
        self.client.patch(path).json(&body).send_json().await
    }

    /// Deletes a switchback test.
    ///
    /// Its runs stay, since they are production runs: the platform only
    /// removes their link to the test. The platform deletes the test in the
    /// background, canceling it first if it is running, so it can still be
    /// read for a short while: with `params.polling`, this waits until a GET
    /// of it is a 404.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Wait`] if polling runs out of time or tries, or if
    /// the platform reports that the delete failed.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn delete_switchback(
        &self,
        workspace_id: &str,
        switchback_id: &str,
        params: SwitchbackDeleteParams,
    ) -> Result<()> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "switchback",
            switchback_id,
        ])?;
        self.client.delete(path).send().await?;
        if let Some(options) = params.polling {
            poll_change(&options, || async {
                let result = self
                    .get_switchback_metadata(workspace_id, switchback_id)
                    .await;
                gone(result, |metadata| {
                    deleting(metadata.status, "switchback test")
                })
            })
            .await?;
        }
        Ok(())
    }
}

pub(crate) fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "decision_platform_switchback_create",
            "Creates a switchback test, as a draft.",
            |cx, a: InWorkspace<SwitchbackCreateParams>| async move {
                cx.platform()?
                    .create_switchback(&a.workspace_id, a.params)
                    .await
            },
        ),
        Tool::new(
            "decision_platform_switchback_get",
            "Gets a switchback test, including its runs.",
            |cx, a: SwitchbackRef<NoParams>| async move {
                cx.platform()?
                    .get_switchback(&a.workspace_id, &a.switchback_id)
                    .await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_switchback_list",
            "Lists the switchback tests of a workspace.",
            |cx, a: InWorkspace<NoParams>| async move {
                cx.platform()?.list_switchbacks(&a.workspace_id).await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_switchback_metadata_get",
            "Gets the metadata of a switchback test, without its runs.",
            |cx, a: SwitchbackRef<NoParams>| async move {
                cx.platform()?
                    .get_switchback_metadata(&a.workspace_id, &a.switchback_id)
                    .await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_switchback_start",
            "Starts a switchback test.",
            |cx, a: SwitchbackRef<NoParams>| async move {
                cx.platform()?
                    .start_switchback(&a.workspace_id, &a.switchback_id)
                    .await
            },
        ),
        Tool::new(
            "decision_platform_switchback_stop",
            "Stops a switchback test.",
            |cx, a: SwitchbackRef<SwitchbackStopParams>| async move {
                cx.platform()?
                    .stop_switchback(&a.workspace_id, &a.switchback_id, a.params)
                    .await
            },
        )
        .destructive(),
        Tool::new(
            "decision_platform_switchback_update",
            "Updates a switchback test.",
            |cx, a: SwitchbackRef<SwitchbackUpdateParams>| async move {
                cx.platform()?
                    .update_switchback(&a.workspace_id, &a.switchback_id, a.params)
                    .await
            },
        )
        .destructive(),
        Tool::new(
            "decision_platform_switchback_delete",
            "Deletes a switchback test.",
            |cx, a: SwitchbackRef<SwitchbackDeleteParams>| async move {
                cx.platform()?
                    .delete_switchback(&a.workspace_id, &a.switchback_id, a.params)
                    .await
            },
        )
        .destructive(),
    ]
}
