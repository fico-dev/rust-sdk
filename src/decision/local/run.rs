//! Local runs, in `<src>/.fico/runs/<run_id>/`. The directory layout is in
//! `run_dir.rs`.
//!
//! Every read of a run that is not final finalizes it first
//! (`executor::finalize`), so a run whose workspace exited while no executor
//! was there to finish it, such as one whose host stopped, still ends.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use tokio::fs;

use super::{InLocalWorkspace, Local, executor, registry, run_dir, visuals};
use crate::decision::clone::{clone_description, clone_name, cloned_options};
use crate::decision::compare::{check_run_ids, compare};
use crate::decision::id::safe_id;
use crate::decision::manifest::{self, ManifestValidateParams};
use crate::decision::polling::poll;
use crate::decision::time::now_rfc3339;
use crate::decision::{
    ContentFormat, ErrorLog, Format, FormatInput, FormatOutput, LocalWorkspaceRef, Manifest,
    Metadata, OptionSummary, PollingOptions, Run, RunComparison, RunCreated, RunInformation,
    RunLog, RunOptions, RunResult, RunStatus, RunTrackingMetadata, RunType, RunTypeConfiguration,
};
use crate::tool::{NoParams, Tool};
use crate::{Error, Result};

/// What [`Error::NotFound`] calls a local run.
const RUN_KIND: &str = "local run";

/// Tool arguments that address a local run, plus the operation's parameters.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct RunRef<P> {
    #[serde(flatten)]
    pub(crate) workspace: LocalWorkspaceRef,
    /// ID of the local run.
    pub(crate) run_id: String,
    #[serde(flatten)]
    pub(crate) params: P,
}

/// Parameters for [`Local::create_run`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RunCreateParams {
    /// Input of the run, as a JSON object, for a workspace with JSON input.
    /// Set exactly one of `input`, `input_file` and `input_dir`.
    pub input: Option<Value>,
    /// JSON file that holds the run's input, a JSON object, for a workspace
    /// with JSON input. Set exactly one of `input`, `input_file` and
    /// `input_dir`. For multi-file input, use `input_dir`.
    pub input_file: Option<PathBuf>,
    /// Directory whose files are the run's input, for a workspace with
    /// multi-file input. Set exactly one of `input`, `input_file` and
    /// `input_dir`. Links to files count as files; links to directories,
    /// broken links and special files are left out.
    pub input_dir: Option<PathBuf>,
    /// Format of the run's input: `json` for `input` and `input_file`,
    /// `multi-file` for `input_dir`. Defaults to the one in the workspace's
    /// manifest, which also sets the format of the output.
    pub content_format: Option<ContentFormat>,
    /// Name of the run. Defaults to `local run <run ID>`.
    pub name: Option<String>,
    /// Description of the run. Defaults to when the run was created.
    pub description: Option<String>,
    /// Options to run the workspace with, by name, e.g.
    /// `{"solve.duration": "10s"}`.
    pub options: BTreeMap<String, String>,
}

/// Parameters for [`Local::get_run`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RunGetParams {
    /// Polls until the run reaches a terminal state, then returns it. Without
    /// it, returns the run's current state.
    pub polling: Option<PollingOptions>,
    /// Directory to copy the run's output files into, for multi-file output.
    /// Files already there with the same names are overwritten. JSON output
    /// comes in the result instead.
    pub output_dir: Option<PathBuf>,
}

/// Parameters for [`Local::get_run_input`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RunInputGetParams {
    /// Directory to copy the run's input files into, for multi-file input.
    /// Files already there with the same names are overwritten.
    pub output_dir: Option<PathBuf>,
}

/// Parameters for [`Local::get_run_logs`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RunLogsGetParams {
    /// Polls until the run reaches a terminal state, then returns its logs.
    /// Without it, returns the logs so far.
    pub polling: Option<PollingOptions>,
}

/// Parameters for [`Local::list_runs`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RunListParams {
    /// Only runs with this status.
    pub status: Option<RunStatus>,
}

/// Parameters for [`Local::clone_run`]. Each one that is set replaces the
/// cloned run's.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RunCloneParams {
    /// Input of the run, as a JSON object, for a workspace with JSON input.
    /// Set at most one of `input`, `input_file` and `input_dir`; with none,
    /// the run gets the cloned run's input.
    pub input: Option<Value>,
    /// JSON file that holds the run's input, a JSON object, for a workspace
    /// with JSON input. Set at most one of `input`, `input_file` and
    /// `input_dir`. For multi-file input, use `input_dir`.
    pub input_file: Option<PathBuf>,
    /// Directory whose files are the run's input, for a workspace with
    /// multi-file input. Set at most one of `input`, `input_file` and
    /// `input_dir`. Links to files count as files; links to directories,
    /// broken links and special files are left out.
    pub input_dir: Option<PathBuf>,
    /// Format of the run's input: `json` for `input` and `input_file`,
    /// `multi-file` for `input_dir`. Defaults to the format of the input
    /// given, or, without one, to the cloned run's, which also sets the
    /// format of the output.
    pub content_format: Option<ContentFormat>,
    /// Name of the run, cut to 128 characters. Defaults to the cloned run's
    /// name, or its ID if it has none, followed by ` clone`.
    pub name: Option<String>,
    /// Description of the run, cut to 256 characters. Defaults to `Clone of
    /// ` and the cloned run's description, or its ID if it has none.
    pub description: Option<String>,
    /// Options to run the workspace with, by name, e.g.
    /// `{"solve.duration": "10s"}`. Defaults to the cloned run's options.
    pub options: BTreeMap<String, String>,
}

/// Parameters for [`Local::compare_runs`].
///
/// `run_ids` is required in tool arguments. The `Default` value leaves it
/// empty, which [`Local::compare_runs`] rejects, so set it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
pub struct RunCompareParams {
    /// IDs of the local runs to compare, from 1 to 20, each one once.
    pub run_ids: Vec<String>,
}

