//! Shadow tests, on `v1/applications/{workspace_id}/experiments/shadow`.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::Platform;
use crate::decision::id::safe_id;
use crate::decision::path::api_path;
use crate::decision::polling::{Change, deleting, gone, poll_change};
use crate::decision::time::optional_timestamp;
use crate::decision::{
    PollingOptions, Run, ShadowTest, ShadowTestMetadata, StartEvents, StopIntent,
    TerminationEvents, TestComparison,
};
use crate::tool::{InWorkspace, NoParams, Tool};
use crate::{Error, Result};

/// Tool arguments that address a shadow test, plus the operation's parameters.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ShadowRef<P> {
    /// ID of the workspace.
    pub(crate) workspace_id: String,
    /// ID of the shadow test.
    pub(crate) shadow_id: String,
    #[serde(flatten)]
    pub(crate) params: P,
}

/// Parameters for [`Platform::create_shadow`].
///
/// `comparisons` and `termination_events` are required, so this has no
/// `Default`.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct ShadowCreateParams {
    /// ID of the shadow test. Generated when not set.
    #[serde(default)]
    pub shadow_id: Option<String>,
    /// Name of the shadow test. Defaults to its ID.
    #[serde(default)]
    pub name: Option<String>,
    /// Description of the shadow test.
    #[serde(default)]
    pub description: Option<String>,
    /// The environments to compare: each baseline environment, with the
    /// candidate environments that run next to it on the same inputs. A
    /// baseline environment can be in only one comparison.
    pub comparisons: Vec<TestComparison>,
    /// When the shadow test ends: after a number of runs of its baseline
    /// environments, or at a time, whichever comes first.
    pub termination_events: TerminationEvents,
    /// When the shadow test begins comparing runs, once it is started.
    /// Without it, it begins as soon as it is started.
    #[serde(default)]
    pub start_events: Option<StartEvents>,
}

/// Parameters for [`Platform::stop_shadow`].
///
/// `intent` is required, so this has no `Default`.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct ShadowStopParams {
    /// Whether the stopped shadow test is marked as completed or canceled.
    pub intent: StopIntent,
    /// Polls until the shadow test has stopped. Without it, returns once the
    /// platform accepts the stop, which it finishes later. With the
    /// defaults, polling has no time or try limit.
    #[serde(default)]
    pub polling: Option<PollingOptions>,
}

/// Parameters for [`Platform::delete_shadow`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ShadowDeleteParams {
    /// Polls until the shadow test is gone. Without it, returns once the
    /// platform accepts the delete, which it finishes later. With the
    /// defaults, polling has no time or try limit.
    pub polling: Option<PollingOptions>,
}

/// Parameters for [`Platform::update_shadow`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ShadowUpdateParams {
    /// New name of the shadow test.
    pub name: Option<String>,
    /// New description of the shadow test.
    pub description: Option<String>,
}

