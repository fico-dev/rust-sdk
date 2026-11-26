//! Managed inputs, on `v1/applications/{workspace_id}/inputs`.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::Platform;
use crate::decision::id::safe_id;
use crate::decision::path::api_path;
use crate::decision::{ContentFormat, Format, FormatInput, FormatOutput, ManagedInput};
use crate::tool::{InWorkspace, NoParams, Tool};
use crate::{Error, Result};

/// Tool arguments that address a managed input, plus the operation's
/// parameters.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ManagedInputRef<P> {
    /// ID of the workspace.
    pub(crate) workspace_id: String,
    /// ID of the managed input.
    pub(crate) managed_input_id: String,
    #[serde(flatten)]
    pub(crate) params: P,
}

/// Parameters for [`Platform::create_managed_input`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ManagedInputCreateParams {
    /// ID of the managed input. Generated when not set.
    pub managed_input_id: Option<String>,
    /// Name of the managed input. Defaults to its ID.
    pub name: Option<String>,
    /// Description of the managed input.
    pub description: Option<String>,
    /// ID of the upload whose data becomes the managed input. Not with
    /// `run_id`.
    pub upload_id: Option<String>,
    /// ID of the run whose input becomes the managed input. Not with
    /// `upload_id`.
    pub run_id: Option<String>,
    /// Content format of the managed input. Defaults to JSON. With `run_id`,
    /// it must match the run's input.
    pub content_format: Option<ContentFormat>,
}

/// Parameters for [`Platform::update_managed_input`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ManagedInputUpdateParams {
    /// New name of the managed input.
    pub name: Option<String>,
    /// New description of the managed input.
    pub description: Option<String>,
}

impl Platform {
    /// Creates a managed input from an upload or from a run's input.
    ///
    /// Exactly one of `upload_id` and `run_id` must be set; an empty one
    /// counts as unset. An unset or empty `managed_input_id` is generated,
    /// an unset or empty `name` defaults to the ID, and `content_format`,
    /// JSON by default, is the format of both the input and the output.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] unless exactly one of `upload_id` and
    /// `run_id` is set, or if `content_format` is `Unknown`.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn create_managed_input(
        &self,
        workspace_id: &str,
        params: ManagedInputCreateParams,
    ) -> Result<ManagedInput> {
        let upload_id = params.upload_id.filter(|upload_id| !upload_id.is_empty());
        let run_id = params.run_id.filter(|run_id| !run_id.is_empty());
        if upload_id.is_some() == run_id.is_some() {
            return Err(Error::Invalid {
                reason: "a managed input needs exactly one of `upload_id` and `run_id`".to_owned(),
            });
        }
        let managed_input_id = params
            .managed_input_id
            .filter(|managed_input_id| !managed_input_id.is_empty())
            .unwrap_or_else(|| safe_id("managed-input"));
        let name = params
            .name
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| managed_input_id.clone());
        let content_format = params.content_format.unwrap_or_default();
        if content_format == ContentFormat::Unknown {
            return Err(Error::Invalid {
                reason: "`content_format` must be `json` or `multi-file`".to_owned(),
            });
        }
        let format = Format {
            format_input: FormatInput {
                input_type: content_format,
            },
            format_output: Some(FormatOutput {
                output_type: content_format,
            }),
        };
        let mut body = json!({ "id": managed_input_id, "name": name, "format": format });
        if let Some(description) = params.description {
            body["description"] = description.into();
        }
        if let Some(upload_id) = upload_id {
            body["upload_id"] = upload_id.into();
        }
        if let Some(run_id) = run_id {
            body["run_id"] = run_id.into();
        }
        let path = api_path(&["v1", "applications", workspace_id, "inputs"])?;
        self.client.post(path).json(&body).send_json().await
    }

    /// Gets a managed input.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Status`] with status 404 if the managed input does not
    /// exist, and the [errors of every method](Platform#errors).
    pub async fn get_managed_input(
        &self,
        workspace_id: &str,
        managed_input_id: &str,
    ) -> Result<ManagedInput> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "inputs",
            managed_input_id,
        ])?;
        self.client.get(path).send_json().await
    }

    /// Lists the managed inputs of a workspace.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn list_managed_inputs(&self, workspace_id: &str) -> Result<Vec<ManagedInput>> {
        let path = api_path(&["v1", "applications", workspace_id, "inputs"])?;
        self.client.get(path).paginate().collect().await
    }

    /// Updates a managed input.
    ///
    /// Fields that `params` leaves unset keep their values.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `params` sets no field.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn update_managed_input(
        &self,
        workspace_id: &str,
        managed_input_id: &str,
        params: ManagedInputUpdateParams,
    ) -> Result<ManagedInput> {
        if params.name.is_none() && params.description.is_none() {
            return Err(Error::Invalid {
                reason: "nothing to update: set `name` or `description`".to_owned(),
            });
        }
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "inputs",
            managed_input_id,
        ])?;
        // The whole managed input goes back, so fields this SDK does not
        // model keep their values too.
        let mut managed_input: Map<String, Value> = self.client.get(&path).send_json().await?;
        if let Some(name) = params.name {
            managed_input.insert("name".to_owned(), name.into());
        }
        if let Some(description) = params.description {
            managed_input.insert("description".to_owned(), description.into());
        }
        self.client.put(path).json(&managed_input).send_json().await
    }

    /// Deletes a managed input.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn delete_managed_input(
        &self,
        workspace_id: &str,
        managed_input_id: &str,
    ) -> Result<()> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "inputs",
            managed_input_id,
        ])?;
        self.client.delete(path).send().await?;
        Ok(())
    }
}

pub(crate) fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "decision_platform_managed_input_create",
            "Creates a managed input from an upload or from a run's input.",
            |cx, a: InWorkspace<ManagedInputCreateParams>| async move {
                cx.platform()?
                    .create_managed_input(&a.workspace_id, a.params)
                    .await
            },
        ),
        Tool::new(
            "decision_platform_managed_input_get",
            "Gets a managed input.",
            |cx, a: ManagedInputRef<NoParams>| async move {
                cx.platform()?
                    .get_managed_input(&a.workspace_id, &a.managed_input_id)
                    .await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_managed_input_list",
            "Lists the managed inputs of a workspace.",
            |cx, a: InWorkspace<NoParams>| async move {
                cx.platform()?.list_managed_inputs(&a.workspace_id).await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_managed_input_update",
            "Updates a managed input.",
            |cx, a: ManagedInputRef<ManagedInputUpdateParams>| async move {
                cx.platform()?
                    .update_managed_input(&a.workspace_id, &a.managed_input_id, a.params)
                    .await
            },
        )
        .destructive(),
        Tool::new(
            "decision_platform_managed_input_delete",
            "Deletes a managed input.",
            |cx, a: ManagedInputRef<NoParams>| async move {
                cx.platform()?
                    .delete_managed_input(&a.workspace_id, &a.managed_input_id)
                    .await
            },
        )
        .destructive(),
    ]
}