impl Local {
    /// Starts a local run of a workspace.
    ///
    /// Returns as soon as the run is created. To wait for it, call
    /// [`Local::get_run`] with `polling`.
    ///
    /// The input is checked first: exactly one of `params.input`,
    /// `params.input_file` and `params.input_dir` is set, a JSON input, given
    /// or read from the file, is an object and needs the `json` content
    /// format, and an input directory needs the `multi-file` one. Then a
    /// workspace that is not registered is registered, and the result's
    /// `workspace_registered` says so. The run starts out `queued`, with its
    /// input in its directory, for the executor to start it.
    ///
    /// A Python workspace runs with the `uv` set with [`Local::with_uv`],
    /// else `FICO_UV`, else the SDK's own: the first Python run in a FICO
    /// home downloads it (about 15 MB, [`crate::uv::install`]) before this
    /// returns.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if the input breaks those rules,
    /// [`Error::Io`] if the input file or directory cannot be read or the
    /// run's directory cannot be written, the errors of
    /// [`Local::register_workspace`] for a workspace it registers and those
    /// of [`Local::get_workspace`] for one it finds, [`Error::Io`] or
    /// [`Error::Command`] if the workspace cannot be started, and, for the
    /// first Python run, those of [`crate::uv::install`] if the SDK's `uv`
    /// can't be installed.
    pub async fn create_run(
        &self,
        workspace: &LocalWorkspaceRef,
        params: RunCreateParams,
    ) -> Result<RunCreated> {
        let input = new_input(&params).await?;
        let (_, manifest) = self.find_workspace(workspace).await?;
        let format = params
            .content_format
            .unwrap_or(manifest.configuration.content.format);
        let new_run = NewRun {
            input,
            format,
            name: params.name.as_deref().filter(|name| !name.is_empty()),
            description: params.description.as_deref(),
            options: &params.options,
            cloned_run_id: "",
        };
        self.start_run(workspace, &manifest, new_run).await
    }

    /// Gets a local run, including its output.
    ///
    /// With `params.polling`, waits until the run reaches a terminal state.
    /// Polling without `max_duration` or `max_tries` waits as long as the
    /// run does, and forever on a status this SDK does not know; drop the
    /// future to stop sooner.
    ///
    /// The result has the run's metrics, an `error_log` when the run
    /// recorded an error, and, for a run that succeeded with JSON output, its
    /// output. The output files of a run that succeeded with multi-file
    /// output are copied into `params.output_dir`, if it is set; the result
    /// has no `output` then, and its output format says so.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] if the run does not exist,
    /// [`Error::Invalid`] if `run_id` cannot be a local run ID or the run has
    /// an output format this SDK does not know, or if `params.output_dir` is
    /// inside the output, [`Error::Wait`] if polling runs out of time or
    /// tries, and [`Error::Io`] if the run's files cannot be read, hold
    /// invalid JSON or cannot be copied. Finding the workspace fails as
    /// [`Local::get_workspace`] does, except that a `src` needs no
    /// registration.
    pub async fn get_run(
        &self,
        workspace: &LocalWorkspaceRef,
        run_id: &str,
        params: RunGetParams,
    ) -> Result<RunResult> {
        let (src, dir) = self.find_run(workspace, run_id).await?;
        let polling = params.polling.as_ref();
        let mut information = wait_for(&src, &dir, run_id, polling).await?;
        attach_metrics(&dir, &mut information).await?;
        let error = &information.metadata.error;
        let error_log = (!error.is_empty()).then(|| ErrorLog {
            error: Some(error.clone()),
            stdout: None,
            stderr: None,
        });
        let output = match information.metadata.status {
            RunStatus::Succeeded => {
                read_output(&dir, &information, set(params.output_dir.as_deref())).await?
            }
            _ => None,
        };
        Ok(RunResult {
            information,
            error_log,
            output,
            ensemble: None,
        })
    }

    /// Gets a local run's information and metadata, without its output.
    ///
    /// The metadata has the run's metrics, unless they are not a JSON
    /// object.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] if the run does not exist,
    /// [`Error::Invalid`] if `run_id` cannot be a local run ID, and
    /// [`Error::Io`] if the run's files cannot be read or hold invalid JSON.
    /// Finding the workspace fails as [`Local::get_workspace`] does, except
    /// that a `src` needs no registration.
    pub async fn get_run_information(
        &self,
        workspace: &LocalWorkspaceRef,
        run_id: &str,
    ) -> Result<RunInformation> {
        let (src, dir) = self.find_run(workspace, run_id).await?;
        let mut information = read_run(&src, &dir, run_id).await?;
        attach_metrics(&dir, &mut information).await?;
        Ok(information)
    }

    /// Gets the input of a local run.
    ///
    /// Returns `None` for multi-file input: its files are copied into
    /// `params.output_dir` instead. JSON input that was never written is
    /// `None` too.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] if the run does not exist,
    /// [`Error::Invalid`] if `run_id` cannot be a local run ID, if the input
    /// is multi-file and `params.output_dir` is not set, or if the run has an
    /// input format this SDK does not know, and [`Error::Io`] if the run's
    /// files cannot be read or copied. Finding the workspace fails as
    /// [`Local::get_workspace`] does, except that a `src` needs no
    /// registration.
    pub async fn get_run_input(
        &self,
        workspace: &LocalWorkspaceRef,
        run_id: &str,
        params: RunInputGetParams,
    ) -> Result<Option<Value>> {
        let (src, dir) = self.find_run(workspace, run_id).await?;
        let information = read_run(&src, &dir, run_id).await?;
        match information.metadata.format.format_input.input_type {
            ContentFormat::Json => read_json(&run_dir::input_json_file(&dir)).await,
            ContentFormat::MultiFile => {
                let output_dir =
                    set(params.output_dir.as_deref()).ok_or_else(|| Error::Invalid {
                        reason: format!(
                            "the input of local run `{run_id}` is multi-file: set `output_dir` to \
                         copy its files into"
                        ),
                    })?;
                copy_dir(&run_dir::inputs_dir(&dir), output_dir).await?;
                Ok(None)
            }
            ContentFormat::Unknown => Err(unknown_format(run_id, "input")),
        }
    }

    /// Gets the logs of a local run.
    ///
    /// With `params.polling`, waits until the run reaches a terminal state,
    /// with the same limits as [`Local::get_run`]. A run that has written no
    /// logs has empty logs.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] if the run does not exist,
    /// [`Error::Invalid`] if `run_id` cannot be a local run ID,
    /// [`Error::Wait`] if polling runs out of time or tries, and
    /// [`Error::Io`] if the run's files cannot be read. Finding the workspace
    /// fails as [`Local::get_workspace`] does, except that a `src` needs no
    /// registration.
    pub async fn get_run_logs(
        &self,
        workspace: &LocalWorkspaceRef,
        run_id: &str,
        params: RunLogsGetParams,
    ) -> Result<RunLog> {
        let (src, dir) = self.find_run(workspace, run_id).await?;
        wait_for(&src, &dir, run_id, params.polling.as_ref()).await?;
        let path = run_dir::logs_file(&dir);
        let log = match fs::read(&path).await {
            // Workspaces may write anything to stderr.
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(source) if source.kind() == io::ErrorKind::NotFound => String::new(),
            Err(source) => return Err(Error::Io { path, source }),
        };
        Ok(RunLog { log })
    }

