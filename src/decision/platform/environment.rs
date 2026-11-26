//! Environment operations, on `v1/applications/{workspace_id}/instances`.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::Platform;
use super::build::BuildRef;
use crate::decision::id::safe_id;
use crate::decision::path::api_path;
use crate::decision::{ContentFormat, Environment, EnvironmentConfiguration, RunQueuing};
use crate::tool::{InWorkspace, NoParams, Tool};
use crate::{Error, Result};

/// Execution class of the environments that run through an integration.
const INTEGRATION: &str = "integration";

/// The reserved environment ID that names a workspace's default
/// environment.
///
/// A call that runs on an environment, such as [`Platform::create_run`],
/// reads `default` as the workspace's
/// [`default_environment_id`](crate::decision::Workspace::default_environment_id)
/// when it has one. It reads the workspace first, only for this ID. A
/// workspace's first push makes [`LATEST_ENVIRONMENT_ID`] its default, so a
/// workspace without one has usually never been pushed, and has no
/// `latest` yet. Then a run sends no environment, and the platform records
/// it in its reserved `devint`, which needs no push for a tracked run;
/// [`Platform::create_input_set`], which needs an environment that exists,
/// reads `default` as `latest`. No environment can have this ID:
/// [`Platform::create_environment`] rejects it, and so does a workspace's
/// default environment.
pub const DEFAULT_ENVIRONMENT_ID: &str = "default";

/// The environment that the platform makes for every workspace, which runs
/// the build of the workspace's last push.
pub const LATEST_ENVIRONMENT_ID: &str = "latest";

/// Why [`DEFAULT_ENVIRONMENT_ID`] can't be an environment's ID.
const RESERVED: &str = "`default` is reserved: it names the workspace's default environment";

/// [`Error::Invalid`] if `environment_id` is [`DEFAULT_ENVIRONMENT_ID`],
/// which no environment can have.
pub(crate) fn check_not_reserved(environment_id: Option<&str>) -> Result<()> {
    match environment_id {
        Some(DEFAULT_ENVIRONMENT_ID) => Err(Error::Invalid {
            reason: RESERVED.to_owned(),
        }),
        _ => Ok(()),
    }
}

/// Tool arguments that address an environment, plus the operation's parameters.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct EnvironmentRef<P> {
    /// ID of the workspace.
    pub(crate) workspace_id: String,
    /// ID of the environment.
    pub(crate) environment_id: String,
    #[serde(flatten)]
    pub(crate) params: P,
}

/// Parameters for [`Platform::create_environment`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct EnvironmentCreateParams {
    /// ID of the environment. Generated when not set. Can't be `default`,
    /// which names the workspace's default environment.
    pub environment_id: Option<String>,
    /// Name of the environment. Defaults to its ID.
    pub name: Option<String>,
    /// Description of the environment.
    pub description: Option<String>,
    /// Configuration to run the build with. The platform's defaults apply to
    /// the fields that are not set.
    pub configuration: Option<EnvironmentConfiguration>,
    /// Whether to return the environment with this ID if it exists, instead of
    /// creating one. Needs `environment_id`.
    pub exist_ok: bool,
}

/// Parameters for [`Platform::update_environment`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct EnvironmentUpdateParams {
    /// New name of the environment.
    pub name: Option<String>,
    /// New description of the environment.
    pub description: Option<String>,
    /// ID of the new build the environment runs.
    pub build_id: Option<String>,
    /// Changes to the environment's configuration. Only the fields that are
    /// set change, and in `queuing`, only its fields that are set. `options`
    /// replaces all the options. An execution class other than `integration`,
    /// or an empty `integration_id`, removes the integration.
    pub configuration: Option<EnvironmentConfiguration>,
    /// Whether to lock (true) or unlock (false) the environment. A locked
    /// environment can't be updated or deleted. Unset leaves it as it is.
    pub locked: Option<bool>,
}

