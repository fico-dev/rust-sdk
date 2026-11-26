//! Workspaces in the local registry, `<home>/decision/registry.json`, and
//! the sync of their runs to the platform.

use std::io;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use tokio::fs;
use tokio::sync::OnceCell;

use super::registry::{self, Lookup};
use super::{InLocalWorkspace, Local, RunListParams, RunLogsGetParams, run_dir};
use crate::decision::fs::write_atomically;
use crate::decision::id::safe_id;
use crate::decision::manifest::{self, ManifestValidateParams};
use crate::decision::path::api_path;
use crate::decision::platform::{DEFAULT_ENVIRONMENT_ID, Platform, RunTrackParams};
use crate::decision::time::now_rfc3339;
use crate::decision::{
    ContentFormat, LocalWorkspace, LocalWorkspaceRef, Registry, RegistryEntry, RunInformation,
    RunStatus, RunSyncOutcome, RunSyncResult, SyncedRun, TrackedRun, TrackedRunStatus,
    WorkspaceSynced,
};
use crate::tool::{NoParams, Tool};
use crate::{Error, Result};

/// Prefix of the IDs that registering generates.
const ID_PREFIX: &str = "local-workspace";

/// What [`Error::NotFound`] calls a local run, as the local run operations
/// do.
const RUN_KIND: &str = "local run";

/// The environment a sync records runs in when it is given none: the
/// platform workspace's default one, which [`Platform::track_run`] resolves.
const DEFAULT_ENVIRONMENT: &str = DEFAULT_ENVIRONMENT_ID;

/// Tool arguments of a sync: the local workspace, the platform workspace and
/// the operation's parameters.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct SyncRef<P> {
    #[serde(flatten)]
    pub(crate) workspace: LocalWorkspaceRef,
    /// ID of the platform workspace to sync the runs to.
    pub(crate) platform_workspace_id: String,
    #[serde(flatten)]
    pub(crate) params: P,
}

/// Parameters for [`Local::register_workspace`].
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct WorkspaceRegisterParams {
    /// Directory of the workspace: the one that holds its `workspace.yaml`.
    /// A relative path is resolved against the current directory.
    pub src: PathBuf,
    /// ID to register the workspace under, which no other workspace may
    /// have. Generated when not set or empty.
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// Description of the workspace.
    #[serde(default)]
    pub description: Option<String>,
}

/// Parameters for [`Local::update_workspace`]. At least one field must be
/// set.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct WorkspaceUpdateParams {
    /// New description of the workspace.
    pub description: Option<String>,
    /// New ID of the workspace, which no other workspace may have. Runs made
    /// before keep the ID they were made with.
    pub new_workspace_id: Option<String>,
}

/// Parameters for [`Local::sync_workspace`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct WorkspaceSyncParams {
    /// IDs of the local runs to sync, in the order to sync them. Defaults to
    /// every run of the workspace, oldest first.
    pub run_ids: Vec<String>,
    /// ID of the platform environment to record the runs in. Defaults to the
    /// platform workspace's default environment, which `default` also names;
    /// for a workspace without one, the platform's fallback, its reserved
    /// `devint`, which needs no push.
    /// Without it, a run synced to the platform workspace before, in any
    /// environment, is not synced again; with it, only one synced to this
    /// environment.
    pub environment_id: Option<String>,
}

impl Local {
    /// Removes a workspace from the local registry.
    ///
    /// Its files stay where they are.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `workspace` sets neither field, and
    /// [`Error::NotFound`] if no registered workspace matches it.
    pub async fn delete_workspace(&self, workspace: &LocalWorkspaceRef) -> Result<()> {
        let lookup = Lookup::new(workspace).await?;
        registry::change(&self.home, |registry| {
            let found = lookup.find(registry).ok_or_else(|| lookup.not_found())?;
            let (id, src) = (found.workspace_id.clone(), found.src.clone());
            // Every entry with the same ID and directory goes.
            registry
                .workspaces
                .retain(|entry| entry.workspace.workspace_id != id || entry.workspace.src != src);
            Ok(())
        })
        .await
    }