    /// Lists the local runs of a workspace.
    ///
    /// The newest runs come first. A workspace without runs has none, and
    /// directories without a readable run are skipped. A run that cannot be
    /// finalized is listed as its files have it. Each run has no metrics or
    /// options, and an empty `run_type`, `execution_class` and `runtime`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the runs directory cannot be read. Finding
    /// the workspace fails as [`Local::get_workspace`] does, except that a
    /// `src` needs no registration.
    pub async fn list_runs(
        &self,
        workspace: &LocalWorkspaceRef,
        params: RunListParams,
    ) -> Result<Vec<Run>> {
        let (src, _) = self.find_workspace(workspace).await?;
        let runs_dir = run_dir::runs_dir(&src);
        let mut entries = match fs::read_dir(&runs_dir).await {
            Ok(entries) => entries,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(io_error(&runs_dir, source)),
        };
        let mut runs = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|source| io_error(&runs_dir, source))?
        {
            // Files, directories whose name is no run ID, and runs whose
            // information cannot be read are not runs.
            let Ok(run_id) = entry.file_name().into_string() else {
                continue;
            };
            let Ok(dir) = run_dir::run_dir(&src, &run_id) else {
                continue;
            };
            let information = match read_run(&src, &dir, &run_id).await {
                Ok(information) => information,
                // A run that cannot be finalized is still a run: it is
                // listed as it is, and a get reports why.
                Err(_) => match read_information(&dir, &run_id).await {
                    Ok(information) => information,
                    Err(_) => continue,
                },
            };
            if params
                .status
                .is_none_or(|status| status == information.metadata.status)
            {
                runs.push(to_run(information));
            }
        }
        // The timestamps all have microseconds, so they sort as strings.
        runs.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(runs)
    }

    /// Starts a local run with the input and configuration of an existing one.
    ///
    /// What `params` sets replaces the cloned run's: the input, its format,
    /// the name, the description and the options; an empty name,
    /// description or set of options counts as unset. Without an input, the
    /// run gets the cloned run's, from its directory. The new run records
    /// the cloned run's ID in `metadata.tracking.cloned_run_id`. Returns as
    /// soon as the run is created, as [`Local::create_run`] does, and
    /// registers the workspace the same way.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] if the cloned run does not exist, and
    /// [`Error::Invalid`] if `run_id` cannot be a local run ID, if more than
    /// one input is set, if the input does not fit the run's format as in
    /// [`Local::create_run`], or if the cloned run has no input to reuse.
    /// Otherwise it fails as [`Local::create_run`] does.
    pub async fn clone_run(
        &self,
        workspace: &LocalWorkspaceRef,
        run_id: &str,
        params: RunCloneParams,
    ) -> Result<RunCreated> {
        let (src, manifest) = self.find_workspace(workspace).await?;
        let dir = run_dir::run_dir(&src, run_id)?;
        let cloned = read_run(&src, &dir, run_id).await?;
        let given = given_input(
            params.input.as_ref(),
            params.input_file.as_deref(),
            params.input_dir.as_deref(),
        );
        let cloned_format = cloned.metadata.format.format_input.input_type;
        let (input, format) = match given.await? {
            Some(input) => {
                let format = match input {
                    Input::Json(_) => ContentFormat::Json,
                    Input::Dir(_) => ContentFormat::MultiFile,
                };
                (input, format)
            }
            None => (cloned_input(&dir, &cloned).await?, cloned_format),
        };
        let options = match params.options.is_empty() {
            true => cloned_options(&cloned),
            false => params.options,
        };
        let new_run = NewRun {
            input,
            format: params.content_format.unwrap_or(format),
            name: Some(&clone_name(params.name.as_deref(), &cloned)),
            description: Some(&clone_description(params.description.as_deref(), &cloned)),
            options: &options,
            cloned_run_id: run_id,
        };
        self.start_run(workspace, &manifest, new_run).await
    }

    /// Compares local runs field by field.
    ///
    /// It compares the runs' information fields but `metadata` and
    /// `console_url`; their metadata fields but metrics and options, with
    /// the format as `content_format`, the integration's and the tracking's
    /// fields on their own, the run type as its type and `status_v2` as
    /// `status`; their metrics, with decimals rounded to 5 digits; and
    /// their options. Field names are the API's, and each field has the
    /// value of every run that has it, by run ID. `has_differences` says
    /// whether those values are not all equal as JSON, numbers compared by
    /// value. `metrics` and `options` are unset when no run has any.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `params.run_ids` has fewer than 1 or
    /// more than 20 runs, the same run twice, or an ID that cannot be a
    /// local run ID, [`Error::NotFound`] if a run does not exist, and
    /// [`Error::Io`] if a run's files cannot be read or hold invalid JSON.
    /// Finding the workspace fails as [`Local::get_workspace`] does, except
    /// that a `src` needs no registration.
    pub async fn compare_runs(
        &self,
        workspace: &LocalWorkspaceRef,
        params: RunCompareParams,
    ) -> Result<RunComparison> {
        let run_ids = params.run_ids;
        check_run_ids(&run_ids, RUN_KIND)?;
        let (src, _) = self.find_workspace(workspace).await?;
        let mut runs = Vec::with_capacity(run_ids.len());
        for run_id in &run_ids {
            let dir = run_dir::run_dir(&src, run_id)?;
            let mut information = read_run(&src, &dir, run_id).await?;
            attach_metrics(&dir, &mut information).await?;
            runs.push(information);
        }
        Ok(compare(run_ids, &runs))
    }

    /// Gets the HTML files that show a local run's visuals.
    ///
    /// Opening them is up to the caller, e.g. the CLI opens a browser. The
    /// paths are absolute, in the run's `visuals/` directory, one per Plotly
    /// or GeoJSON visual in the run's assets, in their order. Finalizing a
    /// run renders its visuals; this renders them again, the same way, so
    /// that a rendering error comes back here. A run that has not ended has
    /// no visuals yet.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] if the run does not exist,
    /// [`Error::Invalid`] if `run_id` cannot be a local run ID, and
    /// [`Error::Io`] if the run's files cannot be read, its assets are not
    /// JSON, or the visuals cannot be written. Finding the workspace fails
    /// as [`Local::get_workspace`] does, except that a `src` needs no
    /// registration.
    pub async fn get_run_visuals(
        &self,
        workspace: &LocalWorkspaceRef,
        run_id: &str,
    ) -> Result<Vec<PathBuf>> {
        let (src, dir) = self.find_run(workspace, run_id).await?;
        let information = read_run(&src, &dir, run_id).await?;
        if !information.metadata.status.is_terminal() {
            return Ok(Vec::new());
        }
        visuals::render(&src, run_id).await
    }

    /// Starts `new_run` in the workspace that `workspace` identifies, whose
    /// manifest is `manifest`: checks its input against its format, gets or
    /// registers the workspace, writes the run's directory and hands the run
    /// to the executor.
    async fn start_run(
        &self,
        workspace: &LocalWorkspaceRef,
        manifest: &Manifest,
        new_run: NewRun<'_>,
    ) -> Result<RunCreated> {
        check_format(&new_run.input, new_run.format)?;
        // Only valid runs register their workspace.
        let (entry, workspace_registered) = self.get_or_register_workspace(workspace).await?;
        let information = new_information(
            safe_id(run_dir::RUN_ID_PREFIX),
            now_rfc3339(),
            entry.workspace_id,
            &new_run,
        );
        let run_id = information.id.clone();
        prepare(&entry.src, information, &new_run.input).await?;
        executor::start(self, &entry.src, &run_id, manifest, new_run.options).await?;
        Ok(RunCreated {
            run_id,
            workspace_registered,
        })
    }

    /// The directory and the manifest of the workspace that `workspace`
    /// identifies, without registering it. The directory is its `src`, made
    /// absolute as the registry stores it, or, without one, the directory of
    /// the registered workspace with its ID. A directory without a valid
    /// manifest fails here.
    async fn find_workspace(&self, workspace: &LocalWorkspaceRef) -> Result<(PathBuf, Manifest)> {
        let src = match workspace.src.as_deref() {
            Some(src) if !src.as_os_str().is_empty() => registry::normalize(src).await?,
            _ => registry::resolve(&self.home, workspace).await?.src,
        };
        let params = ManifestValidateParams {
            workspace_dir: Some(src.clone()),
        };
        let manifest = manifest::validate(params).await?;
        Ok((src, manifest))
    }

    /// The directory of the workspace that `workspace` identifies, and the
    /// directory of its local run `run_id`.
    pub(crate) async fn find_run(
        &self,
        workspace: &LocalWorkspaceRef,
        run_id: &str,
    ) -> Result<(PathBuf, PathBuf)> {
        let (src, _) = self.find_workspace(workspace).await?;
        let dir = run_dir::run_dir(&src, run_id)?;
        Ok((src, dir))
    }
}

