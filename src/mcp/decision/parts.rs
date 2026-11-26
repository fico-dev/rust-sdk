//! The heavy parts of a run, its output, logs and input, that `include`
//! adds to a result: inline when small, else in the cache (local mode), a
//! resource link (remote mode), or the run's own files (local runs).

use std::path::Path;

use serde_json::{Map, Value};

use crate::decision::local::Local;
use crate::decision::platform::{Platform, RunGetParams, RunInputGetParams, RunLogsGetParams};
use crate::decision::{ContentFormat, LocalWorkspaceRef, RunInformation, RunStatus};
use crate::mcp::cache::{self, Part, Placed};
use crate::{Error, Result};

/// The format of `information`'s output: its own, or its input's.
fn output_format(information: &RunInformation) -> ContentFormat {
    let format = &information.metadata.format;
    format
        .format_output
        .as_ref()
        .map_or(format.format_input.input_type, |output| output.output_type)
}

/// Why `part` of a run in `status` is not there yet, or `None` if it is.
fn not_yet(part: Part, status: RunStatus) -> Option<String> {
    match (part, status) {
        (Part::Input, _) => None,
        (Part::Output, RunStatus::Succeeded) => None,
        (Part::Output, RunStatus::Failed | RunStatus::Canceled) => Some(format!(
            "the run {}, so it has no output; see `error` and its logs",
            crate::mcp::decision::views::wire(&status)
                .as_str()
                .unwrap_or("ended")
        )),
        (Part::Logs, status) if status.is_terminal() => None,
        (_, _) => Some(
            "the run has not ended yet; wait for it with `wait_seconds`, then get it again"
                .to_owned(),
        ),
    }
}

/// Adds `parts` of the platform run `information` to `result`.
///
/// With `local`, the local mode's FICO home, large content goes to the
/// cache, and content already there is read from it. Without it, large
/// content gets a resource link.
pub(super) async fn add_platform(
    platform: &Platform,
    local: Option<&Local>,
    information: &RunInformation,
    parts: &[Part],
    result: &mut Map<String, Value>,
) -> Result<()> {
    for part in parts {
        let placed = place_platform(platform, local, information, *part).await?;
        placed.insert(result, part.as_str());
    }
    Ok(())
}

/// Where `part` of the platform run `information` goes in a result.
async fn place_platform(
    platform: &Platform,
    local: Option<&Local>,
    information: &RunInformation,
    part: Part,
) -> Result<Placed> {
    let metadata = &information.metadata;
    let (workspace_id, run_id) = (metadata.workspace_id.as_str(), information.id.as_str());
    if let Some(reason) = not_yet(part, metadata.status) {
        return Ok(Placed::Missing(reason));
    }
    let multi_file = match part {
        Part::Output => output_format(information) == ContentFormat::MultiFile,
        Part::Input => metadata.format.format_input.input_type == ContentFormat::MultiFile,
        Part::Logs => false,
    };
    let Some(local) = local else {
        if multi_file {
            return Ok(Placed::Missing(format!(
                "the run's {} is multi-file, which only local mode can download",
                part.as_str()
            )));
        }
        // Remote mode never writes, so large parts get a link, by their size
        // when the metadata has it.
        let size = match part {
            Part::Output => Some(metadata.output_size),
            Part::Input => Some(metadata.input_size),
            Part::Logs => None,
        };
        if let Some(size) = size.filter(|size| cache::too_large(*size as u64)) {
            let uri = cache::uri(workspace_id, run_id, part);
            return Ok(Placed::Link(uri, size as u64));
        }
        let content = fetch(platform, workspace_id, run_id, part).await?;
        let text = cache::text(&content);
        return Ok(match cache::too_large(text.len() as u64) {
            true => Placed::Link(cache::uri(workspace_id, run_id, part), text.len() as u64),
            false => Placed::Inline(content),
        });
    };
    let run_dir = cache::run_dir(local.home(), run_id)?;
    if multi_file {
        let dir = cache::dir(&run_dir, part);
        if !cache::exists(&dir).await {
            download_dir(platform, workspace_id, run_id, part, &run_dir).await?;
        }
        return Ok(Placed::Dir(dir));
    }
    let file = cache::file(&run_dir, part);
    if let Some(placed) = cached(&file).await? {
        return Ok(placed);
    }
    let content = fetch(platform, workspace_id, run_id, part).await?;
    let text = cache::text(&content);
    if !cache::too_large(text.len() as u64) {
        return Ok(Placed::Inline(content));
    }
    let bytes = match &content {
        Value::String(text) => text.clone().into_bytes(),
        value => serde_json::to_vec_pretty(value).unwrap_or_default(),
    };
    cache::write(&file, &bytes).await?;
    Ok(Placed::File(file, bytes.len() as u64))
}

/// `part` of a platform run, which must be JSON content or the logs.
async fn fetch(platform: &Platform, workspace_id: &str, run_id: &str, part: Part) -> Result<Value> {
    match part {
        Part::Output => {
            let result = platform
                .get_run(workspace_id, run_id, RunGetParams::default())
                .await?;
            Ok(result.output.map_or(Value::Null, Value::Object))
        }
        Part::Logs => {
            let logs = platform
                .get_run_logs(workspace_id, run_id, RunLogsGetParams::default())
                .await?;
            Ok(Value::String(logs.log))
        }
        Part::Input => {
            let input = platform
                .get_run_input(workspace_id, run_id, RunInputGetParams::default())
                .await?;
            Ok(input.unwrap_or(Value::Null))
        }
    }
}