    /// Gets a workspace from the local registry.
    ///
    /// It reads the workspace's manifest too, so the content format is the
    /// one its `workspace.yaml` has now.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `workspace` sets neither field,
    /// [`Error::NotFound`] if no registered workspace matches it,
    /// [`Error::Io`] if its `workspace.yaml` cannot be read, e.g. because its
    /// directory is gone, and [`Error::Manifest`] if the manifest is invalid.
    pub async fn get_workspace(&self, workspace: &LocalWorkspaceRef) -> Result<LocalWorkspace> {
        let mut entry = registry::resolve(&self.home, workspace).await?;
        entry.content_format = content_format(&entry.src).await?;
        Ok(entry)
    }

    /// Lists the workspaces in the local registry.
    ///
    /// They come as the registry holds them, in the order they were
    /// registered, without reading their manifests. Without a registry, the
    /// list is empty.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the registry cannot be read.
    pub async fn list_workspaces(&self) -> Result<Vec<LocalWorkspace>> {
        let registry = registry::load(&self.home).await?;
        Ok(registry
            .workspaces
            .into_iter()
            .map(|entry| entry.workspace)
            .collect())
    }

    /// Registers a workspace directory in the local registry.
    ///
    /// It reads the workspace's manifest, `workspace.yaml`, for its content
    /// format. An unset or empty `workspace_id` is generated. A directory
    /// that is already registered keeps its entry, which is returned
    /// unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if `src` or its `workspace.yaml` cannot be read,
    /// [`Error::Manifest`] if the manifest is invalid, and
    /// [`Error::Invalid`] if `src` is empty or not UTF-8, if it is registered
    /// under another ID than `workspace_id`, or if another directory is
    /// registered under `workspace_id`.
    pub async fn register_workspace(
        &self,
        params: WorkspaceRegisterParams,
    ) -> Result<LocalWorkspace> {
        let (entry, _) = self
            .register(&params.src, params.workspace_id, params.description)
            .await?;
        Ok(entry)
    }

