//! Build operations, on `v1/applications/{workspace_id}/versions`.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::Platform;
use crate::decision::Build;
use crate::decision::id::safe_id;
use crate::decision::path::api_path;
use crate::tool::{InWorkspace, NoParams, Tool};
use crate::{Error, Result};

/// Tool arguments that address a build, plus the operation's parameters.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct BuildRef<P> {
    /// ID of the workspace.
    pub(crate) workspace_id: String,
    /// ID of the build.
    pub(crate) build_id: String,
    #[serde(flatten)]
    pub(crate) params: P,
}

/// Parameters for [`Platform::create_build`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct BuildCreateParams {
    /// ID of the build. Generated when not set.
    pub build_id: Option<String>,
    /// Name of the build. Defaults to its ID.
    pub name: Option<String>,
    /// Description of the build.
    pub description: Option<String>,
    /// Whether to return the build with this ID if it exists, instead of
    /// creating one. Needs `build_id`.
    pub exist_ok: bool,
}

/// Parameters for [`Platform::update_build`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct BuildUpdateParams {
    /// New name of the build.
    pub name: Option<String>,
    /// New description of the build.
    pub description: Option<String>,
}

impl Platform {
    /// Creates a build from the workspace's latest pushed binary.
    ///
    /// An unset or empty `build_id` is generated, and an unset or empty
    /// `name` defaults to the ID. With `exist_ok`, an existing build with the
    /// same ID is returned instead.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `exist_ok` is set without a `build_id`.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn create_build(
        &self,
        workspace_id: &str,
        params: BuildCreateParams,
    ) -> Result<Build> {
        let build_id = match params.build_id.filter(|build_id| !build_id.is_empty()) {
            Some(build_id) => build_id,
            None if params.exist_ok => {
                return Err(Error::Invalid {
                    reason: "`exist_ok` needs a `build_id`".to_owned(),
                });
            }
            None => safe_id("build"),
        };
        if params.exist_ok {
            match self.get_build(workspace_id, &build_id).await {
                Err(Error::Status { status: 404, .. }) => {}
                found => return found,
            }
        }
        let name = match params.name.filter(|name| !name.is_empty()) {
            Some(name) => name,
            None => build_id.clone(),
        };
        let mut body = json!({ "id": build_id, "name": name });
        if let Some(description) = params.description {
            body["description"] = description.into();
        }
        let path = api_path(&["v1", "applications", workspace_id, "versions"])?;
        self.client.post(path).json(&body).send_json().await
    }

    /// Gets a build.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Status`] with status 404 if the build does not exist,
    /// and the [errors of every method](Platform#errors).
    pub async fn get_build(&self, workspace_id: &str, build_id: &str) -> Result<Build> {
        let path = api_path(&["v1", "applications", workspace_id, "versions", build_id])?;
        self.client.get(path).send_json().await
    }

    /// Checks whether a build exists.
    ///
    /// Returns `false` when the platform answers `404 Not Found`.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors), except a 404,
    /// which is `false`.
    pub async fn build_exists(&self, workspace_id: &str, build_id: &str) -> Result<bool> {
        let path = api_path(&["v1", "applications", workspace_id, "versions", build_id])?;
        match self.client.get(path).send().await {
            Ok(_) => Ok(true),
            Err(Error::Status { status: 404, .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Lists the builds of a workspace.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn list_builds(&self, workspace_id: &str) -> Result<Vec<Build>> {
        let path = api_path(&["v1", "applications", workspace_id, "versions"])?;
        self.client.get(path).paginate().collect().await
    }

    /// Updates a build.
    ///
    /// Fields that `params` leaves unset keep their values. An empty string
    /// counts as set.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `params` sets no field, before any
    /// request.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn update_build(
        &self,
        workspace_id: &str,
        build_id: &str,
        params: BuildUpdateParams,
    ) -> Result<Build> {
        if params.name.is_none() && params.description.is_none() {
            return Err(Error::Invalid {
                reason: "nothing to update: set `name` or `description`".to_owned(),
            });
        }
        let path = api_path(&["v1", "applications", workspace_id, "versions", build_id])?;
        // The whole build goes back, so fields this SDK does not model keep
        // their values too.
        let mut build: Map<String, Value> = self.client.get(&path).send_json().await?;
        if let Some(name) = params.name {
            build.insert("name".to_owned(), name.into());
        }
        if let Some(description) = params.description {
            build.insert("description".to_owned(), description.into());
        }
        self.client.put(path).json(&build).send_json().await
    }

    /// Deletes a build.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn delete_build(&self, workspace_id: &str, build_id: &str) -> Result<()> {
        let path = api_path(&["v1", "applications", workspace_id, "versions", build_id])?;
        self.client.delete(path).send().await?;
        Ok(())
    }
}

pub(crate) fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "decision_platform_build_create",
            "Creates a build from the workspace's latest pushed binary.",
            |cx, a: InWorkspace<BuildCreateParams>| async move {
                cx.platform()?.create_build(&a.workspace_id, a.params).await
            },
        ),
        Tool::new(
            "decision_platform_build_get",
            "Gets a build.",
            |cx, a: BuildRef<NoParams>| async move {
                cx.platform()?.get_build(&a.workspace_id, &a.build_id).await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_build_exists",
            "Checks whether a build exists.",
            |cx, a: BuildRef<NoParams>| async move {
                cx.platform()?
                    .build_exists(&a.workspace_id, &a.build_id)
                    .await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_build_list",
            "Lists the builds of a workspace.",
            |cx, a: InWorkspace<NoParams>| async move {
                cx.platform()?.list_builds(&a.workspace_id).await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_build_update",
            "Updates a build.",
            |cx, a: BuildRef<BuildUpdateParams>| async move {
                cx.platform()?
                    .update_build(&a.workspace_id, &a.build_id, a.params)
                    .await
            },
        )
        .destructive(),
        Tool::new(
            "decision_platform_build_delete",
            "Deletes a build.",
            |cx, a: BuildRef<NoParams>| async move {
                cx.platform()?
                    .delete_build(&a.workspace_id, &a.build_id)
                    .await
            },
        )
        .destructive(),
    ]
}
