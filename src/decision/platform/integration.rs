//! Integrations of the account, on `v1/integrations`.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::Platform;
use crate::decision::id::safe_id;
use crate::decision::path::api_path;
use crate::decision::{
    ExecutableType, Integration, IntegrationHealth, IntegrationProvider, IntegrationType,
};
use crate::tool::{NoParams, Tool};
use crate::{Error, Result};

/// Tool arguments that address an integration, plus the operation's
/// parameters.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct IntegrationRef<P> {
    /// ID of the integration.
    pub(crate) integration_id: String,
    #[serde(flatten)]
    pub(crate) params: P,
}

/// Parameters for [`Platform::create_integration`].
///
/// `integration_type`, `executable_types`, `provider` and `provider_config`
/// are required in tool arguments. The `Default` value leaves them unset or
/// empty, so set them.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
pub struct IntegrationCreateParams {
    /// ID of the integration. Generated when not set.
    #[serde(default)]
    pub integration_id: Option<String>,
    /// Name of the integration. Defaults to its ID.
    #[serde(default)]
    pub name: Option<String>,
    /// Description of the integration.
    #[serde(default)]
    pub description: Option<String>,
    /// Whether every workspace of the account may use the integration. Not
    /// with `workspace_ids`.
    #[serde(default)]
    pub is_global: bool,
    /// IDs of the workspaces that may use the integration. Needs at least
    /// one when the integration is not global.
    #[serde(default)]
    pub workspace_ids: Vec<String>,
    /// Kind of integration.
    pub integration_type: IntegrationType,
    /// Types of binary the integration runs. At least one.
    pub executable_types: Vec<ExecutableType>,
    /// The provider of the integration.
    pub provider: IntegrationProvider,
    /// The provider's configuration, such as a Databricks job and its
    /// credentials. The platform checks it for the provider.
    pub provider_config: Value,
    /// Whether to return the integration with this ID if it exists, instead
    /// of creating one. Needs `integration_id`.
    #[serde(default)]
    pub exist_ok: bool,
}

/// Parameters for [`Platform::update_integration`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct IntegrationUpdateParams {
    /// New name of the integration.
    pub name: Option<String>,
    /// New description of the integration.
    pub description: Option<String>,
    /// Whether every workspace of the account may use the integration.
    pub is_global: Option<bool>,
    /// IDs of the workspaces that may use the integration. They replace the
    /// current ones.
    pub workspace_ids: Option<Vec<String>>,
    /// Types of binary the integration runs. They replace the current ones.
    pub executable_types: Option<Vec<ExecutableType>>,
    /// The provider's new configuration. It replaces the current one.
    pub provider_config: Option<Value>,
}

impl Platform {
    /// Creates an integration of the account.
    ///
    /// An unset or empty `integration_id` is generated, and an unset or
    /// empty `name` defaults to the ID. With `exist_ok`, an existing
    /// integration with the same ID is returned instead.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`], before any request, if `exist_ok` is set
    /// without an `integration_id`, if the integration is global and has
    /// `workspace_ids`, or is not global and has none, if
    /// `executable_types` is empty, or if a type, provider or executable
    /// type is `Unknown`.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn create_integration(&self, params: IntegrationCreateParams) -> Result<Integration> {
        let integration_id = match params.integration_id.filter(|id| !id.is_empty()) {
            Some(integration_id) => integration_id,
            None if params.exist_ok => {
                return Err(Error::Invalid {
                    reason: "`exist_ok` needs an `integration_id`".to_owned(),
                });
            }
            None => safe_id("integration"),
        };
        check_access(params.is_global, &params.workspace_ids)?;
        check_executable_types(&params.executable_types)?;
        if params.integration_type == IntegrationType::Unknown {
            return Err(invalid("`integration_type` must be `compute` or `data`"));
        }
        if params.provider == IntegrationProvider::Unknown {
            return Err(invalid("`provider` must be `dbx` or `aws-batch`"));
        }
        if params.exist_ok {
            match self.get_integration(&integration_id).await {
                Err(Error::Status { status: 404, .. }) => {}
                found => return found,
            }
        }
        let name = match params.name.filter(|name| !name.is_empty()) {
            Some(name) => name,
            None => integration_id.clone(),
        };
        let body = json!({
            "id": integration_id,
            "name": name,
            "description": params.description.unwrap_or_default(),
            "global": params.is_global,
            "application_ids": params.workspace_ids,
            "type": params.integration_type,
            "exec_types": params.executable_types,
            "provider": params.provider,
            "provider_config": params.provider_config,
        });
        let path = api_path(&["v1", "integrations"])?;
        self.client.post(path).json(&body).send_json().await
    }

    /// Gets an integration, with its `provider_config`, credentials included,
    /// in clear text.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Status`] with status 404 if the integration does not
    /// exist, and the [errors of every method](Platform#errors).
    pub async fn get_integration(&self, integration_id: &str) -> Result<Integration> {
        let path = api_path(&["v1", "integrations", integration_id])?;
        self.client.get(path).send_json().await
    }

    /// Checks whether an integration exists.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors), except a 404,
    /// which is `false`.
    pub async fn integration_exists(&self, integration_id: &str) -> Result<bool> {
        let path = api_path(&["v1", "integrations", integration_id])?;
        match self.client.get(path).send().await {
            Ok(_) => Ok(true),
            Err(Error::Status { status: 404, .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Lists the integrations of the account, without their
    /// `provider_config`.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn list_integrations(&self) -> Result<Vec<Integration>> {
        let path = api_path(&["v1", "integrations"])?;
        self.client.get(path).paginate().collect().await
    }

    /// Updates an integration.
    ///
    /// Fields that `params` leaves unset keep their values. An empty string
    /// counts as set. The whole integration goes back, `provider_config`
    /// included, as the platform needs it. Its type and provider can't
    /// change.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`], before any request, if `params` sets no
    /// field, if `executable_types` is empty or has an `Unknown` value, or,
    /// after the integration is read, if the result would be global with
    /// workspaces, or not global without any.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn update_integration(
        &self,
        integration_id: &str,
        params: IntegrationUpdateParams,
    ) -> Result<Integration> {
        let IntegrationUpdateParams {
            name,
            description,
            is_global,
            workspace_ids,
            executable_types,
            provider_config,
        } = params;
        if name.is_none()
            && description.is_none()
            && is_global.is_none()
            && workspace_ids.is_none()
            && executable_types.is_none()
            && provider_config.is_none()
        {
            return Err(invalid(
                "nothing to update: set `name`, `description`, `is_global`, `workspace_ids`, \
                 `executable_types` or `provider_config`",
            ));
        }
        if let Some(types) = &executable_types {
            check_executable_types(types)?;
        }
        let path = api_path(&["v1", "integrations", integration_id])?;
        // The whole integration goes back, so fields this SDK does not model
        // keep their values too.
        let mut integration: Map<String, Value> = self.client.get(&path).send_json().await?;
        if let Some(name) = name {
            integration.insert("name".to_owned(), name.into());
        }
        if let Some(description) = description {
            integration.insert("description".to_owned(), description.into());
        }
        if let Some(is_global) = is_global {
            integration.insert("global".to_owned(), is_global.into());
        }
        if let Some(workspace_ids) = workspace_ids {
            integration.insert("application_ids".to_owned(), json!(workspace_ids));
        }
        if let Some(types) = executable_types {
            integration.insert("exec_types".to_owned(), json!(types));
        }
        if let Some(config) = provider_config {
            integration.insert("provider_config".to_owned(), config);
        }
        let global = integration.get("global") == Some(&Value::Bool(true));
        let workspace_ids: Vec<String> = integration
            .get("application_ids")
            .and_then(|ids| serde_json::from_value(ids.clone()).ok())
            .unwrap_or_default();
        check_access(global, &workspace_ids)?;
        self.client.put(path).json(&integration).send_json().await
    }

    /// Deletes an integration.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn delete_integration(&self, integration_id: &str) -> Result<()> {
        let path = api_path(&["v1", "integrations", integration_id])?;
        self.client.delete(path).send().await?;
        Ok(())
    }

    /// Gets the health of an integration, as its provider last reported it.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn get_integration_health(&self, integration_id: &str) -> Result<IntegrationHealth> {
        let path = api_path(&["v1", "integrations", integration_id, "health"])?;
        self.client.get(path).send_json().await
    }
}