    /// Checks whether a workspace is in the local registry.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `workspace` sets neither field, and
    /// [`Error::Io`] if the registry cannot be read.
    pub async fn workspace_registered(&self, workspace: &LocalWorkspaceRef) -> Result<bool> {
        match registry::resolve(&self.home, workspace).await {
            Ok(_) => Ok(true),
            Err(Error::NotFound { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Updates a workspace in the local registry.
    ///
    /// Fields that `params` leaves unset keep their values. A `src` that is
    /// not registered yet is registered first, in the same change of the
    /// registry, so an update that fails registers nothing. The content
    /// format is read from the workspace's manifest again.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `params` sets no field, if
    /// `new_workspace_id` is empty or another workspace has it, or if
    /// `workspace` sets neither field; [`Error::NotFound`] if no registered
    /// workspace matches `workspace` and it sets no `src`; and the errors of
    /// [`Local::register_workspace`].
    pub async fn update_workspace(
        &self,
        workspace: &LocalWorkspaceRef,
        params: WorkspaceUpdateParams,
    ) -> Result<LocalWorkspace> {
        if params.description.is_none() && params.new_workspace_id.is_none() {
            return Err(Error::Invalid {
                reason: "set `description` or `new_workspace_id` to update a local workspace"
                    .to_owned(),
            });
        }
        if params.new_workspace_id.as_deref() == Some("") {
            return Err(Error::Invalid {
                reason: "`new_workspace_id` must not be empty".to_owned(),
            });
        }
        let lookup = Lookup::new(workspace).await?;
        // The directory of the registered workspace, or the one to register.
        let current = registry::load(&self.home).await?;
        let found = lookup.find(&current).map(|entry| entry.src.clone());
        let (src, register) = match (found, lookup.src()) {
            (Some(src), _) => (src, false),
            (None, Some(src)) => (registrable(src).await?, true),
            (None, None) => return Err(lookup.not_found()),
        };
        let content_format = content_format(&src).await?;
        registry::change(&self.home, |registry| {
            let index = if register {
                let workspace_id = lookup.workspace_id().map(str::to_owned);
                insert(registry, &src, workspace_id, None, content_format)?.0
            } else {
                // Found again by its directory, which nothing changes, in
                // case another writer changed the entry since.
                position(registry, &src).ok_or_else(|| lookup.not_found())?
            };
            if let Some(id) = &params.new_workspace_id {
                check_id_is_free(registry, id, Some(index))?;
            }
            let entry = &mut registry.workspaces[index].workspace;
            if let Some(id) = params.new_workspace_id {
                entry.workspace_id = id;
            }
            if let Some(description) = params.description {
                entry.description = Some(description);
            }
            entry.content_format = content_format;
            entry.updated_at = now_rfc3339();
            Ok(entry.clone())
        })
        .await
    }

    /// Uploads the local runs of a workspace to a platform workspace.
    ///
    /// Each run is recorded in the platform workspace
    /// `platform_workspace_id` as a tracked run, with
    /// [`Platform::track_run`]: its status (succeeded, or failed for any
    /// other end), duration, error, logs, name and description, its input,
    /// its output, and, for multi-file output, its metrics and assets. Then
    /// its information file records the platform run in `synced_runs`, and
    /// nothing else in the file changes, so a second sync sends no requests.
    ///
    /// It syncs the runs of `params.run_ids`, in their order, or else every
    /// run of the workspace, oldest first. A workspace that is not
    /// registered is registered once those runs are found, and the result's
    /// `workspace_registered` says so; a sync that fails before registers
    /// nothing. The result has one outcome per run:
    ///
    /// * `synced`, with the record the sync added;
    /// * `already_synced`, with the record of an earlier sync to this
    ///   platform workspace, or, if `params.environment_id` is set, to this
    ///   environment of it;
    /// * `skipped`, with the reason, for a run that has not ended, whose
    ///   input is gone, whose files cannot be read, or that
    ///   [`Platform::track_run`] rejects before any request, such as a run
    ///   with neither output nor logs.
    ///
    /// Without an environment, or with `default`, the platform workspace is
    /// read once, before the first run to sync, for its default environment,
    /// as [`Platform::track_run`] does, so a platform workspace that does not
    /// exist fails that read with a 404. With another environment, nothing
    /// checks that the platform workspace exists: the first run's request
    /// fails with a 404. The records keep the environment as given.
    ///
    /// # Errors
    ///
    /// Returns, before any request, [`Error::Invalid`] if
    /// `platform_workspace_id` cannot be an ID or a run ID of
    /// `params.run_ids` cannot be a local run ID, and [`Error::NotFound`] if
    /// one of those runs does not exist. Finding the workspace fails as
    /// [`Local::get_workspace`] does, and registering it as
    /// [`Local::register_workspace`] does, and listing its runs as
    /// [`Local::list_runs`] does. A request error of [`Platform::track_run`],
    /// such as [`Error::Status`], and an [`Error::Io`] when a run's
    /// information file cannot be written back, stop the sync: the runs
    /// synced before keep their records, so the next sync skips them.
    pub async fn sync_workspace(
        &self,
        workspace: &LocalWorkspaceRef,
        platform: &Platform,
        platform_workspace_id: &str,
        params: WorkspaceSyncParams,
    ) -> Result<WorkspaceSynced> {
        // Checks the ID as `Platform::track_run` does, before anything else,
        // so that it is not a reason to skip every run.
        api_path(&["v1", "applications", platform_workspace_id])?;
        // Before registering, so that a sync that fails here registers
        // nothing, as for local runs.
        let run_ids = self.runs_to_sync(workspace, params.run_ids).await?;
        let (entry, workspace_registered) = self.get_or_register_workspace(workspace).await?;
        let by_src = LocalWorkspaceRef {
            workspace_id: None,
            src: Some(entry.src.clone()),
        };
        let target = SyncTarget {
            platform,
            workspace_id: platform_workspace_id,
            environment_id: params.environment_id.as_deref().filter(|id| !id.is_empty()),
            resolved: OnceCell::new(),
        };
        let mut runs = Vec::with_capacity(run_ids.len());
        for run_id in run_ids {
            let outcome = self.sync_run(&by_src, &entry.src, &target, &run_id).await?;
            runs.push(RunSyncResult { run_id, outcome });
        }
        Ok(WorkspaceSynced {
            runs,
            workspace_registered,
        })
    }

    /// The IDs of the local runs of `workspace` to sync: `run_ids`, each of
    /// which must exist, or else every run, oldest first. The workspace is
    /// found without registering it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if a run ID cannot be a local run ID,
    /// [`Error::NotFound`] if a run does not exist, [`Error::Io`] if that
    /// cannot be checked, and the errors of finding the workspace, as
    /// [`Local::list_runs`] has them.
    async fn runs_to_sync(
        &self,
        workspace: &LocalWorkspaceRef,
        run_ids: Vec<String>,
    ) -> Result<Vec<String>> {
        if run_ids.is_empty() {
            let mut runs = self.list_runs(workspace, RunListParams::default()).await?;
            // The timestamps all have microseconds, so they sort as strings.
            runs.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.id.cmp(&b.id))
            });
            return Ok(runs.into_iter().map(|run| run.id).collect());
        }
        for run_id in &run_ids {
            let (_, dir) = self.find_run(workspace, run_id).await?;
            check_run_exists(&dir, run_id).await?;
        }
        Ok(run_ids)
    }

    /// Syncs the local run `run_id` of the workspace in `src`, which
    /// `by_src` identifies, to `target`, as [`Local::sync_workspace`] does.
    ///
    /// # Errors
    ///
    /// Returns the errors of reading the run and of [`Platform::track_run`]
    /// that [`skip_or_stop`] does not turn into a skip, and [`Error::Io`] if
    /// the run's information file cannot be written back.
    async fn sync_run(
        &self,
        by_src: &LocalWorkspaceRef,
        src: &Path,
        target: &SyncTarget<'_>,
        run_id: &str,
    ) -> Result<RunSyncOutcome> {
        let params = match self.track_params(by_src, src, target, run_id).await {
            Ok(Ok(params)) => params,
            Ok(Err(outcome)) => return Ok(outcome),
            Err(error) => return skip_or_stop(run_id, error),
        };
        // `track_run` checks the run and reads its files before its first
        // request, so its `Error::Invalid` and `Error::Io` are this run's,
        // and the next run may still sync. The one that comes later, an
        // upload URL from the platform that is not absolute, skips the run
        // too.
        // `Platform::track_run` defaults to `latest`, but a sync records runs
        // in the default environment. Resolved once for the whole sync, so
        // that no run reads the workspace again; `None` is the platform's
        // fallback, for a workspace without a default environment.
        let environment_id = target.environment_id.unwrap_or(DEFAULT_ENVIRONMENT);
        let resolve = || {
            target
                .platform
                .resolve_environment(target.workspace_id, environment_id)
        };
        let resolved = target.resolved.get_or_try_init(resolve).await?;
        let created =
            target
                .platform
                .track_run_in(target.workspace_id, params, resolved.as_deref());
        let created = match created.await {
            Ok(created) => created,
            Err(error) => return skip_or_stop(run_id, error),
        };
        let synced_run = SyncedRun {
            run_id: created.run_id,
            synced_at: now_rfc3339(),
            workspace_id: target.workspace_id.to_owned(),
            environment_id: target.environment_id.map(str::to_owned),
        };
        // The run is on the platform now, so failing to record it stops the
        // sync instead of skipping it.
        record_synced_run(&run_dir::run_dir(src, run_id)?, run_id, &synced_run).await?;
        Ok(RunSyncOutcome::Synced { synced_run })
    }

    /// What tracks the local run `run_id` of the workspace in `src`, which
    /// `by_src` identifies, on `target`, or the run's outcome if it is not
    /// to be tracked: already synced, not ended, or without input.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Local::get_run_information`], and those of
    /// [`tracked_run`].
    async fn track_params(
        &self,
        by_src: &LocalWorkspaceRef,
        src: &Path,
        target: &SyncTarget<'_>,
        run_id: &str,
    ) -> Result<Result<RunTrackParams, RunSyncOutcome>> {
        // Finalizes the run first, and has its metrics.
        let information = self.get_run_information(by_src, run_id).await?;
        if let Some(synced_run) = synced_record(&information, target) {
            return Ok(Err(RunSyncOutcome::AlreadySynced {
                synced_run: synced_run.clone(),
            }));
        }
        if !information.metadata.status.is_terminal() {
            return Ok(Err(skipped(format!(
                "local run `{run_id}` has not ended: only runs that ended are synced"
            ))));
        }
        let dir = run_dir::run_dir(src, run_id)?;
        let Some(mut tracked_run) = tracked_run(&dir, &information).await? else {
            return Ok(Err(skipped(format!(
                "local run `{run_id}` has no input: its input files are gone"
            ))));
        };
        // A run without a log file is synced with empty logs.
        let logs = self.get_run_logs(by_src, run_id, RunLogsGetParams::default());
        tracked_run.logs = Some(logs.await?.log);
        Ok(Ok(RunTrackParams {
            tracked_run,
            // `sync_run` records it in the resolved environment.
            environment_id: None,
            content_format: Some(information.metadata.format.format_input.input_type),
        }))
    }

    /// Gets a workspace from the local registry, or registers its `src`
    /// first if no registered workspace matches it. Also returns whether
    /// this call registered it, so callers can say so.
    ///
    /// Either way it reads the workspace's manifest, so the content format
    /// is the one its `workspace.yaml` has now.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] if no registered workspace matches and
    /// `workspace` sets no `src`, and the errors of
    /// [`Local::get_workspace`] and [`Local::register_workspace`].
    pub(crate) async fn get_or_register_workspace(
        &self,
        workspace: &LocalWorkspaceRef,
    ) -> Result<(LocalWorkspace, bool)> {
        let lookup = Lookup::new(workspace).await?;
        let found = lookup.find(&registry::load(&self.home).await?).cloned();
        if let Some(mut entry) = found {
            entry.content_format = content_format(&entry.src).await?;
            return Ok((entry, false));
        }
        match lookup.src() {
            Some(src) => {
                let workspace_id = lookup.workspace_id().map(str::to_owned);
                self.register(src, workspace_id, None).await
            }
            None => Err(lookup.not_found()),
        }
    }

    /// Registers `src`, as [`Local::register_workspace`] does, and also
    /// returns whether it was not registered before.
    async fn register(
        &self,
        src: &Path,
        workspace_id: Option<String>,
        description: Option<String>,
    ) -> Result<(LocalWorkspace, bool)> {
        let src = registrable(src).await?;
        let content_format = content_format(&src).await?;
        registry::change(&self.home, |registry| {
            let (index, created) =
                insert(registry, &src, workspace_id, description, content_format)?;
            Ok((registry.workspaces[index].workspace.clone(), created))
        })
        .await
    }
}

/// Where [`Local::sync_workspace`] records runs.
struct SyncTarget<'a> {
    platform: &'a Platform,
    /// ID of the platform workspace.
    workspace_id: &'a str,
    /// ID of the environment, if one was given.
    environment_id: Option<&'a str>,
    /// The environment the runs are recorded in, once a run needs it:
    /// `environment_id`, or the default one, resolved; `None` for the
    /// platform's fallback.
    resolved: OnceCell<Option<String>>,
}