impl Platform {
    /// Creates a shadow test, as a draft.
    ///
    /// Start it with [`Platform::start_shadow`]. An unset or empty
    /// `shadow_id` is generated, an unset or empty `name` defaults to the
    /// ID, and an empty time counts as unset.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`], before any request, if
    /// `termination_events.maximum_runs` is not between 1 and 300, if a time
    /// is not an RFC 3339 timestamp, or if a baseline environment is in more
    /// than one comparison.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn create_shadow(
        &self,
        workspace_id: &str,
        params: ShadowCreateParams,
    ) -> Result<ShadowTest> {
        let mut termination = params.termination_events;
        if !(1..=300).contains(&termination.maximum_runs) {
            return Err(Error::Invalid {
                reason: "`termination_events.maximum_runs` must be between 1 and 300".to_owned(),
            });
        }
        termination.time = optional_timestamp("termination_events.time", termination.time)?;
        let mut start_events = params.start_events;
        if let Some(start_events) = &mut start_events {
            start_events.time = optional_timestamp("start_events.time", start_events.time.take())?;
        }
        let comparisons = comparison_map(params.comparisons)?;
        let id = params
            .shadow_id
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| safe_id("shadow"));
        let name = params
            .name
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| id.clone());
        let mut body = json!({
            "id": id,
            "name": name,
            "comparisons": comparisons,
            "termination_events": termination,
        });
        if let Some(description) = params.description {
            body["description"] = description.into();
        }
        if let Some(start_events) = start_events {
            body["start_events"] = json!(start_events);
        }
        let path = api_path(&["v1", "applications", workspace_id, "experiments", "shadow"])?;
        self.client.post(path).json(&body).send_json().await
    }

    /// Gets a shadow test, including its runs.
    ///
    /// The runs come from a second request, which pages through all of
    /// them.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Status`] with status 404 if the shadow test does not
    /// exist, and the [errors of every method](Platform#errors).
    pub async fn get_shadow(&self, workspace_id: &str, shadow_id: &str) -> Result<ShadowTest> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "shadow",
            shadow_id,
        ])?;
        let mut shadow: ShadowTest = self.client.get(path).send_json().await?;
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "shadow",
            shadow_id,
            "runs",
        ])?;
        let runs: Vec<Run> = self
            .client
            .get(path)
            .paginate()
            .items_key("runs")
            .collect()
            .await?;
        shadow.runs = Some(runs);
        Ok(shadow)
    }

    /// Lists the shadow tests of a workspace.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn list_shadows(&self, workspace_id: &str) -> Result<Vec<ShadowTest>> {
        let path = api_path(&["v1", "applications", workspace_id, "experiments", "shadow"])?;
        self.client.get(path).paginate().collect().await
    }

    /// Gets the metadata of a shadow test, without its runs.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Status`] with status 404 if the shadow test does not
    /// exist, and the [errors of every method](Platform#errors).
    pub async fn get_shadow_metadata(
        &self,
        workspace_id: &str,
        shadow_id: &str,
    ) -> Result<ShadowTestMetadata> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "shadow",
            shadow_id,
            "metadata",
        ])?;
        self.client.get(path).send_json().await
    }

    /// Starts a shadow test.
    ///
    /// The shadow test must be a draft, see [`Platform::create_shadow`].
    /// Starting one that already started does nothing.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn start_shadow(&self, workspace_id: &str, shadow_id: &str) -> Result<()> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "shadow",
            shadow_id,
            "start",
        ])?;
        self.client.put(path).send().await?;
        Ok(())
    }

    /// Stops a shadow test.
    ///
    /// The shadow test must have started. `params.intent` says whether it is
    /// marked as completed or canceled. The platform stops it in the
    /// background: with `params.polling`, this waits until its status is
    /// terminal. Its metadata then has the status it ended with.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Wait`] if polling runs out of time or tries.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn stop_shadow(
        &self,
        workspace_id: &str,
        shadow_id: &str,
        params: ShadowStopParams,
    ) -> Result<()> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "shadow",
            shadow_id,
            "stop",
        ])?;
        let body = json!({ "intent": params.intent });
        self.client.put(path).json(&body).send().await?;
        if let Some(options) = params.polling {
            poll_change(&options, || async {
                let metadata = self.get_shadow_metadata(workspace_id, shadow_id).await?;
                Ok(match metadata.status.is_some_and(|s| s.is_terminal()) {
                    true => Change::Done,
                    false => Change::Pending,
                })
            })
            .await?;
        }
        Ok(())
    }

    /// Updates a shadow test.
    ///
    /// Only the fields that `params` sets change. The platform answers with
    /// the shadow test's ID, name, description and timestamps only, so the
    /// result's other fields are `None`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`], before any request, if `params` sets no
    /// field.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn update_shadow(
        &self,
        workspace_id: &str,
        shadow_id: &str,
        params: ShadowUpdateParams,
    ) -> Result<ShadowTest> {
        if params.name.is_none() && params.description.is_none() {
            return Err(Error::Invalid {
                reason: "an update needs `name` or `description`".to_owned(),
            });
        }
        let mut body = Map::new();
        if let Some(name) = params.name {
            body.insert("name".to_owned(), name.into());
        }
        if let Some(description) = params.description {
            body.insert("description".to_owned(), description.into());
        }
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "shadow",
            shadow_id,
        ])?;
        self.client.patch(path).json(&body).send_json().await
    }

    /// Deletes a shadow test.
    ///
    /// The platform deletes it in the background: with `params.polling`,
    /// this waits until a GET of it is a 404.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Wait`] if polling runs out of time or tries, or if
    /// the platform reports that the delete failed.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn delete_shadow(
        &self,
        workspace_id: &str,
        shadow_id: &str,
        params: ShadowDeleteParams,
    ) -> Result<()> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "experiments",
            "shadow",
            shadow_id,
        ])?;
        self.client.delete(path).send().await?;
        if let Some(options) = params.polling {
            poll_change(&options, || async {
                let result = self.get_shadow_metadata(workspace_id, shadow_id).await;
                gone(result, |metadata| deleting(metadata.status, "shadow test"))
            })
            .await?;
        }
        Ok(())
    }
}