fn invalid(reason: &str) -> Error {
    Error::Invalid {
        reason: reason.to_owned(),
    }
}

/// Checks who may use an integration: every workspace (global) or the
/// listed ones, not both and not neither.
fn check_access(is_global: bool, workspace_ids: &[String]) -> Result<()> {
    match (is_global, workspace_ids.is_empty()) {
        (true, false) => Err(invalid(
            "a global integration has no `workspace_ids`: every workspace may use it",
        )),
        (false, true) => Err(invalid(
            "an integration that is not global needs `workspace_ids`",
        )),
        _ => Ok(()),
    }
}

/// Checks the executable types of an integration: at least one, all known.
fn check_executable_types(types: &[ExecutableType]) -> Result<()> {
    if types.is_empty() {
        return Err(invalid("`executable_types` needs at least one type"));
    }
    if types.contains(&ExecutableType::Unknown) {
        return Err(invalid(
            "`executable_types` must be known types, such as `python`",
        ));
    }
    Ok(())
}

pub(crate) fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "decision_platform_integration_create",
            "Creates an integration of the account.",
            |cx, a: IntegrationCreateParams| async move {
                cx.platform()?
                    .create_integration(a)
                    .await
                    .map(Integration::without_provider_config)
            },
        ),
        // A client may auto-approve this read-only tool, so its result never
        // carries the provider's configuration, which holds credentials.
        Tool::new(
            "decision_platform_integration_get",
            "Gets an integration, without its provider configuration.",
            |cx, a: IntegrationRef<NoParams>| async move {
                cx.platform()?
                    .get_integration(&a.integration_id)
                    .await
                    .map(Integration::without_provider_config)
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_integration_exists",
            "Checks whether an integration exists.",
            |cx, a: IntegrationRef<NoParams>| async move {
                cx.platform()?.integration_exists(&a.integration_id).await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_integration_list",
            "Lists the integrations of the account, without their `provider_config`.",
            |cx, _: NoParams| async move { cx.platform()?.list_integrations().await },
        )
        .read_only(),
        Tool::new(
            "decision_platform_integration_update",
            "Updates an integration.",
            |cx, a: IntegrationRef<IntegrationUpdateParams>| async move {
                cx.platform()?
                    .update_integration(&a.integration_id, a.params)
                    .await
                    .map(Integration::without_provider_config)
            },
        )
        .destructive(),
        Tool::new(
            "decision_platform_integration_delete",
            "Deletes an integration.",
            |cx, a: IntegrationRef<NoParams>| async move {
                cx.platform()?.delete_integration(&a.integration_id).await
            },
        )
        .destructive(),
        Tool::new(
            "decision_platform_integration_health_get",
            "Gets the health of an integration, as its provider last reported it.",
            |cx, a: IntegrationRef<NoParams>| async move {
                cx.platform()?
                    .get_integration_health(&a.integration_id)
                    .await
            },
        )
        .read_only(),
    ]
}