/// Checks that the local run `run_id`, whose directory is `dir`, exists.
///
/// # Errors
///
/// Returns [`Error::NotFound`] if the run has no information file, and
/// [`Error::Io`] if that cannot be checked.
async fn check_run_exists(dir: &Path, run_id: &str) -> Result<()> {
    let path = run_dir::information_file(dir, run_id);
    match fs::try_exists(&path).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(Error::NotFound {
            kind: RUN_KIND,
            id: run_id.to_owned(),
        }),
        Err(source) => Err(Error::Io { path, source }),
    }
}

/// The record of an earlier sync of the run `information` to `target`: one
/// for the same platform workspace, and, if `target` has an environment,
/// for the same environment.
///
/// `default` and no environment are the same environment, the platform
/// workspace's default one, since [`Platform::track_run`] sends neither.
fn synced_record<'a>(
    information: &'a RunInformation,
    target: &SyncTarget<'_>,
) -> Option<&'a SyncedRun> {
    let same_environment = |record: Option<&str>| match (target.environment_id, record) {
        (None, _) => true,
        (Some(DEFAULT_ENVIRONMENT), None) => true,
        (Some(id), record) => record == Some(id),
    };
    let records = information.synced_runs.iter().flatten();
    records.into_iter().find(|record| {
        record.workspace_id == target.workspace_id
            && same_environment(record.environment_id.as_deref())
    })
}