/// A run to start, from [`Local::create_run`] or [`Local::clone_run`].
#[derive(Debug)]
struct NewRun<'a> {
    /// Its input.
    input: Input<'a>,
    /// The format of its input and output.
    format: ContentFormat,
    /// Its name, if it is not the default one.
    name: Option<&'a str>,
    /// Its description, if it is not the default one.
    description: Option<&'a str>,
    /// Its options.
    options: &'a BTreeMap<String, String>,
    /// The ID of the run it is a clone of, or empty.
    cloned_run_id: &'a str,
}

/// The input of a new run.
#[derive(Debug, Clone, PartialEq)]
enum Input<'a> {
    /// A JSON object, for JSON input.
    Json(Cow<'a, Map<String, Value>>),
    /// A directory of files, for multi-file input.
    Dir(Cow<'a, Path>),
}

/// The input of a new run that `params` sets, which must be exactly one.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if `params` sets no input, and the errors of
/// [`given_input`].
async fn new_input(params: &RunCreateParams) -> Result<Input<'_>> {
    let given = given_input(
        params.input.as_ref(),
        params.input_file.as_deref(),
        params.input_dir.as_deref(),
    );
    given.await?.ok_or_else(|| invalid(EXACTLY_ONE_INPUT))
}

/// What [`Error::Invalid`] says when a run gets more than one input, or
/// none.
const EXACTLY_ONE_INPUT: &str = "set exactly one of `input`, `input_file` and `input_dir`";

/// The input that a run's parameters set, `input`, `input_file` or
/// `input_dir`, or `None` if they set none. An empty path counts as unset.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if more than one is set, if `input` or what
/// `input_file` holds is not a JSON object, or if `input_dir` is not a
/// directory, and [`Error::Io`] if `input_file` or `input_dir` cannot be
/// read.
async fn given_input<'a>(
    input: Option<&'a Value>,
    input_file: Option<&'a Path>,
    input_dir: Option<&'a Path>,
) -> Result<Option<Input<'a>>> {
    let (input_file, input_dir) = (set(input_file), set(input_dir));
    let given = [input.is_some(), input_file.is_some(), input_dir.is_some()];
    if given.into_iter().filter(|&given| given).count() > 1 {
        return Err(invalid(EXACTLY_ONE_INPUT));
    }
    if let Some(input) = input {
        return match input {
            Value::Object(input) => Ok(Some(Input::Json(Cow::Borrowed(input)))),
            _ => Err(invalid("`input` must be a JSON object")),
        };
    }
    if let Some(file) = input_file {
        return read_input_file(file)
            .await
            .map(|input| Some(Input::Json(input)));
    }
    let Some(dir) = input_dir else {
        return Ok(None);
    };
    let meta = fs::metadata(dir)
        .await
        .map_err(|source| io_error(dir, source))?;
    if !meta.is_dir() {
        return Err(invalid(format!(
            "`input_dir` must be a directory, and `{}` is not one",
            dir.display()
        )));
    }
    Ok(Some(Input::Dir(Cow::Borrowed(dir))))
}

/// The JSON object in the input file at `path`.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if the file does not hold a JSON object, such
/// as a tar.gz of input files, and [`Error::Io`] if it cannot be read.
async fn read_input_file(path: &Path) -> Result<Cow<'static, Map<String, Value>>> {
    let bytes = fs::read(path)
        .await
        .map_err(|source| io_error(path, source))?;
    match serde_json::from_slice(&bytes) {
        Ok(Value::Object(input)) => Ok(Cow::Owned(input)),
        Ok(_) => Err(invalid(format!(
            "`input_file` must hold a JSON object, and `{}` does not",
            path.display()
        ))),
        Err(error) => Err(invalid(format!(
            "`input_file` must be a JSON file, and `{}` is not one ({error}): for multi-file \
             input, set `input_dir`",
            path.display()
        ))),
    }
}

/// The input of the run `cloned`, in `dir`, for a clone of it: its JSON
/// input, or the directory of its input files.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if the run has no JSON input or a format this
/// SDK does not know, and [`Error::Io`] if its input cannot be read.
async fn cloned_input(dir: &Path, cloned: &RunInformation) -> Result<Input<'static>> {
    match cloned.metadata.format.format_input.input_type {
        ContentFormat::Json => match read_json(&run_dir::input_json_file(dir)).await? {
            Some(input) => Ok(Input::Json(Cow::Owned(input))),
            None => Err(invalid(format!(
                "local run `{}` has no input to clone: set `input` or `input_file`",
                cloned.id
            ))),
        },
        ContentFormat::MultiFile => Ok(Input::Dir(Cow::Owned(run_dir::inputs_dir(dir)))),
        ContentFormat::Unknown => Err(unknown_format(&cloned.id, "input")),
    }
}

/// Checks that `input` fits the run's content `format`.
fn check_format(input: &Input<'_>, format: ContentFormat) -> Result<()> {
    match (input, format) {
        (Input::Json(_), ContentFormat::Json) | (Input::Dir(_), ContentFormat::MultiFile) => Ok(()),
        (_, ContentFormat::Unknown) => Err(invalid("the run's content format is not known")),
        (Input::Json(_), ContentFormat::MultiFile) => Err(invalid(
            "a JSON input needs the `json` content format, and the run's is `multi-file`: set \
             `input_dir` instead",
        )),
        (Input::Dir(_), ContentFormat::Json) => Err(invalid(
            "`input_dir` needs the `multi-file` content format, and the run's is `json`: set \
             `input` or `input_file` instead, or `content_format`",
        )),
    }
}