impl Platform {
    /// `environment_id`, unless it is [`DEFAULT_ENVIRONMENT_ID`]: then the
    /// workspace's default environment, or `None` when it has none, for the
    /// platform's own fallback. Reads the workspace only for that ID.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Platform::get_workspace`].
    pub(crate) async fn resolve_environment(
        &self,
        workspace_id: &str,
        environment_id: &str,
    ) -> Result<Option<String>> {
        if environment_id != DEFAULT_ENVIRONMENT_ID {
            return Ok(Some(environment_id.to_owned()));
        }
        let workspace = self.get_workspace(workspace_id).await?;
        Ok(workspace
            .default_environment_id
            .filter(|environment_id| !environment_id.is_empty()))
    }

    /// Creates an environment that runs a build.
    ///
    /// An unset or empty `environment_id` is generated, and an unset or empty
    /// `name` defaults to the ID. With `exist_ok`, an existing environment
    /// with the same ID is returned instead.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`], before any request, if `environment_id`
    /// is [`DEFAULT_ENVIRONMENT_ID`], which is reserved, if `exist_ok` is set
    /// without an `environment_id`, or if the configuration is invalid: a
    /// queuing priority outside 1 to 9, an `integration_id` with an execution
    /// class other than `integration`, or an `Unknown` input format.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn create_environment(
        &self,
        workspace_id: &str,
        build_id: &str,
        params: EnvironmentCreateParams,
    ) -> Result<Environment> {
        check_not_reserved(params.environment_id.as_deref())?;
        let configuration = params.configuration.map(checked).transpose()?;
        let environment_id = match params
            .environment_id
            .filter(|environment_id| !environment_id.is_empty())
        {
            Some(environment_id) => environment_id,
            None if params.exist_ok => {
                return Err(Error::Invalid {
                    reason: "`exist_ok` needs an `environment_id`".to_owned(),
                });
            }
            None => safe_id("environment"),
        };
        if params.exist_ok {
            match self.get_environment(workspace_id, &environment_id).await {
                Err(Error::Status { status: 404, .. }) => {}
                found => return found,
            }
        }
        let name = match params.name.filter(|name| !name.is_empty()) {
            Some(name) => name,
            None => environment_id.clone(),
        };
        let mut body = json!({ "id": environment_id, "name": name, "version_id": build_id });
        if let Some(description) = params.description {
            body["description"] = description.into();
        }
        if let Some(configuration) = configuration {
            body["configuration"] = to_json(&configuration)?;
        }
        let path = api_path(&["v1", "applications", workspace_id, "instances"])?;
        self.client.post(path).json(&body).send_json().await
    }

    /// Gets an environment.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Status`] with status 404 if the environment does not
    /// exist, and the [errors of every method](Platform#errors).
    pub async fn get_environment(
        &self,
        workspace_id: &str,
        environment_id: &str,
    ) -> Result<Environment> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "instances",
            environment_id,
        ])?;
        self.client.get(path).send_json().await
    }

    /// Checks whether an environment exists.
    ///
    /// Returns `false` when the platform answers `404 Not Found`.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors), except a 404,
    /// which is `false`.
    pub async fn environment_exists(
        &self,
        workspace_id: &str,
        environment_id: &str,
    ) -> Result<bool> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "instances",
            environment_id,
        ])?;
        match self.client.get(path).send().await {
            Ok(_) => Ok(true),
            Err(Error::Status { status: 404, .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Lists the environments of a workspace.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn list_environments(&self, workspace_id: &str) -> Result<Vec<Environment>> {
        let path = api_path(&["v1", "applications", workspace_id, "instances"])?;
        self.client.get(path).paginate().collect().await
    }

    /// Updates an environment.
    ///
    /// Fields that `params` leaves unset keep their values, inside the
    /// configuration and its queuing settings too. The platform refuses to
    /// update a locked environment, so `locked: false` unlocks it before the
    /// other changes, and `locked: true` locks it after them.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `environment_id` is
    /// [`DEFAULT_ENVIRONMENT_ID`], which is reserved, if `params` changes
    /// nothing, or if the configuration is invalid, as in
    /// [`Platform::create_environment`], before any request. Also returns it,
    /// instead of sending the update the
    /// platform would refuse, if the fetched environment is locked and
    /// `locked` is not `false`.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn update_environment(
        &self,
        workspace_id: &str,
        environment_id: &str,
        params: EnvironmentUpdateParams,
    ) -> Result<Environment> {
        check_not_reserved(Some(environment_id))?;
        let EnvironmentUpdateParams {
            name,
            description,
            build_id,
            configuration,
            locked,
        } = params;
        let configuration = configuration
            .filter(|configuration| !sets_nothing(configuration))
            .map(checked)
            .transpose()?;
        let changes = name.is_some()
            || description.is_some()
            || build_id.is_some()
            || configuration.is_some();
        if !changes {
            return match locked {
                Some(locked) => {
                    self.lock_environment(workspace_id, environment_id, locked)
                        .await
                }
                None => Err(Error::Invalid {
                    reason: "the update changes nothing: set at least one field".to_owned(),
                }),
            };
        }
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "instances",
            environment_id,
        ])?;
        if locked == Some(false) {
            self.lock_environment(workspace_id, environment_id, false)
                .await?;
        }
        // The whole environment goes back, so fields this SDK does not model
        // keep their values too.
        let mut environment: Map<String, Value> = self.client.get(&path).send_json().await?;
        // The platform would refuse the PUT with a 403 that doesn't say why.
        if locked != Some(false) && environment.get("locked") == Some(&Value::Bool(true)) {
            return Err(Error::Invalid {
                reason: "the environment is locked: set `locked` to false to update it".to_owned(),
            });
        }
        if let Some(name) = name {
            environment.insert("name".to_owned(), name.into());
        }
        if let Some(description) = description {
            environment.insert("description".to_owned(), description.into());
        }
        if let Some(build_id) = build_id {
            environment.insert("version_id".to_owned(), build_id.into());
        }
        if let Some(configuration) = configuration {
            merge_configuration(&mut environment, &configuration)?;
        }
        let updated = self.client.put(path).json(&environment).send_json().await?;
        match locked {
            Some(true) => {
                self.lock_environment(workspace_id, environment_id, true)
                    .await
            }
            _ => Ok(updated),
        }
    }

    /// Deletes an environment.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn delete_environment(&self, workspace_id: &str, environment_id: &str) -> Result<()> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "instances",
            environment_id,
        ])?;
        self.client.delete(path).send().await?;
        Ok(())
    }

    /// Locks or unlocks an environment, and returns it.
    async fn lock_environment(
        &self,
        workspace_id: &str,
        environment_id: &str,
        locked: bool,
    ) -> Result<Environment> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "instances",
            environment_id,
            "lock",
        ])?;
        self.client
            .put(path)
            .json(&json!({ "locked": locked }))
            .send_json()
            .await
    }
}