/// The outcome of a run that is not synced, because of `reason`.
fn skipped(reason: String) -> RunSyncOutcome {
    RunSyncOutcome::Skipped { reason }
}

/// The outcome of the local run `run_id` when reading it or tracking it
/// failed with `error`: skipped, for the errors that concern only this run
/// and come before any request, and otherwise `error`, which stops the
/// sync.
fn skip_or_stop(run_id: &str, error: Error) -> Result<RunSyncOutcome> {
    let reason = match &error {
        Error::Invalid { reason, .. } => reason.clone(),
        Error::Io { path, source, .. } => {
            format!("could not access `{}`: {source}", path.display())
        }
        Error::NotFound { .. } => error.to_string(),
        _ => return Err(error),
    };
    Ok(skipped(format!(
        "local run `{run_id}` cannot be synced: {reason}"
    )))
}

/// The tracked run of the final local run `information`, whose directory
/// is `dir`, without its logs; or `None` if its input is gone.
///
/// Its input is `inputs/input.json` or `inputs/`. Its output is
/// `outputs/solutions/solution.json`, only if it succeeded, or
/// `outputs/solutions/`, if it exists. A run with multi-file output also
/// has its metrics, from `information`, and its assets, from
/// `outputs/assets.json`, if they are a list.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if the run has an input or output format this
/// SDK does not know, and [`Error::Io`] if its files cannot be read or its
/// assets are not JSON.
async fn tracked_run(dir: &Path, information: &RunInformation) -> Result<Option<TrackedRun>> {
    let metadata = &information.metadata;
    let format = &metadata.format;
    let input_format = format.format_input.input_type;
    // Without an output format, the output has the input's, as for local
    // run reads.
    let output_format = format
        .format_output
        .as_ref()
        .map_or(input_format, |output| output.output_type);
    let status = match metadata.status {
        RunStatus::Succeeded => TrackedRunStatus::Succeeded,
        _ => TrackedRunStatus::Failed,
    };
    let mut run = TrackedRun {
        status,
        // Truncated to whole milliseconds.
        duration: Some(metadata.duration as u64),
        error: Some(metadata.error.clone())
            .filter(|error| !error.is_empty() && status == TrackedRunStatus::Failed),
        name: Some(information.name.clone()).filter(|name| !name.is_empty()),
        description: Some(information.description.clone()).filter(|text| !text.is_empty()),
        ..TrackedRun::default()
    };
    match input_format {
        ContentFormat::Json => {
            let file = run_dir::input_json_file(dir);
            if !exists_as(&file, Kind::File).await? {
                return Ok(None);
            }
            run.input_file = Some(file);
        }
        ContentFormat::MultiFile => {
            let inputs = run_dir::inputs_dir(dir);
            if !exists_as(&inputs, Kind::Dir).await? {
                return Ok(None);
            }
            run.input_dir_path = Some(inputs);
        }
        ContentFormat::Unknown => return Err(unknown_format(&information.id, "input")),
    }
    match output_format {
        ContentFormat::Json => {
            let file = run_dir::output_json_file(dir);
            if status == TrackedRunStatus::Succeeded && exists_as(&file, Kind::File).await? {
                run.output_file = Some(file);
            }
        }
        ContentFormat::MultiFile => {
            let solutions = run_dir::solutions_dir(dir);
            if exists_as(&solutions, Kind::Dir).await? {
                run.output_dir_path = Some(solutions);
            }
            run.metrics = metadata.metrics.clone();
            run.assets = read_assets(dir).await?;
        }
        ContentFormat::Unknown => return Err(unknown_format(&information.id, "output")),
    }
    Ok(Some(run))
}