/// `comparisons` as the API takes them: the ID of each baseline environment,
/// mapped to the IDs of its candidate environments.
fn comparison_map(comparisons: Vec<TestComparison>) -> Result<Map<String, Value>> {
    let mut map = Map::new();
    for comparison in comparisons {
        let baseline = comparison.baseline_environment_id;
        if map.contains_key(&baseline) {
            return Err(Error::Invalid {
                reason: format!("baseline environment `{baseline}` is in more than one comparison"),
            });
        }
        map.insert(baseline, comparison.candidate_environment_ids.into());
    }
    Ok(map)
}

pub(crate) fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "decision_platform_shadow_create",
            "Creates a shadow test, as a draft.",
            |cx, a: InWorkspace<ShadowCreateParams>| async move {
                cx.platform()?
                    .create_shadow(&a.workspace_id, a.params)
                    .await
            },
        ),
        Tool::new(
            "decision_platform_shadow_get",
            "Gets a shadow test, including its runs.",
            |cx, a: ShadowRef<NoParams>| async move {
                cx.platform()?
                    .get_shadow(&a.workspace_id, &a.shadow_id)
                    .await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_shadow_list",
            "Lists the shadow tests of a workspace.",
            |cx, a: InWorkspace<NoParams>| async move {
                cx.platform()?.list_shadows(&a.workspace_id).await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_shadow_metadata_get",
            "Gets the metadata of a shadow test, without its runs.",
            |cx, a: ShadowRef<NoParams>| async move {
                cx.platform()?
                    .get_shadow_metadata(&a.workspace_id, &a.shadow_id)
                    .await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_shadow_start",
            "Starts a shadow test.",
            |cx, a: ShadowRef<NoParams>| async move {
                cx.platform()?
                    .start_shadow(&a.workspace_id, &a.shadow_id)
                    .await
            },
        ),
        Tool::new(
            "decision_platform_shadow_stop",
            "Stops a shadow test.",
            |cx, a: ShadowRef<ShadowStopParams>| async move {
                cx.platform()?
                    .stop_shadow(&a.workspace_id, &a.shadow_id, a.params)
                    .await
            },
        )
        .destructive(),
        Tool::new(
            "decision_platform_shadow_update",
            "Updates a shadow test.",
            |cx, a: ShadowRef<ShadowUpdateParams>| async move {
                cx.platform()?
                    .update_shadow(&a.workspace_id, &a.shadow_id, a.params)
                    .await
            },
        )
        .destructive(),
        Tool::new(
            "decision_platform_shadow_delete",
            "Deletes a shadow test.",
            |cx, a: ShadowRef<ShadowDeleteParams>| async move {
                cx.platform()?
                    .delete_shadow(&a.workspace_id, &a.shadow_id, a.params)
                    .await
            },
        )
        .destructive(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_comparisons_by_baseline() {
        let comparisons = vec![
            TestComparison {
                baseline_environment_id: "production".to_owned(),
                candidate_environment_ids: vec!["staging".to_owned(), "canary".to_owned()],
            },
            TestComparison {
                baseline_environment_id: "legacy".to_owned(),
                candidate_environment_ids: vec!["next".to_owned()],
            },
        ];
        assert_eq!(
            Value::Object(comparison_map(comparisons).unwrap()),
            json!({ "production": ["staging", "canary"], "legacy": ["next"] })
        );
        let twice = vec![
            TestComparison {
                baseline_environment_id: "production".to_owned(),
                candidate_environment_ids: vec!["staging".to_owned()],
            },
            TestComparison {
                baseline_environment_id: "production".to_owned(),
                candidate_environment_ids: vec!["canary".to_owned()],
            },
        ];
        let err = comparison_map(twice).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid argument: baseline environment `production` is in more than one comparison"
        );
    }
}