/// `configuration` after the checks that run before sending one: the input
/// format is known, the queuing priority is 1 to 9, and an integration runs
/// on the `integration` execution class, with only an input format.
fn checked(mut configuration: EnvironmentConfiguration) -> Result<EnvironmentConfiguration> {
    if let Some(format) = &configuration.format
        && format.format_input.input_type == ContentFormat::Unknown
    {
        return Err(Error::Invalid {
            reason: "the input `format` must be `json` or `multi-file`".to_owned(),
        });
    }
    let priority = configuration.queuing.as_ref().and_then(|q| q.priority);
    if let Some(priority) = priority.filter(|priority| !(1..=9).contains(priority)) {
        return Err(Error::Invalid {
            reason: format!("the queuing `priority` must be between 1 and 9, not {priority}"),
        });
    }
    if configuration
        .integration_id
        .as_deref()
        .is_some_and(|integration_id| !integration_id.is_empty())
    {
        if !matches!(
            configuration.execution_class.as_deref(),
            None | Some("" | INTEGRATION)
        ) {
            return Err(Error::Invalid {
                reason: format!(
                    "when `integration_id` is set, `execution_class` must be `{INTEGRATION}` or unset"
                ),
            });
        }
        configuration.execution_class = Some(INTEGRATION.to_owned());
        if let Some(format) = &mut configuration.format {
            format.format_output = None;
        }
    }
    Ok(configuration)
}