/// Writes the directory of the new local run `information` in the
/// workspace in `src`: `.fico/.gitignore` if it is missing, the run's
/// `input`, and its information file, with the input's size. If any of it
/// fails, the run's directory is removed.
///
/// # Errors
///
/// Returns [`Error::Io`] if a file or directory cannot be read or written,
/// and [`Error::Invalid`] if the run ID cannot be one or the input directory
/// holds the run's directory.
async fn prepare(src: &Path, information: RunInformation, input: &Input<'_>) -> Result<()> {
    let runs_dir = run_dir::runs_dir(src);
    fs::create_dir_all(&runs_dir)
        .await
        .map_err(|source| io_error(&runs_dir, source))?;
    let gitignore = run_dir::gitignore_file(src);
    let exists = fs::try_exists(&gitignore)
        .await
        .map_err(|source| io_error(&gitignore, source))?;
    if !exists {
        fs::write(&gitignore, run_dir::GITIGNORE)
            .await
            .map_err(|source| io_error(&gitignore, source))?;
    }
    let dir = run_dir::run_dir(src, &information.id)?;
    // Never `create_dir_all`: an existing directory belongs to another run.
    fs::create_dir(&dir)
        .await
        .map_err(|source| io_error(&dir, source))?;
    let written = write_run(&dir, information, input).await;
    if written.is_err() {
        // The first error is the one to report.
        let _ = fs::remove_dir_all(&dir).await;
    }
    written
}

/// Writes `input` and `information`, with the input's size, into the run's
/// directory, `dir`.
async fn write_run(dir: &Path, mut information: RunInformation, input: &Input<'_>) -> Result<()> {
    let inputs = run_dir::inputs_dir(dir);
    fs::create_dir(&inputs)
        .await
        .map_err(|source| io_error(&inputs, source))?;
    let input_size = match input {
        Input::Json(input) => write_json(&run_dir::input_json_file(dir), input).await?,
        Input::Dir(from) => copy_dir(from, &inputs).await?,
    };
    information.metadata.input_size = input_size as f64;
    let path = run_dir::information_file(dir, &information.id);
    write_json(&path, &information).await?;
    Ok(())
}

/// The information of the local run `new_run`, with the ID `run_id`,
/// created at `created_at` in the workspace `workspace_id`: status
/// `queued`, and an input size of 0 until the input is written.
fn new_information(
    run_id: String,
    created_at: String,
    workspace_id: String,
    new_run: &NewRun<'_>,
) -> RunInformation {
    let format = new_run.format;
    let options: Map<String, Value> = new_run
        .options
        .iter()
        .map(|(name, value)| (name.clone(), Value::from(value.clone())))
        .collect();
    let options_summary = new_run
        .options
        .iter()
        .map(|(name, value)| OptionSummary {
            name: name.clone(),
            source: "run".to_owned(),
            value: value.clone(),
        })
        .collect();
    let metadata = Metadata {
        workspace_id,
        environment_id: String::new(),
        build_id: String::new(),
        created_at: created_at.clone(),
        duration: 0.0,
        error: String::new(),
        execution_class: Some("local".to_owned()),
        execution_duration: Some(0.0),
        experiment_id: Some(String::new()),
        experiment_type: Some(String::new()),
        format: Format {
            format_input: FormatInput { input_type: format },
            format_output: Some(FormatOutput {
                output_type: format,
            }),
        },
        initiated_at: Some(created_at.clone()),
        input_size: 0.0,
        integration: None,
        metrics: None,
        options: Some(RunOptions {
            active_options: Some(options.clone()),
            options_summary: Some(options_summary),
            request_options: Some(options),
        }),
        output_size: 0.0,
        queuing_disabled: Some(true),
        queuing_priority: Some(0),
        run_type: Some(RunTypeConfiguration {
            run_type: Some(RunType::Standard),
            definition_id: Some(String::new()),
            reference_id: Some(String::new()),
        }),
        runtime: Some("local".to_owned()),
        secrets_collection_id: Some(String::new()),
        status: RunStatus::Queued,
        tracking: Some(RunTrackingMetadata {
            cloned_run_id: Some(new_run.cloned_run_id.to_owned()),
            input_id: Some(String::new()),
        }),
    };
    let name = match new_run.name {
        Some(name) => name.to_owned(),
        None => format!("local run {run_id}"),
    };
    let description = match new_run.description {
        Some(description) => description.to_owned(),
        None => format!("Local run created at {created_at}"),
    };
    RunInformation {
        description,
        id: run_id,
        metadata,
        name,
        user_email: String::new(),
        console_url: String::new(),
        synced_runs: None,
    }
}

/// Writes `value` to the file at `path` as indented JSON, and returns the
/// number of bytes written.
async fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<u64> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| invalid_data(path, error))?;
    fs::write(path, &bytes)
        .await
        .map_err(|source| io_error(path, source))?;
    Ok(bytes.len() as u64)
}

/// The information of the run `run_id` in `dir`, of the workspace in
/// `src`, as [`read_run`] reads it: once without `polling`, and with it,
/// again until the run reaches a terminal state. The options are only read.
async fn wait_for(
    src: &Path,
    dir: &Path,
    run_id: &str,
    polling: Option<&PollingOptions>,
) -> Result<RunInformation> {
    let Some(options) = polling else {
        return read_run(src, dir, run_id).await;
    };
    poll(options, || async move {
        let information = read_run(src, dir, run_id).await?;
        let done = information.metadata.status.is_terminal();
        Ok(done.then_some(information))
    })
    .await
}

/// The information of the run `run_id` in `dir`, of the workspace in `src`,
/// finalized first if it is not final: a run whose workspace has exited
/// gets its status, metrics, assets and visuals, and the others are left as
/// they are.
///
/// # Errors
///
/// Returns the errors of [`read_information`], and those of
/// [`executor::finalize`] for a run that is not final.
async fn read_run(src: &Path, dir: &Path, run_id: &str) -> Result<RunInformation> {
    let information = read_information(dir, run_id).await?;
    if information.metadata.status.is_terminal() {
        return Ok(information);
    }
    executor::finalize(src, run_id).await?;
    read_information(dir, run_id).await
}

/// The information file of the run `run_id` in `dir`.
///
/// # Errors
///
/// Returns [`Error::NotFound`] if the run has no information file, and
/// [`Error::Io`] if it cannot be read or is not a run's information.
async fn read_information(dir: &Path, run_id: &str) -> Result<RunInformation> {
    let path = run_dir::information_file(dir, run_id);
    read_json(&path).await?.ok_or_else(|| Error::NotFound {
        kind: RUN_KIND,
        id: run_id.to_owned(),
    })
}