/// What a path should be, for [`exists_as`].
#[derive(Clone, Copy)]
enum Kind {
    File,
    Dir,
}

/// Whether `path` is a `kind`, following links. A missing path is not.
async fn exists_as(path: &Path, kind: Kind) -> Result<bool> {
    match fs::metadata(path).await {
        Ok(meta) => Ok(match kind {
            Kind::File => meta.is_file(),
            Kind::Dir => meta.is_dir(),
        }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(Error::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

/// The assets of the run in `dir`, from `outputs/assets.json`, unwrapped
/// from `{"assets": …}`, if they are a list. A run without that file has
/// none.
///
/// # Errors
///
/// Returns [`Error::Io`] if the file cannot be read or is not JSON.
async fn read_assets(dir: &Path) -> Result<Option<Vec<Value>>> {
    let path = run_dir::assets_file(dir);
    let bytes = match fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(Error::Io { path, source }),
    };
    let assets = match serde_json::from_slice(&bytes) {
        Ok(Value::Object(mut assets)) => assets.remove("assets"),
        Ok(other) => Some(other),
        Err(error) => {
            let source = io::Error::new(io::ErrorKind::InvalidData, error);
            return Err(Error::Io { path, source });
        }
    };
    Ok(match assets {
        Some(Value::Array(assets)) => Some(assets),
        _ => None,
    })
}

/// Adds `synced_run` to `synced_runs` in the information file of the run
/// `run_id` in `dir`, atomically, and changes nothing else in it.
///
/// # Errors
///
/// Returns [`Error::Io`] if the file cannot be read or written, or does not
/// hold a run's information.
async fn record_synced_run(dir: &Path, run_id: &str, synced_run: &SyncedRun) -> Result<()> {
    let path = run_dir::information_file(dir, run_id);
    let not_information = |path: &Path| Error::Io {
        path: path.to_owned(),
        source: io::Error::new(
            io::ErrorKind::InvalidData,
            "not the information of a local run",
        ),
    };
    let bytes = fs::read(&path).await.map_err(|source| Error::Io {
        path: path.clone(),
        source,
    })?;
    let Ok(Value::Object(mut information)) = serde_json::from_slice::<Value>(&bytes) else {
        return Err(not_information(&path));
    };
    // A `SyncedRun` is strings only, so it always serializes.
    let record = serde_json::to_value(synced_run).unwrap_or_default();
    match information.get_mut("synced_runs") {
        Some(Value::Array(records)) => records.push(record),
        None | Some(Value::Null) => {
            information.insert("synced_runs".to_owned(), Value::Array(vec![record]));
        }
        Some(_) => return Err(not_information(&path)),
    }
    let bytes =
        serde_json::to_vec_pretty(&Value::Object(information)).map_err(|error| Error::Io {
            path: path.clone(),
            source: io::Error::new(io::ErrorKind::InvalidData, error),
        })?;
    write_atomically(&path, &bytes).await
}

/// The error for the run `run_id`, whose `what` (input or output) format
/// this SDK does not know.
fn unknown_format(run_id: &str, what: &str) -> Error {
    Error::Invalid {
        reason: format!("local run `{run_id}` has an {what} format this SDK does not know"),
    }
}

/// `src` as the registry stores it, which must be valid UTF-8.
async fn registrable(src: &Path) -> Result<PathBuf> {
    let src = registry::normalize(src).await?;
    if src.to_str().is_none() {
        return Err(Error::Invalid {
            reason: format!(
                "`{}` is not valid UTF-8, which the local registry needs",
                src.display()
            ),
        });
    }
    Ok(src)
}

/// Registers the workspace in `src`, a path from [`registrable`], in
/// `registry`, unless it is registered already, with the rules of
/// [`Local::register_workspace`]. Returns where its entry is, and whether
/// this registered it.
fn insert(
    registry: &mut Registry,
    src: &Path,
    workspace_id: Option<String>,
    description: Option<String>,
    content_format: ContentFormat,
) -> Result<(usize, bool)> {
    let workspace_id = workspace_id.filter(|id| !id.is_empty());
    if let Some(index) = position(registry, src) {
        let registered = &registry.workspaces[index].workspace.workspace_id;
        return match workspace_id {
            Some(id) if id != *registered => Err(Error::Invalid {
                reason: format!(
                    "`{}` is already registered as local workspace `{registered}`: to change \
                     its ID, update it with `new_workspace_id`",
                    src.display()
                ),
            }),
            _ => Ok((index, false)),
        };
    }
    let workspace_id = match workspace_id {
        Some(id) => {
            check_id_is_free(registry, &id, None)?;
            id
        }
        // A generated ID is all but certainly new, but it is checked like
        // any other.
        None => loop {
            let id = safe_id(ID_PREFIX);
            if check_id_is_free(registry, &id, None).is_ok() {
                break id;
            }
        },
    };
    let now = now_rfc3339();
    registry.workspaces.push(RegistryEntry::new(LocalWorkspace {
        workspace_id,
        src: src.to_owned(),
        content_format,
        created_at: now.clone(),
        updated_at: now,
        description,
    }));
    Ok((registry.workspaces.len() - 1, true))
}

/// The content format in the manifest of the workspace in `src`.
async fn content_format(src: &Path) -> Result<ContentFormat> {
    let params = ManifestValidateParams {
        workspace_dir: Some(src.to_owned()),
    };
    Ok(manifest::validate(params)
        .await?
        .configuration
        .content
        .format)
}

/// Where the workspace in `src` is in the registry, if it is registered.
fn position(registry: &Registry, src: &Path) -> Option<usize> {
    registry
        .workspaces
        .iter()
        .position(|entry| entry.workspace.src == src)
}

/// Checks that no registered workspace has the ID `id`, except the one at
/// `index`.
fn check_id_is_free(registry: &Registry, id: &str, index: Option<usize>) -> Result<()> {
    let taken = registry
        .workspaces
        .iter()
        .enumerate()
        .find(|(i, entry)| Some(*i) != index && entry.workspace.workspace_id == id);
    match taken {
        Some((_, other)) => Err(Error::Invalid {
            reason: format!(
                "local workspace ID `{id}` is already registered for `{}`",
                other.workspace.src.display()
            ),
        }),
        None => Ok(()),
    }
}

pub(crate) fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "decision_local_workspace_delete",
            "Removes a workspace from the local registry.",
            |cx, a: InLocalWorkspace<NoParams>| async move {
                cx.local()?.delete_workspace(&a.workspace).await
            },
        )
        .local_only()
        .destructive(),
        Tool::new(
            "decision_local_workspace_get",
            "Gets a workspace from the local registry.",
            |cx, a: InLocalWorkspace<NoParams>| async move {
                cx.local()?.get_workspace(&a.workspace).await
            },
        )
        .local_only()
        .read_only(),
        Tool::new(
            "decision_local_workspace_list",
            "Lists the workspaces in the local registry.",
            |cx, _: NoParams| async move { cx.local()?.list_workspaces().await },
        )
        .local_only()
        .read_only(),
        Tool::new(
            "decision_local_workspace_register",
            "Registers a workspace directory in the local registry.",
            |cx, a: WorkspaceRegisterParams| async move { cx.local()?.register_workspace(a).await },
        )
        .local_only(),
        Tool::new(
            "decision_local_workspace_registered",
            "Checks whether a workspace is in the local registry.",
            |cx, a: InLocalWorkspace<NoParams>| async move {
                cx.local()?.workspace_registered(&a.workspace).await
            },
        )
        .local_only()
        .read_only(),
        Tool::new(
            "decision_local_workspace_update",
            "Updates a workspace in the local registry.",
            |cx, a: InLocalWorkspace<WorkspaceUpdateParams>| async move {
                cx.local()?.update_workspace(&a.workspace, a.params).await
            },
        )
        .local_only()
        .destructive(),
        Tool::new(
            "decision_local_workspace_sync",
            "Uploads the local runs of a workspace to a platform workspace.",
            |cx, a: SyncRef<WorkspaceSyncParams>| async move {
                cx.local()?
                    .sync_workspace(
                        &a.workspace,
                        cx.platform()?,
                        &a.platform_workspace_id,
                        a.params,
                    )
                    .await
            },
        )
        .local_only(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::local::registry::tests::TempDir;

    /// A workspace directory `name` in `root` with `manifest` as its
    /// `workspace.yaml`.
    fn workspace(root: &TempDir, name: &str, manifest: &str) -> PathBuf {
        let dir = root.0.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("workspace.yaml"), manifest).unwrap();
        dir
    }

    #[tokio::test]
    async fn gets_or_registers_with_the_current_content_format() {
        let root = TempDir::new();
        let local = Local::with_home(root.0.join(".fico"));
        let src = workspace(&root, "routing", "files: [main.py]\n");
        let by_src = LocalWorkspaceRef {
            workspace_id: None,
            src: Some(src.clone()),
        };
        let (registered, created) = local.get_or_register_workspace(&by_src).await.unwrap();
        assert!(created);
        assert_eq!(registered.content_format, ContentFormat::Json);

        let multi_file = "files: [main.py]\nconfiguration:\n  content:\n    format: multi-file\n";
        std::fs::write(src.join("workspace.yaml"), multi_file).unwrap();
        let (found, created) = local.get_or_register_workspace(&by_src).await.unwrap();
        assert!(!created);
        assert_eq!(found.workspace_id, registered.workspace_id);
        assert_eq!(found.content_format, ContentFormat::MultiFile);

        // A manifest that is gone is an error, as in `get_workspace`.
        std::fs::remove_file(src.join("workspace.yaml")).unwrap();
        let err = local.get_or_register_workspace(&by_src).await.unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err}");

        // An ID alone has no directory to register.
        let by_id = LocalWorkspaceRef {
            workspace_id: Some("trucks".to_owned()),
            src: None,
        };
        let err = local.get_or_register_workspace(&by_id).await.unwrap_err();
        assert!(matches!(err, Error::NotFound { .. }), "{err}");
    }
}