/// Downloads multi-file `part` of a platform run into its directory in the
/// cache directory `run_dir`: into a new directory next to it first, then
/// renamed into place, so that an interrupted download never looks
/// complete.
async fn download_dir(
    platform: &Platform,
    workspace_id: &str,
    run_id: &str,
    part: Part,
    run_dir: &Path,
) -> Result<()> {
    let dir = cache::dir(run_dir, part);
    let temp = cache::temp_path(&dir);
    let downloaded = match part {
        Part::Output => {
            let params = RunGetParams {
                output_dir: Some(temp.clone()),
                ..RunGetParams::default()
            };
            platform
                .get_run(workspace_id, run_id, params)
                .await
                .map(drop)
        }
        Part::Input => {
            let params = RunInputGetParams {
                output_dir: Some(temp.clone()),
            };
            platform
                .get_run_input(workspace_id, run_id, params)
                .await
                .map(drop)
        }
        Part::Logs => Ok(()),
    };
    if let Err(error) = downloaded {
        let _ = tokio::fs::remove_dir_all(&temp).await;
        return Err(error);
    }
    // An empty directory left by an earlier failed rename is replaced; one
    // that another call filled in the meantime is kept.
    let _ = tokio::fs::remove_dir(&dir).await;
    if let Err(source) = tokio::fs::rename(&temp, &dir).await {
        let _ = tokio::fs::remove_dir_all(&temp).await;
        if !cache::exists(&dir).await {
            return Err(Error::Io { path: dir, source });
        }
    }
    Ok(())
}

/// The content of `file` if it is in the cache: inline when small, else
/// the file itself.
async fn cached(file: &Path) -> Result<Option<Placed>> {
    let metadata = match tokio::fs::metadata(file).await {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => return Ok(None),
    };
    if cache::too_large(metadata.len()) {
        return Ok(Some(Placed::File(file.to_path_buf(), metadata.len())));
    }
    let bytes = tokio::fs::read(file).await.map_err(|source| Error::Io {
        path: file.to_path_buf(),
        source,
    })?;
    // Logs are text; output and input are JSON.
    let value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    Ok(Some(Placed::Inline(value)))
}

/// The content of a part of a platform run, for `resources/read`: its text
/// and its MIME type.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if the part is multi-file, or not there yet.
pub(crate) async fn read_resource(
    platform: &Platform,
    workspace_id: &str,
    run_id: &str,
    part: Part,
) -> Result<String> {
    let information = platform.get_run_information(workspace_id, run_id).await?;
    if let Some(reason) = not_yet(part, information.metadata.status) {
        return Err(Error::Invalid { reason });
    }
    let multi_file = match part {
        Part::Output => output_format(&information) == ContentFormat::MultiFile,
        Part::Input => {
            information.metadata.format.format_input.input_type == ContentFormat::MultiFile
        }
        Part::Logs => false,
    };
    if multi_file {
        return Err(Error::Invalid {
            reason: format!(
                "the run's {} is multi-file, which only local mode can download",
                part.as_str()
            ),
        });
    }
    let content = fetch(platform, workspace_id, run_id, part).await?;
    Ok(match content {
        Value::String(text) => text,
        value => serde_json::to_string_pretty(&value).unwrap_or_default(),
    })
}

/// Adds `parts` of the local run `information`, of `workspace`, to
/// `result`: inline when small, else the path of the run's own file, which
/// is never written. `visuals` adds the paths of its visuals.
pub(super) async fn add_local(
    local: &Local,
    workspace: &LocalWorkspaceRef,
    information: &RunInformation,
    parts: &[Part],
    visuals: bool,
    result: &mut Map<String, Value>,
) -> Result<()> {
    let run_id = information.id.as_str();
    let (_, run_dir) = local.find_run(workspace, run_id).await?;
    let status = information.metadata.status;
    for part in parts {
        let placed = match not_yet(*part, status) {
            Some(reason) => Placed::Missing(reason),
            None => place_local(&run_dir, information, *part).await?,
        };
        placed.insert(result, part.as_str());
    }
    if visuals {
        let placed = match status.is_terminal() {
            true => {
                let paths = local.get_run_visuals(workspace, run_id).await?;
                match paths.is_empty() {
                    true => Placed::Missing("the run's assets have no visuals".to_owned()),
                    false => Placed::Inline(serde_json::json!(paths)),
                }
            }
            false => Placed::Missing(
                "the run has not ended yet; its visuals are made when it ends".to_owned(),
            ),
        };
        placed.insert(result, "visuals");
    }
    Ok(())
}

/// Where `part` of the local run `information`, in `run_dir`, goes in a
/// result.
async fn place_local(run_dir: &Path, information: &RunInformation, part: Part) -> Result<Placed> {
    let multi_file = match part {
        Part::Output => output_format(information) == ContentFormat::MultiFile,
        Part::Input => {
            information.metadata.format.format_input.input_type == ContentFormat::MultiFile
        }
        Part::Logs => false,
    };
    if multi_file {
        return Ok(Placed::Dir(cache::dir(run_dir, part)));
    }
    let file = cache::file(run_dir, part);
    Ok(match cached(&file).await? {
        Some(placed) => placed,
        None if part == Part::Logs => Placed::Inline(Value::String(String::new())),
        None => Placed::Missing(format!("the run has no {} file", part.as_str())),
    })
}