/// Puts the metrics of the run in `dir`, from `outputs/metrics.json`, into
/// `information`, unwrapped from `{"metrics": …}`.
///
/// A run without that file keeps the metrics it has, and so does a run
/// whose metrics are not a JSON object, which `Metadata` cannot hold: for
/// example the `{"metrics": null}` that executors write for a workspace
/// that printed `"metrics": null`. Failing instead would hide the run's
/// status and output.
///
/// # Errors
///
/// Returns [`Error::Io`] if the file cannot be read or is not JSON.
async fn attach_metrics(dir: &Path, information: &mut RunInformation) -> Result<()> {
    let path = run_dir::metrics_file(dir);
    let metrics = match read_json::<Value>(&path).await? {
        Some(Value::Object(mut metrics)) => match metrics.remove("metrics") {
            Some(inner) => inner,
            None => Value::Object(metrics),
        },
        Some(other) => other,
        None => return Ok(()),
    };
    if let Value::Object(metrics) = metrics {
        information.metadata.metrics = Some(metrics);
    }
    Ok(())
}

/// The run that a run list has for `information`.
fn to_run(information: RunInformation) -> Run {
    let metadata = information.metadata;
    Run {
        id: information.id,
        user_email: information.user_email,
        name: information.name,
        description: information.description,
        created_at: metadata.created_at,
        workspace_id: metadata.workspace_id,
        environment_id: metadata.environment_id,
        build_id: metadata.build_id,
        run_type: RunTypeConfiguration::default(),
        execution_class: String::new(),
        runtime: String::new(),
        status: metadata.status,
        queuing_priority: None,
        queuing_disabled: None,
        experiment_id: None,
        metrics: None,
        input_id: None,
        option_set: None,
        options: None,
        request_options: None,
        options_summary: None,
        switchback_experiment: None,
        scenario_id: None,
        repetition: None,
        input_set_id: None,
    }
}

/// The output of the succeeded run in `dir`: its JSON output, or `None` for
/// multi-file output, whose files are copied into `output_dir` if it is set,
/// and for a run that wrote no JSON output.
async fn read_output(
    dir: &Path,
    information: &RunInformation,
    output_dir: Option<&Path>,
) -> Result<Option<Map<String, Value>>> {
    let format = &information.metadata.format;
    // Without an output format, the output has the input's, which is the
    // one a new run gets.
    let output_type = format
        .format_output
        .as_ref()
        .map_or(format.format_input.input_type, |output| output.output_type);
    match output_type {
        ContentFormat::Json => read_json(&run_dir::output_json_file(dir)).await,
        ContentFormat::MultiFile => {
            if let Some(output_dir) = output_dir {
                copy_dir(&run_dir::solutions_dir(dir), output_dir).await?;
            }
            Ok(None)
        }
        ContentFormat::Unknown => Err(unknown_format(&information.id, "output")),
    }
}

/// The JSON value in the file at `path`, or `None` if there is no such file.
///
/// # Errors
///
/// Returns [`Error::Io`] if the file cannot be read or does not hold a `T`.
async fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let bytes = match fs::read(path).await {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error(path, source)),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| invalid_data(path, error))
}

/// Copies the files under `from` into `to`, creating `to` and its
/// subdirectories, and replacing files with the same names. A missing
/// `from` copies nothing.
///
/// Links to files are copied as files; links to directories, broken links
/// and special files are skipped, as the archive does, so links can't make
/// it loop. Returns the number of bytes copied.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if `to` is `from` or inside it, and
/// [`Error::Io`] if a file or directory cannot be read or written.
async fn copy_dir(from: &Path, to: &Path) -> Result<u64> {
    let from = match fs::canonicalize(from).await {
        Ok(from) => from,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(source) => return Err(io_error(from, source)),
    };
    // Checked before `to` exists, so a refused copy creates nothing. The
    // registry's normalization resolves the links in the part of `to` that
    // exists.
    if registry::normalize(to).await?.starts_with(&from) {
        return Err(Error::Invalid {
            reason: format!(
                "cannot copy `{}` into `{}`, which is inside it",
                from.display(),
                to.display()
            ),
        });
    }
    fs::create_dir_all(to)
        .await
        .map_err(|source| io_error(to, source))?;
    let mut copied = 0;
    let mut dirs = vec![PathBuf::new()];
    while let Some(relative) = dirs.pop() {
        let source_dir = from.join(&relative);
        fs::create_dir_all(to.join(&relative))
            .await
            .map_err(|source| io_error(&to.join(&relative), source))?;
        let mut entries = fs::read_dir(&source_dir)
            .await
            .map_err(|source| io_error(&source_dir, source))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|source| io_error(&source_dir, source))?
        {
            let path = entry.path();
            let name = relative.join(entry.file_name());
            let file_type = entry
                .file_type()
                .await
                .map_err(|source| io_error(&path, source))?;
            if file_type.is_dir() {
                dirs.push(name);
            } else if file_type.is_file()
                || fs::metadata(&path).await.is_ok_and(|meta| meta.is_file())
            {
                let target = to.join(&name);
                copied += fs::copy(&path, &target)
                    .await
                    .map_err(|source| io_error(&target, source))?;
            }
        }
    }
    Ok(copied)
}

/// `path`, unless it is unset or empty, which is the same for a path
/// parameter.
fn set(path: Option<&Path>) -> Option<&Path> {
    path.filter(|path| !path.as_os_str().is_empty())
}

/// An [`Error::Invalid`] for `reason`.
fn invalid(reason: impl Into<String>) -> Error {
    Error::Invalid {
        reason: reason.into(),
    }
}

/// The error for the run `run_id`, whose `what` (input or output) format
/// this SDK does not know.
fn unknown_format(run_id: &str, what: &str) -> Error {
    Error::Invalid {
        reason: format!("local run `{run_id}` has an {what} format this SDK does not know"),
    }
}