/// Whether `configuration` sets no field, so it changes nothing.
fn sets_nothing(configuration: &EnvironmentConfiguration) -> bool {
    let EnvironmentConfiguration {
        execution_class,
        format,
        options,
        secrets_collection_id,
        queuing,
        integration_id,
    } = configuration;
    execution_class.is_none()
        && format.is_none()
        && options.is_none()
        && secrets_collection_id.is_none()
        && integration_id.is_none()
        && queuing
            .as_ref()
            .is_none_or(|RunQueuing { priority, disabled }| {
                priority.is_none() && disabled.is_none()
            })
}

/// Writes the fields that `changes` sets into the configuration of
/// `environment`, a fetched environment. Each one replaces the stored value,
/// except that `queuing` keeps the stored fields it does not set.
///
/// The platform rejects an `integration_id`, even an empty one, with any
/// execution class but `integration`. So, as [`checked`] moves an
/// environment with an `integration_id` to that class, another class removes
/// the stored `integration_id`, and so does an empty one.
fn merge_configuration(
    environment: &mut Map<String, Value>,
    changes: &EnvironmentConfiguration,
) -> Result<()> {
    let configuration = environment
        .entry("configuration")
        .or_insert_with(|| json!({}));
    if !configuration.is_object() {
        *configuration = json!({});
    }
    let (Value::Object(stored), Value::Object(changes)) = (configuration, to_json(changes)?) else {
        return Ok(());
    };
    let class = changes.get("execution_class").and_then(Value::as_str);
    if !changes.contains_key("integration_id")
        && class.is_some_and(|class| !class.is_empty() && class != INTEGRATION)
    {
        stored.remove("integration_id");
    }
    for (key, value) in changes {
        match (key.as_str(), value) {
            ("integration_id", Value::String(integration_id)) if integration_id.is_empty() => {
                stored.remove("integration_id");
            }
            ("queuing", Value::Object(fields)) => match stored.get_mut("queuing") {
                Some(Value::Object(queuing)) => queuing.extend(fields),
                _ => {
                    stored.insert(key, Value::Object(fields));
                }
            },
            (_, value) => {
                stored.insert(key, value);
            }
        }
    }
    Ok(())
}

/// `configuration` as the JSON object the API reads, without the fields it
/// does not set.
fn to_json(configuration: &EnvironmentConfiguration) -> Result<Value> {
    serde_json::to_value(configuration).map_err(|error| Error::Encode {
        reason: "the environment configuration is not serializable".to_owned(),
        source: Some(Box::new(error)),
    })
}

pub(crate) fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "decision_platform_environment_create",
            "Creates an environment that runs a build.",
            |cx, a: BuildRef<EnvironmentCreateParams>| async move {
                cx.platform()?
                    .create_environment(&a.workspace_id, &a.build_id, a.params)
                    .await
            },
        ),
        Tool::new(
            "decision_platform_environment_get",
            "Gets an environment.",
            |cx, a: EnvironmentRef<NoParams>| async move {
                cx.platform()?
                    .get_environment(&a.workspace_id, &a.environment_id)
                    .await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_environment_exists",
            "Checks whether an environment exists.",
            |cx, a: EnvironmentRef<NoParams>| async move {
                cx.platform()?
                    .environment_exists(&a.workspace_id, &a.environment_id)
                    .await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_environment_list",
            "Lists the environments of a workspace.",
            |cx, a: InWorkspace<NoParams>| async move {
                cx.platform()?.list_environments(&a.workspace_id).await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_environment_update",
            "Updates an environment.",
            |cx, a: EnvironmentRef<EnvironmentUpdateParams>| async move {
                cx.platform()?
                    .update_environment(&a.workspace_id, &a.environment_id, a.params)
                    .await
            },
        )
        .destructive(),
        Tool::new(
            "decision_platform_environment_delete",
            "Deletes an environment.",
            |cx, a: EnvironmentRef<NoParams>| async move {
                cx.platform()?
                    .delete_environment(&a.workspace_id, &a.environment_id)
                    .await
            },
        )
        .destructive(),
    ]
}
