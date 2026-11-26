//! The assets of platform runs, on
//! `v1/applications/{workspace_id}/runs/{run_id}/assets`.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

use super::Platform;
use super::run::RunRef;
use crate::decision::fs::write_atomically;
use crate::decision::path::api_path;
use crate::decision::{RunAsset, RunAssetContent};
use crate::tool::{NoParams, Tool};
use crate::{Error, Result};

/// Tool arguments that address an asset of a run, plus the operation's
/// parameters.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct RunAssetRef<P> {
    /// ID of the workspace.
    pub(crate) workspace_id: String,
    /// ID of the run.
    pub(crate) run_id: String,
    /// ID of the asset.
    pub(crate) asset_id: String,
    #[serde(flatten)]
    pub(crate) params: P,
}

/// Parameters for [`Platform::get_run_asset`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RunAssetGetParams {
    /// File to write the asset's content to, instead of returning it. Its
    /// directory is created if needed, and a file already there is
    /// overwritten. Not available in remote mode.
    pub output_file: Option<PathBuf>,
}

/// A run's assets, as their list returns them.
#[derive(Deserialize)]
struct RunAssets {
    #[serde(default)]
    items: Vec<RunAsset>,
}

impl Platform {
    /// Lists the assets of a run: their metadata, not their content.
    ///
    /// The platform lists them all at once, visuals first, by their tab.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn list_run_assets(&self, workspace_id: &str, run_id: &str) -> Result<Vec<RunAsset>> {
        let path = assets_path(workspace_id, run_id, None)?;
        let assets: RunAssets = self.client.get(path).send_json().await?;
        Ok(assets.items)
    }

    /// Gets an asset of a run, with its content.
    ///
    /// The content comes from the asset's presigned `download_url`, with no
    /// token. A JSON asset is returned as `content`; with
    /// `params.output_file`, the content is written to that file instead,
    /// and `content` is `None`. An asset of another content type needs
    /// `output_file`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if the asset has no `download_url`, or if
    /// it is not JSON and `params.output_file` is not set, [`Error::Decode`]
    /// if a JSON asset's content is not JSON, and [`Error::Io`] if the file
    /// cannot be written.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn get_run_asset(
        &self,
        workspace_id: &str,
        run_id: &str,
        asset_id: &str,
        params: RunAssetGetParams,
    ) -> Result<RunAssetContent> {
        let path = assets_path(workspace_id, run_id, Some(asset_id))?;
        let asset: RunAsset = self.client.get(path).send_json().await?;
        let Some(url) = asset.download_url.as_deref() else {
            return Err(Error::Invalid {
                reason: format!("the asset `{asset_id}` has no content to download"),
            });
        };
        let json = asset
            .content_type
            .as_deref()
            .is_none_or(|kind| kind == "json");
        if !json && params.output_file.is_none() {
            return Err(Error::Invalid {
                reason: format!(
                    "the asset `{asset_id}` is not JSON: set `output_file` to download it"
                ),
            });
        }
        let response = self.client.get(url).no_auth().send().await?;
        let content = match params.output_file {
            Some(file) => {
                write_atomically(&file, &response.into_bytes()).await?;
                None
            }
            None => Some(response.json()?),
        };
        Ok(RunAssetContent { asset, content })
    }

    /// Deletes an asset of a run.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn delete_run_asset(
        &self,
        workspace_id: &str,
        run_id: &str,
        asset_id: &str,
    ) -> Result<()> {
        let path = assets_path(workspace_id, run_id, Some(asset_id))?;
        self.client.delete(path).send().await?;
        Ok(())
    }
}

/// Path of the assets of the run `run_id`, or of its asset `asset_id`.
fn assets_path(workspace_id: &str, run_id: &str, asset_id: Option<&str>) -> Result<String> {
    let mut path = vec!["v1", "applications", workspace_id, "runs", run_id, "assets"];
    path.extend(asset_id);
    api_path(&path)
}

pub(crate) fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "decision_platform_run_asset_list",
            "Lists the assets of a run, without their content.",
            |cx, a: RunRef<NoParams>| async move {
                cx.platform()?
                    .list_run_assets(&a.workspace_id, &a.run_id)
                    .await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_run_asset_get",
            "Gets an asset of a run, with its content.",
            |cx, a: RunAssetRef<RunAssetGetParams>| async move {
                if a.params.output_file.is_some() {
                    cx.local()?;
                }
                cx.platform()?
                    .get_run_asset(&a.workspace_id, &a.run_id, &a.asset_id, a.params)
                    .await
            },
        )
        .destructive()
        .read_only_in_remote_mode(),
        Tool::new(
            "decision_platform_run_asset_delete",
            "Deletes an asset of a run.",
            |cx, a: RunAssetRef<NoParams>| async move {
                cx.platform()?
                    .delete_run_asset(&a.workspace_id, &a.run_id, &a.asset_id)
                    .await
            },
        )
        .destructive(),
    ]
}