/// An [`Error::Io`] for a file at `path` that does not hold what it should.
fn invalid_data(path: &Path, error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Error {
    io_error(path, io::Error::new(io::ErrorKind::InvalidData, error))
}

/// An [`Error::Io`] on `path`.
fn io_error(path: &Path, source: io::Error) -> Error {
    Error::Io {
        path: path.to_owned(),
        source,
    }
}

pub(crate) fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "decision_local_run_create",
            "Starts a local run of a workspace.",
            |cx, a: InLocalWorkspace<RunCreateParams>| async move {
                cx.local()?.create_run(&a.workspace, a.params).await
            },
        )
        .local_only(),
        Tool::new(
            "decision_local_run_get",
            "Gets a local run, including its output.",
            |cx, a: RunRef<RunGetParams>| async move {
                cx.local()?.get_run(&a.workspace, &a.run_id, a.params).await
            },
        )
        .local_only()
        .destructive(),
        Tool::new(
            "decision_local_run_information_get",
            "Gets a local run's information and metadata, without its output.",
            |cx, a: RunRef<NoParams>| async move {
                cx.local()?
                    .get_run_information(&a.workspace, &a.run_id)
                    .await
            },
        )
        .local_only()
        .read_only(),
        Tool::new(
            "decision_local_run_input_get",
            "Gets the input of a local run.",
            |cx, a: RunRef<RunInputGetParams>| async move {
                cx.local()?
                    .get_run_input(&a.workspace, &a.run_id, a.params)
                    .await
            },
        )
        .local_only()
        .destructive(),
        Tool::new(
            "decision_local_run_logs_get",
            "Gets the logs of a local run.",
            |cx, a: RunRef<RunLogsGetParams>| async move {
                cx.local()?
                    .get_run_logs(&a.workspace, &a.run_id, a.params)
                    .await
            },
        )
        .local_only()
        .read_only(),
        Tool::new(
            "decision_local_run_list",
            "Lists the local runs of a workspace.",
            |cx, a: InLocalWorkspace<RunListParams>| async move {
                cx.local()?.list_runs(&a.workspace, a.params).await
            },
        )
        .local_only()
        .read_only(),
        Tool::new(
            "decision_local_run_clone",
            "Starts a local run with the input and configuration of an existing one.",
            |cx, a: RunRef<RunCloneParams>| async move {
                cx.local()?
                    .clone_run(&a.workspace, &a.run_id, a.params)
                    .await
            },
        )
        .local_only(),
        Tool::new(
            "decision_local_run_compare",
            "Compares local runs field by field.",
            |cx, a: InLocalWorkspace<RunCompareParams>| async move {
                cx.local()?.compare_runs(&a.workspace, a.params).await
            },
        )
        .local_only()
        .read_only(),
        Tool::new(
            "decision_local_run_visuals_get",
            "Gets the HTML files that show a local run's visuals.",
            |cx, a: RunRef<NoParams>| async move {
                cx.local()?.get_run_visuals(&a.workspace, &a.run_id).await
            },
        )
        .local_only()
        .read_only(),
    ]
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;

    /// A new empty directory, removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> TempDir {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let name = format!(
                "fico-sdk-run-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let dir = std::env::temp_dir().join(name);
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir.canonicalize().unwrap())
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const RUN_ID: &str = "local-3k9x0q2a";
    const CREATED_AT: &str = "2026-10-13T15:00:00.123456Z";

    /// The information of a new run with `format` input and `params`.
    fn information(format: ContentFormat, params: &RunCreateParams) -> RunInformation {
        let (run_id, created_at) = (RUN_ID.to_owned(), CREATED_AT.to_owned());
        let input = Map::new();
        let new_run = NewRun {
            input: Input::Json(Cow::Borrowed(&input)),
            format,
            name: params.name.as_deref().filter(|name| !name.is_empty()),
            description: params.description.as_deref(),
            options: &params.options,
            cloned_run_id: "",
        };
        new_information(run_id, created_at, "routing".to_owned(), &new_run)
    }

    fn read(path: &Path) -> Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[tokio::test]
    async fn prepares_a_run_with_json_input() {
        let src = TempDir::new();
        let input = json!({ "vehicles": [{ "id": "v1" }] });
        let input = input.as_object().unwrap();
        let params = RunCreateParams {
            options: [("duration", "10s"), ("solve.iterations", "50")]
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .into(),
            ..RunCreateParams::default()
        };
        let information = information(ContentFormat::Json, &params);
        prepare(src.path(), information, &Input::Json(Cow::Borrowed(input)))
            .await
            .unwrap();

        let fico = src.path().join(".fico");
        let gitignore = std::fs::read_to_string(fico.join(".gitignore")).unwrap();
        assert_eq!(gitignore, "*\n!.gitignore\n");
        let dir = fico.join("runs").join(RUN_ID);
        let written = std::fs::read(dir.join("inputs/input.json")).unwrap();
        assert_eq!(written, serde_json::to_vec_pretty(input).unwrap());
        // The new run's information, with the input's real size.
        let options = json!({ "duration": "10s", "solve.iterations": "50" });
        assert_eq!(
            read(&dir.join(format!("{RUN_ID}.json"))),
            json!({
                "description": "Local run created at 2026-10-13T15:00:00.123456Z",
                "id": RUN_ID,
                "metadata": {
                    "application_id": "routing",
                    "application_instance_id": "",
                    "application_version_id": "",
                    "created_at": CREATED_AT,
                    "duration": 0.0,
                    "error": "",
                    "execution_class": "local",
                    "execution_duration": 0.0,
                    "experiment_id": "",
                    "experiment_type": "",
                    "format": { "input": { "type": "json" }, "output": { "type": "json" } },
                    "initiated_at": CREATED_AT,
                    "input_size": written.len() as f64,
                    "options": {
                        "active_options": options,
                        "options_summary": [
                            { "name": "duration", "source": "run", "value": "10s" },
                            { "name": "solve.iterations", "source": "run", "value": "50" }
                        ],
                        "request_options": options
                    },
                    "output_size": 0.0,
                    "queuing_disabled": true,
                    "queuing_priority": 0,
                    "run_type": { "type": "standard", "definition_id": "", "reference_id": "" },
                    "runtime": "local",
                    "secrets_collection_id": "",
                    "status_v2": "queued",
                    "tracking": { "cloned_run_id": "", "input_id": "" }
                },
                "name": "local run local-3k9x0q2a",
                "user_email": "",
                "console_url": ""
            })
        );
    }

    #[tokio::test]
    async fn prepares_a_run_with_multi_file_input() {
        let root = TempDir::new();
        let src = root.path().join("routing");
        let inputs = root.path().join("inputs");
        write(&inputs.join("stops.csv"), "id\n1\n");
        write(&inputs.join("fleet/vehicles.csv"), "id\nv1\n");
        let params = RunCreateParams {
            name: Some("Monday".to_owned()),
            description: Some(String::new()),
            ..RunCreateParams::default()
        };
        let information = information(ContentFormat::MultiFile, &params);
        prepare(&src, information, &Input::Dir(Cow::Borrowed(&inputs)))
            .await
            .unwrap();

        let dir = src.join(".fico/runs").join(RUN_ID);
        let copied = |name: &str| std::fs::read_to_string(dir.join("inputs").join(name));
        assert_eq!(copied("stops.csv").unwrap(), "id\n1\n");
        assert_eq!(copied("fleet/vehicles.csv").unwrap(), "id\nv1\n");
        let information = read(&dir.join(format!("{RUN_ID}.json")));
        assert_eq!(information["name"], "Monday");
        assert_eq!(information["description"], "");
        assert_eq!(information["metadata"]["input_size"], json!(11.0));
        assert_eq!(
            information["metadata"]["format"],
            json!({ "input": { "type": "multi-file" }, "output": { "type": "multi-file" } })
        );
        assert_eq!(
            information["metadata"]["options"],
            json!({ "active_options": {}, "options_summary": [], "request_options": {} })
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn copies_links_to_files_and_skips_links_to_directories() {
        let root = TempDir::new();
        let inputs = root.path().join("inputs");
        write(&inputs.join("stops.csv"), "id\n1\n");
        write(&root.path().join("shared/fleet.csv"), "id\nv1\n");
        std::os::unix::fs::symlink(
            root.path().join("shared/fleet.csv"),
            inputs.join("fleet.csv"),
        )
        .unwrap();
        std::os::unix::fs::symlink(root.path().join("shared"), inputs.join("shared")).unwrap();
        std::os::unix::fs::symlink(root.path().join("gone"), inputs.join("broken")).unwrap();
        let to = root.path().join("copy");
        let copied = copy_dir(&inputs, &to).await.unwrap();
        assert_eq!(copied, 11);
        assert_eq!(
            std::fs::read_to_string(to.join("fleet.csv")).unwrap(),
            "id\nv1\n"
        );
        assert!(!to.join("fleet.csv").is_symlink());
        assert!(!to.join("shared").exists() && !to.join("broken").exists());
    }

    #[tokio::test]
    async fn keeps_an_existing_gitignore() {
        let src = TempDir::new();
        let gitignore = src.path().join(".fico/.gitignore");
        write(&gitignore, "runs/\n");
        let information = information(ContentFormat::Json, &RunCreateParams::default());
        let input = Map::new();
        prepare(src.path(), information, &Input::Json(Cow::Borrowed(&input)))
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(gitignore).unwrap(), "runs/\n");
    }

    #[tokio::test]
    async fn never_reuses_a_run_directory() {
        let src = TempDir::new();
        let dir = src.path().join(".fico/runs").join(RUN_ID);
        write(&dir.join("keep.txt"), "another run");
        let information = information(ContentFormat::Json, &RunCreateParams::default());
        let input = Map::new();
        let err = prepare(src.path(), information, &Input::Json(Cow::Borrowed(&input)))
            .await
            .unwrap_err();
        match err {
            Error::Io { path, source, .. } => {
                assert_eq!(path, dir);
                assert_eq!(source.kind(), io::ErrorKind::AlreadyExists);
            }
            other => panic!("expected an existing directory, got {other:?}"),
        }
        assert!(dir.join("keep.txt").exists());
    }

    #[tokio::test]
    async fn removes_the_run_when_its_input_cannot_be_written() {
        let src = TempDir::new();
        write(&src.path().join("workspace.yaml"), "files: [main.py]\n");
        let information = information(ContentFormat::MultiFile, &RunCreateParams::default());
        // The workspace's own directory holds the run's directory.
        let err = prepare(
            src.path(),
            information,
            &Input::Dir(Cow::Borrowed(src.path())),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
        assert!(!src.path().join(".fico/runs").join(RUN_ID).exists());
    }

    #[tokio::test]
    async fn takes_exactly_one_input() {
        let root = TempDir::new();
        let file = root.path().join("input.json");
        write(&file, "{}");
        let params = |input: Option<Value>, input_dir: Option<&Path>| RunCreateParams {
            input,
            input_dir: input_dir.map(Path::to_owned),
            ..RunCreateParams::default()
        };
        let object = json!({ "vehicles": 2 });
        let ok = params(Some(object.clone()), None);
        assert_eq!(
            new_input(&ok).await.unwrap(),
            Input::Json(Cow::Borrowed(object.as_object().unwrap()))
        );
        let ok = params(None, Some(root.path()));
        assert_eq!(
            new_input(&ok).await.unwrap(),
            Input::Dir(Cow::Borrowed(root.path()))
        );
        // An empty directory path is no directory.
        let ok = params(Some(object.clone()), Some(Path::new("")));
        assert!(matches!(new_input(&ok).await, Ok(Input::Json(_))));
        for invalid in [
            params(None, None),
            params(Some(object), Some(root.path())),
            params(Some(json!([1, 2])), None),
            params(Some(json!("routes")), None),
            params(None, Some(&file)),
        ] {
            let err = new_input(&invalid).await.unwrap_err();
            assert!(matches!(err, Error::Invalid { .. }), "{invalid:?}: {err:?}");
        }
        let missing = params(None, Some(&root.path().join("missing")));
        let err = new_input(&missing).await.unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn reads_the_input_from_a_json_file() {
        let root = TempDir::new();
        let file = root.path().join("input.json");
        write(&file, r#"{ "vehicles": 2 }"#);
        let params = |input_file: &Path| RunCreateParams {
            input_file: Some(input_file.to_owned()),
            ..RunCreateParams::default()
        };
        let from_file = params(&file);
        let input = new_input(&from_file).await.unwrap();
        let expected = json!({ "vehicles": 2 });
        assert_eq!(
            input,
            Input::Json(Cow::Borrowed(expected.as_object().unwrap()))
        );

        // A file is one input: with another one, the run has two.
        for (input, input_dir) in [(Some(json!({})), None), (None, Some(root.path()))] {
            let both = RunCreateParams {
                input,
                input_dir: input_dir.map(Path::to_owned),
                ..params(&file)
            };
            let err = new_input(&both).await.unwrap_err();
            assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
        }
        // An empty path is no file.
        let empty = RunCreateParams {
            input: Some(json!({})),
            ..params(Path::new(""))
        };
        assert!(matches!(new_input(&empty).await, Ok(Input::Json(_))));

        // Neither JSON that is not an object, nor a tar.gz, nor a directory.
        let list = root.path().join("list.json");
        write(&list, "[1, 2]");
        let archive = root.path().join("input.tar.gz");
        std::fs::write(&archive, [0x1f, 0x8b, 0x08, 0x00]).unwrap();
        for file in [&list, &archive] {
            let err = new_input(&params(file)).await.unwrap_err();
            assert!(matches!(err, Error::Invalid { .. }), "{file:?}: {err:?}");
        }
        for file in [root.path().join("missing.json"), root.path().to_owned()] {
            let err = new_input(&params(&file)).await.unwrap_err();
            assert!(matches!(err, Error::Io { .. }), "{file:?}: {err:?}");
        }
    }

    #[test]
    fn checks_the_input_against_the_format() {
        let object = Map::new();
        let json = Input::Json(Cow::Borrowed(&object));
        let dir = Input::Dir(Cow::Borrowed(Path::new("inputs")));
        assert!(check_format(&json, ContentFormat::Json).is_ok());
        assert!(check_format(&dir, ContentFormat::MultiFile).is_ok());
        for (input, format) in [
            (&json, ContentFormat::MultiFile),
            (&dir, ContentFormat::Json),
            (&json, ContentFormat::Unknown),
            (&dir, ContentFormat::Unknown),
        ] {
            let err = check_format(input, format).unwrap_err();
            assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
        }
    }
}
