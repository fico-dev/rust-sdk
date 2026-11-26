//! Run operations, on `v1/applications/{workspace_id}/runs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::data::{pack_dir, read_file, set};
use super::{LATEST_ENVIRONMENT_ID, Platform};
use crate::decision::archive::unpack;
use crate::decision::clone::{clone_description, clone_name, cloned_options};
use crate::decision::compare::{check_run_ids, compare};
use crate::decision::path::api_path;
use crate::decision::polling::{Change, gone, poll, poll_change};
use crate::decision::time::optional_timestamp;
use crate::decision::{
    ContentFormat, DownloadUrl, ExternalRunResult, Format, FormatInput, FormatOutput, Metadata,
    PollingOptions, Run, RunComparison, RunConfiguration, RunCreated, RunInformation, RunLog,
    RunQueuing, RunResult, RunStatus, RunType, RunTypeConfiguration, TrackedRun, TrackedRunStatus,
};
use crate::tool::{InWorkspace, NoParams, Tool};
use crate::{Error, Response, Result};

/// Output or input larger than this, in bytes, is downloaded from a
/// presigned URL instead of returned inline, and input larger than this is
/// uploaded first.
const MAX_INLINE_SIZE: f64 = 5.0 * 1024.0 * 1024.0;

/// Tool arguments that address a run, plus the operation's parameters.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct RunRef<P> {
    /// ID of the workspace.
    pub(crate) workspace_id: String,
    /// ID of the run.
    pub(crate) run_id: String,
    #[serde(flatten)]
    pub(crate) params: P,
}

/// Parameters for [`Platform::create_run`].
///
/// Set exactly one input: `input`, `input_file`, `input_dir`,
/// `managed_input_id` or `upload_id`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RunCreateParams {
    /// Input of the run, as a JSON object, for a workspace with JSON input.
    /// Uploaded first when it is over 5 MiB. Set exactly one of `input`,
    /// `input_file`, `input_dir`, `managed_input_id` and `upload_id`.
    pub input: Option<Value>,
    /// File whose contents are the run's input: a JSON file, or a tar.gz of
    /// input files for a workspace with multi-file input. Uploaded as it is.
    /// Set exactly one of `input`, `input_file`, `input_dir`,
    /// `managed_input_id` and `upload_id`. Not available in remote mode.
    pub input_file: Option<PathBuf>,
    /// Directory whose files are the run's input, for workspaces that read
    /// several files. Uploaded as a tar.gz. Set exactly one of `input`,
    /// `input_file`, `input_dir`, `managed_input_id` and `upload_id`. Not
    /// available in remote mode.
    pub input_dir: Option<PathBuf>,
    /// ID of the managed input to run on. Set exactly one of `input`,
    /// `input_file`, `input_dir`, `managed_input_id` and `upload_id`.
    pub managed_input_id: Option<String>,
    /// ID of an upload whose data is the run's input, once uploaded. Set
    /// exactly one of `input`, `input_file`, `input_dir`, `managed_input_id`
    /// and `upload_id`.
    pub upload_id: Option<String>,
    /// ID of the environment to run in. Defaults to the workspace's default
    /// environment, which `default` names too. Ensemble runs take no
    /// environment: their definition sets one per run group.
    pub environment_id: Option<String>,
    /// Name of the run.
    pub name: Option<String>,
    /// Description of the run.
    pub description: Option<String>,
    /// Options to run the workspace with, by name, e.g.
    /// `{"solve.duration": "10s"}`.
    pub options: BTreeMap<String, String>,
    /// Configuration of the run. When it sets no format, the format follows
    /// the input: `multi-file` for a directory or a tar.gz, the managed
    /// input's own format, and `json` for JSON. For an `upload_id`, the
    /// platform takes the environment's or the build's. An ensemble run needs
    /// `run_type.definition_id`, and a definition ID with no run type makes
    /// an ensemble run.
    pub configuration: Option<RunConfiguration>,
}

/// Parameters for [`Platform::get_run`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RunGetParams {
    /// Polls until the run reaches a terminal state, then returns it. Without
    /// it, returns the run's current state. With the defaults, polling has no
    /// time or try limit.
    pub polling: Option<PollingOptions>,
    /// Directory to download the run's output into, for multi-file output,
    /// which needs it once the run has succeeded. Created only when files are
    /// downloaded, and files already there with the same names are
    /// overwritten. Not available in remote mode.
    pub output_dir: Option<PathBuf>,
}

/// Parameters for [`Platform::get_run_input`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RunInputGetParams {
    /// Directory to download the run's input into, for multi-file input,
    /// which needs it. Created only when files are downloaded, and files
    /// already there with the same names are overwritten. Not available in
    /// remote mode.
    pub output_dir: Option<PathBuf>,
}

/// Parameters for [`Platform::get_run_logs`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RunLogsGetParams {
    /// Polls until the run reaches a terminal state, then returns its logs.
    /// Without it, the run must have finished: the platform rejects the
    /// request while the run is queued or running. With the defaults, polling
    /// has no time or try limit.
    pub polling: Option<PollingOptions>,
}

/// Parameters for [`Platform::update_run`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RunUpdateParams {
    /// New name of the run.
    pub name: Option<String>,
    /// New description of the run.
    pub description: Option<String>,
}

/// Parameters for [`Platform::delete_run`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RunDeleteParams {
    /// Polls until the run is gone. The platform deletes an ensemble run
    /// and its child runs in the background; other runs are gone once the
    /// delete returns. With the defaults, polling has no time or try limit.
    pub polling: Option<PollingOptions>,
}

/// Parameters for [`Platform::list_runs`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RunListParams {
    /// Lists only the runs with this status. Runs of every status if unset.
    pub status: Option<RunStatus>,
    /// Most runs to list, at least 1. The platform lists the newest runs
    /// first. No limit if unset.
    pub limit: Option<u32>,
    /// Lists only the runs created at or after this time, in RFC 3339.
    /// Defaults to a year before `end_time`.
    pub start_time: Option<String>,
    /// Lists only the runs created before this time, in RFC 3339. Defaults
    /// to now.
    pub end_time: Option<String>,
    /// Whether to list the runs of experiments too, such as the runs of an
    /// acceptance or scenario test. Defaults to `false` (API:
    /// `include_exp_runs`).
    pub include_experiment_runs: Option<bool>,
    /// Whether to list the runs that a shadow test made on its candidates
    /// too. Defaults to `false` (API: `include_shad_runs`).
    pub include_shadow_runs: Option<bool>,
    /// Whether to list the child runs of ensemble runs too. Defaults to
    /// `false` (API: `include_ens_runs`).
    pub include_ensemble_runs: Option<bool>,
}

/// Parameters for [`Platform::clone_run`]. Each one that is set replaces the
/// cloned run's, and empty values count as unset.
///
/// Set at most one input: `input`, `input_file`, `input_dir`,
/// `managed_input_id` or `upload_id`. With none, the clone runs on the
/// cloned run's input.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RunCloneParams {
    /// Input of the run, as a JSON object, for a workspace with JSON input.
    /// Uploaded first when it is over 5 MiB. Set at most one of `input`,
    /// `input_file`, `input_dir`, `managed_input_id` and `upload_id`; with
    /// none, the run gets the cloned run's input.
    pub input: Option<Value>,
    /// File whose contents are the run's input: a JSON file, or a tar.gz of
    /// input files for a workspace with multi-file input. Uploaded as it is.
    /// Set at most one of `input`, `input_file`, `input_dir`,
    /// `managed_input_id` and `upload_id`. Not available in remote mode.
    pub input_file: Option<PathBuf>,
    /// Directory whose files are the run's input, for workspaces that read
    /// several files. Uploaded as a tar.gz. Set at most one of `input`,
    /// `input_file`, `input_dir`, `managed_input_id` and `upload_id`. Not
    /// available in remote mode.
    pub input_dir: Option<PathBuf>,
    /// ID of the managed input to run on. Set at most one of `input`,
    /// `input_file`, `input_dir`, `managed_input_id` and `upload_id`.
    pub managed_input_id: Option<String>,
    /// ID of an upload whose data is the run's input, once uploaded. Set at
    /// most one of `input`, `input_file`, `input_dir`, `managed_input_id`
    /// and `upload_id`.
    pub upload_id: Option<String>,
    /// ID of the environment to run in. Defaults to the cloned run's.
    /// `default` names the workspace's default environment. Ensemble runs
    /// take no environment: their definition sets one per run group.
    pub environment_id: Option<String>,
    /// Name of the run, cut to 128 characters. Defaults to the cloned run's
    /// name, or its ID if it has none, followed by ` clone`.
    pub name: Option<String>,
    /// Description of the run, cut to 256 characters. Defaults to `Clone of
    /// ` and the cloned run's description, or its ID if it has none.
    pub description: Option<String>,
    /// Options to run the workspace with, by name, e.g.
    /// `{"solve.duration": "10s"}`. Defaults to the cloned run's options.
    pub options: BTreeMap<String, String>,
    /// Configuration of the run. Each field it sets replaces the cloned
    /// run's: `queuing.priority` and `queuing.disabled` each on its own, and
    /// `run_type` as a whole. The other fields are the cloned run's, except
    /// what the platform sets itself: the run type and execution class of a
    /// tracked run, and the run type of an ensemble child. Without a format
    /// here, the format follows a new input as it does for a new run, and
    /// is otherwise the cloned run's. An ensemble run needs
    /// `run_type.definition_id`, and a definition ID with no run type makes
    /// an ensemble run.
    pub configuration: Option<RunConfiguration>,
}

/// Parameters for [`Platform::compare_runs`].
///
/// `run_ids` is required in tool arguments. The `Default` value leaves it
/// empty, which [`Platform::compare_runs`] rejects, so set it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
pub struct RunCompareParams {
    /// IDs of the runs to compare, from 1 to 20, each one once.
    pub run_ids: Vec<String>,
}

/// Parameters for [`Platform::track_run`].
///
/// `tracked_run` is required in tool arguments.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
pub struct RunTrackParams {
    /// The run to record: its status, input, output, logs, metrics and
    /// assets.
    pub tracked_run: TrackedRun,
    /// ID of the environment to record the run in. Defaults to `latest`.
    /// `default` names the workspace's default environment; for a workspace
    /// without one, the platform records the run in its reserved `devint`,
    /// which needs no push.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Format of the run's input and output: `json`, or `multi-file` for
    /// input and output directories. Defaults to `multi-file` when the input
    /// is a directory, and to `json` otherwise.
    #[serde(default)]
    pub content_format: Option<ContentFormat>,
}

impl Platform {
    /// Gets a run, including its output.
    ///
    /// With `params.polling`, waits until the run reaches a terminal state.
    /// Polling has no limit unless `max_duration` or `max_tries` is set, and a
    /// status this SDK does not know is not terminal, so if the platform adds
    /// a final status, polling waits until a limit or the caller's own timeout
    /// stops it.
    ///
    /// JSON output is returned in `output`, downloaded first when it is
    /// large. Multi-file output is downloaded and unpacked into
    /// `params.output_dir` instead, and `output` is left out. Large and
    /// multi-file output is only downloaded once the run has succeeded.
    ///
    /// A failed run whose output is over the platform's inline limit, about
    /// 5.8 MiB, has no `error_log`: `output` holds the
    /// `{"error_log_url": …}` link that the platform returns instead.
    ///
    /// With a console base URL, `console_url` is the run's
    /// [`Platform::run_console_url`]. Without one, it is what the platform
    /// sends.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] for a succeeded run with multi-file output
    /// if `params.output_dir` is not set, and [`Error::Wait`] if polling runs
    /// out of time or tries.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn get_run(
        &self,
        workspace_id: &str,
        run_id: &str,
        params: RunGetParams,
    ) -> Result<RunResult> {
        let information = self
            .fetch_run_information(workspace_id, run_id, params.polling.as_ref())
            .await?;
        let metadata = &information.metadata;
        let json = output_format(metadata) == ContentFormat::Json;
        // The platform answers with a URL for output it can't return inline.
        let by_url = !json || metadata.output_size > MAX_INLINE_SIZE;
        let needs_dir = !json && params.output_dir.is_none();
        // A succeeded run stays succeeded, so fail before `GET R`, for which
        // the platform copies the whole output to make its link.
        if needs_dir && metadata.status == RunStatus::Succeeded {
            return Err(needs_output_dir());
        }
        let path = api_path(&["v1", "applications", workspace_id, "runs", run_id])?;
        let mut request = self.client.get(path);
        if by_url {
            request = request.query(&[("format", "url")]);
        }
        let mut result: RunResult = request.send_json().await?;
        self.fill_console_url(workspace_id, &mut result.information);
        let run_type = result.information.metadata.run_type.as_ref();
        if run_type.and_then(|run_type| run_type.run_type) == Some(RunType::Ensemble) {
            let path = api_path(&[
                "v1",
                "applications",
                workspace_id,
                "runs",
                run_id,
                "ensemble",
            ])?;
            result.ensemble = Some(self.client.get(path).send_json().await?);
        }
        // For a failed or canceled run, the output endpoint has the run's
        // stdout, or nothing, rather than its output. The result's status is
        // the newer one: the run may have finished since the metadata.
        if !by_url || result.information.metadata.status != RunStatus::Succeeded {
            return Ok(result);
        }
        // The run succeeded after its metadata was read.
        if needs_dir {
            return Err(needs_output_dir());
        }
        let path = api_path(&["v1", "applications", workspace_id, "runs", run_id, "output"])?;
        let content = self.download(&path).await?;
        match params.output_dir {
            Some(dir) if !json => {
                unpack(content.into_bytes(), dir).await?;
                // It held the `{"url": …}` of the download.
                result.output = None;
            }
            _ => result.output = Some(content.json()?),
        }
        Ok(result)
    }

    /// Gets a run's information and metadata, without its output.
    ///
    /// With a console base URL, `console_url` is the run's
    /// [`Platform::run_console_url`]. Without one, it is what the platform
    /// sends.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Status`] with status 404 if the run does not exist, and
    /// the [errors of every method](Platform#errors).
    pub async fn get_run_information(
        &self,
        workspace_id: &str,
        run_id: &str,
    ) -> Result<RunInformation> {
        let path = api_path(&[
            "v1",
            "applications",
            workspace_id,
            "runs",
            run_id,
            "metadata",
        ])?;
        let mut information = self.client.get(path).send_json().await?;
        self.fill_console_url(workspace_id, &mut information);
        Ok(information)
    }

    /// Gets the input of a run.
    ///
    /// JSON input is returned, downloaded first when it is large. Returns
    /// `None` for multi-file input: its files are downloaded into
    /// `params.output_dir` instead.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] for multi-file input if `params.output_dir`
    /// is not set.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn get_run_input(
        &self,
        workspace_id: &str,
        run_id: &str,
        params: RunInputGetParams,
    ) -> Result<Option<Value>> {
        let metadata = self
            .get_run_information(workspace_id, run_id)
            .await?
            .metadata;
        let json = metadata.format.format_input.input_type == ContentFormat::Json;
        if !json && params.output_dir.is_none() {
            return Err(Error::Invalid {
                reason: "the run's input is multi-file: set `output_dir` to download it".to_owned(),
            });
        }
        let path = api_path(&["v1", "applications", workspace_id, "runs", run_id, "input"])?;
        if json && metadata.input_size <= MAX_INLINE_SIZE {
            return self.client.get(path).send_json().await.map(Some);
        }
        let content = self.download(&path).await?;
        match params.output_dir {
            Some(dir) if !json => {
                unpack(content.into_bytes(), dir).await?;
                Ok(None)
            }
            _ => content.json().map(Some),
        }
    }

    /// Gets the logs of a run.
    ///
    /// The platform has the logs once the run reaches a terminal state, and
    /// rejects the request with a `400` before. With `params.polling`, waits
    /// until the run gets there. Polling has no limit unless `max_duration` or
    /// `max_tries` is set, and a status this SDK does not know is not
    /// terminal, so if the platform adds a final status, polling waits until a
    /// limit or the caller's own timeout stops it.
    ///
    /// Logs over the platform's inline limit, about 5.8 MiB, get a `400` too:
    /// the platform only serves them as a download, which this does not
    /// fetch.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Wait`] if polling runs out of time or tries.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn get_run_logs(
        &self,
        workspace_id: &str,
        run_id: &str,
        params: RunLogsGetParams,
    ) -> Result<RunLog> {
        if let Some(options) = &params.polling {
            self.fetch_run_information(workspace_id, run_id, Some(options))
                .await?;
        }
        let path = api_path(&["v1", "applications", workspace_id, "runs", run_id, "logs"])?;
        self.client.get(path).send_json().await
    }

    /// Lists the runs of a workspace.
    ///
    /// `params.status` keeps only the runs with that status, filtered here,
    /// since the platform does not filter by status. With `params.limit`,
    /// listing stops at that many runs, fetching no more pages than needed.
    /// The platform lists the runs of the past year by default, and leaves
    /// out the runs of experiments, of shadow tests' candidates and of
    /// ensemble runs' children unless `params` asks for them. An empty
    /// `start_time` or `end_time` counts as unset.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `params.limit` is 0, or if `start_time`
    /// or `end_time` is not an RFC 3339 timestamp.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn list_runs(&self, workspace_id: &str, params: RunListParams) -> Result<Vec<Run>> {
        let limit = match params.limit {
            Some(0) => {
                return Err(Error::Invalid {
                    reason: "`limit` must be at least 1".to_owned(),
                });
            }
            Some(limit) => usize::try_from(limit).unwrap_or(usize::MAX),
            None => usize::MAX,
        };
        let start = optional_timestamp("start_time", params.start_time)?;
        let end = optional_timestamp("end_time", params.end_time)?;
        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(start) = start {
            query.push(("start", start));
        }
        if let Some(end) = end {
            query.push(("end", end));
        }
        for (key, include) in [
            ("include_exp_runs", params.include_experiment_runs),
            ("include_shad_runs", params.include_shadow_runs),
            ("include_ens_runs", params.include_ensemble_runs),
        ] {
            if let Some(include) = include {
                query.push((key, include.to_string()));
            }
        }
        let path = api_path(&["v1", "applications", workspace_id, "runs"])?;
        let mut pages = self
            .client
            .get(path)
            .query(&query)
            .paginate::<Run>()
            .items_key("runs");
        let mut runs = Vec::new();
        while runs.len() < limit
            && let Some(run) = pages.next().await?
        {
            if params.status.is_none_or(|status| run.status == status) {
                runs.push(run);
            }
        }
        Ok(runs)
    }

    /// Updates a run's name and description.
    ///
    /// Only the fields that `params` sets change. An empty string counts as
    /// set.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`], before any request, if `params` sets no
    /// field.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn update_run(
        &self,
        workspace_id: &str,
        run_id: &str,
        params: RunUpdateParams,
    ) -> Result<RunInformation> {
        if params.name.is_none() && params.description.is_none() {
            return Err(Error::Invalid {
                reason: "nothing to update: set `name` or `description`".to_owned(),
            });
        }
        let mut body = Map::new();
        if let Some(name) = params.name {
            body.insert("name".to_owned(), name.into());
        }
        if let Some(description) = params.description {
            body.insert("description".to_owned(), description.into());
        }
        let path = api_path(&["v1", "applications", workspace_id, "runs", run_id])?;
        let mut information: RunInformation = self.client.put(path).json(&body).send_json().await?;
        self.fill_console_url(workspace_id, &mut information);
        Ok(information)
    }

    /// Cancels a run.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn cancel_run(&self, workspace_id: &str, run_id: &str) -> Result<()> {
        let path = api_path(&["v1", "applications", workspace_id, "runs", run_id, "cancel"])?;
        self.client.patch(path).send().await?;
        Ok(())
    }

    /// Deletes a run.
    ///
    /// The platform deletes an ensemble run, and its child runs, in the
    /// background: with `params.polling`, this waits until a GET of the run
    /// is a 404.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Wait`] if polling runs out of time or tries.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn delete_run(
        &self,
        workspace_id: &str,
        run_id: &str,
        params: RunDeleteParams,
    ) -> Result<()> {
        let path = api_path(&["v1", "applications", workspace_id, "runs", run_id])?;
        self.client.delete(path).send().await?;
        if let Some(options) = params.polling {
            poll_change(&options, || async {
                let result = self.get_run_information(workspace_id, run_id).await;
                gone(result, |_| Change::Pending)
            })
            .await?;
        }
        Ok(())
    }

    /// Gets a run's information. With `polling`, polls it until the run
    /// reaches a terminal state instead.
    async fn fetch_run_information(
        &self,
        workspace_id: &str,
        run_id: &str,
        polling: Option<&PollingOptions>,
    ) -> Result<RunInformation> {
        let Some(options) = polling else {
            return self.get_run_information(workspace_id, run_id).await;
        };
        poll(options, move || async move {
            let information = self.get_run_information(workspace_id, run_id).await?;
            Ok(information
                .metadata
                .status
                .is_terminal()
                .then_some(information))
        })
        .await
    }

    /// Sets `information.console_url` to the run's console link when there
    /// is a console base URL. Leaves it as the platform sent it otherwise.
    fn fill_console_url(&self, workspace_id: &str, information: &mut RunInformation) {
        if let Some(url) = self.run_console_url(workspace_id, &information.id) {
            information.console_url = url;
        }
    }

    /// Downloads a run's input or output: gets its presigned URL from `path`
    /// with `?format=url`, then the content at that URL.
    ///
    /// The URL is on another origin and needs no authorization, so no bearer
    /// token goes with it. The client's read limit applies to each chunk, so
    /// a large download that keeps arriving never runs out of it.
    async fn download(&self, path: &str) -> Result<Response> {
        let url: DownloadUrl = self
            .client
            .get(path)
            .query(&[("format", "url")])
            .send_json()
            .await?;
        self.client.get(&url.url).no_auth().send().await
    }

    /// Starts a run.
    ///
    /// Returns as soon as the run is created. To wait for it, call
    /// [`Platform::get_run`] with `polling`.
    ///
    /// `params.input` over 5 MiB, `params.input_file` and `params.input_dir`
    /// are uploaded first, through [`Platform::create_upload`] and a
    /// presigned PUT whose read limit grows with the upload's size, as
    /// [`Platform::upload_data`] says. For
    /// `params.managed_input_id`, the managed input is read first, for its
    /// format. For the environment
    /// [`DEFAULT_ENVIRONMENT_ID`](super::DEFAULT_ENVIRONMENT_ID), the
    /// workspace is read first, for its default environment.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`], before any request, if `params` does not
    /// set exactly one input, `input` is not a JSON object, `input_file` is
    /// neither JSON nor a tar.gz, `input_dir` is not a directory, the
    /// queuing priority is not between 1 and 9, `integration_id` is set with
    /// an execution class other than `integration`, an ensemble run has no
    /// definition ID, or another kind of run has one, or the run type or a
    /// format is `Unknown`. Returns [`Error::Io`] if the input file or
    /// directory cannot be read.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn create_run(
        &self,
        workspace_id: &str,
        params: RunCreateParams,
    ) -> Result<RunCreated> {
        // Checks the workspace ID before any request.
        api_path(&["v1", "applications", workspace_id, "runs"])?;
        let mut configuration = params.configuration.unwrap_or_default();
        let fields = InputFields {
            input: params.input.as_ref(),
            input_file: params.input_file.as_deref(),
            input_dir: params.input_dir.as_deref(),
            managed_input_id: params.managed_input_id.as_deref(),
            upload_id: params.upload_id.as_deref(),
        };
        const RULE: &str = "exactly one";
        let source = given_input(fields, RULE)?.ok_or_else(|| Error::Invalid {
            reason: format!("set {RULE} of {INPUT_FIELDS}"),
        })?;
        let ensemble = check_configuration(&mut configuration)?;
        let (input, format) = load_input(source).await?;
        let environment_id = self
            .run_environment(workspace_id, params.environment_id.as_deref(), ensemble)
            .await?;
        if configuration.format.is_none() {
            configuration.format = self.input_format(workspace_id, &input, format).await?;
        }
        let run = NewRun {
            input,
            environment_id: environment_id.as_deref(),
            ensemble,
            name: params.name.as_deref(),
            description: params.description.as_deref(),
            options: &params.options,
            configuration: &configuration,
            cloned_run_id: None,
            result: None,
        };
        self.start_run(workspace_id, run).await
    }

    /// Starts a run with the input and configuration of an existing run.
    ///
    /// What `params` sets replaces the cloned run's, read with
    /// [`Platform::get_run_information`]: the input, the environment, the
    /// name and description (each cut, or defaulted, as for a local clone),
    /// the options, and each field of the configuration on its own. The new
    /// run records the cloned run's ID in its `tracking.cloned_run_id`.
    /// Returns as soon as the run is created, as [`Platform::create_run`]
    /// does.
    ///
    /// Without an input in `params`, the clone runs on the cloned run's
    /// managed input, if it had one. Otherwise its input is read: JSON of up
    /// to 5 MiB is sent inline, and larger or multi-file input is downloaded
    /// from a presigned URL and uploaded again as it is. A new input is read
    /// and uploaded as [`Platform::create_run`] does.
    ///
    /// A new input replaces the cloned run's managed input, an unset queuing
    /// setting keeps the cloned run's, a definition ID with no run type makes
    /// an ensemble run, and an `upload_id` skips reading the cloned run's
    /// input. The environment
    /// [`DEFAULT_ENVIRONMENT_ID`](super::DEFAULT_ENVIRONMENT_ID) is resolved
    /// as [`Platform::create_run`] does.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`], before any request, if `params` sets more
    /// than one input or the configuration is not valid, both as for
    /// [`Platform::create_run`], and [`Error::Io`] if the input file or
    /// directory cannot be read.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn clone_run(
        &self,
        workspace_id: &str,
        run_id: &str,
        params: RunCloneParams,
    ) -> Result<RunCreated> {
        let mut overrides = params.configuration.unwrap_or_default();
        let fields = InputFields {
            input: params.input.as_ref(),
            input_file: params.input_file.as_deref(),
            input_dir: params.input_dir.as_deref(),
            managed_input_id: params.managed_input_id.as_deref(),
            upload_id: params.upload_id.as_deref(),
        };
        let source = given_input(fields, "at most one")?;
        check_configuration(&mut overrides)?;
        let given = match source {
            Some(source) => Some(load_input(source).await?),
            None => None,
        };

        let cloned = self.get_run_information(workspace_id, run_id).await?;
        let metadata = &cloned.metadata;
        let mut configuration = clone_configuration(overrides, metadata);
        let ensemble = check_configuration(&mut configuration)?;
        let fetched;
        let input = match given {
            Some((input, format)) => {
                if configuration.format.is_none() {
                    configuration.format = self.input_format(workspace_id, &input, format).await?;
                }
                input
            }
            None => match cloned_managed_input(metadata) {
                Some(managed_input_id) => BodyInput::ManagedInput(managed_input_id),
                None => match self.cloned_input(workspace_id, run_id, metadata).await? {
                    ClonedInput::Json(input) => {
                        fetched = input;
                        BodyInput::Inline(&fetched)
                    }
                    ClonedInput::Bytes(bytes) => BodyInput::Upload(bytes),
                },
            },
        };
        if configuration.format.is_none() {
            configuration.format = known_format(&metadata.format);
        }

        let options = match params.options.is_empty() {
            true => cloned_options(&cloned),
            false => params.options,
        };
        // Only a given ID is resolved: the cloned run's is the one it ran on.
        let environment_id = match non_empty(params.environment_id.as_deref()) {
            Some(environment_id) => {
                self.run_environment(workspace_id, Some(environment_id), ensemble)
                    .await?
            }
            None => non_empty(Some(&metadata.environment_id)).map(str::to_owned),
        };
        let name = clone_name(params.name.as_deref(), &cloned);
        let description = clone_description(params.description.as_deref(), &cloned);
        let run = NewRun {
            input,
            environment_id: environment_id.as_deref(),
            ensemble,
            name: Some(&name),
            description: Some(&description),
            options: &options,
            configuration: &configuration,
            cloned_run_id: Some(run_id),
            result: None,
        };
        self.start_run(workspace_id, run).await
    }

    /// Compares runs field by field.
    ///
    /// Reads each run's information with [`Platform::get_run_information`],
    /// in order, and compares them: the runs' information fields but
    /// `metadata` and `console_url`; their metadata fields but metrics and
    /// options, with the format as `content_format`, the integration's and
    /// the tracking's fields on their own, the run type as its type and
    /// `status_v2` as `status`; their metrics, out of a lone `metrics` key
    /// that wraps them, with decimals rounded to 5 digits; and their active
    /// options. Field names are the API's, and each field has the value of
    /// every run that has it, by run ID. `has_differences` says whether those
    /// values are not all equal as JSON, numbers compared by value. `metrics`
    /// and `options` are unset when no run has any. Local runs compare the
    /// same way
    /// ([`Local::compare_runs`](crate::decision::local::Local::compare_runs)).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`], before any request, if `params.run_ids`
    /// has fewer than 1 or more than 20 runs, or the same run twice.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn compare_runs(
        &self,
        workspace_id: &str,
        params: RunCompareParams,
    ) -> Result<RunComparison> {
        let run_ids = params.run_ids;
        check_run_ids(&run_ids, "run")?;
        let mut runs = Vec::with_capacity(run_ids.len());
        for run_id in &run_ids {
            runs.push(self.get_run_information(workspace_id, run_id).await?);
        }
        Ok(compare(run_ids, &runs))
    }

    /// Records a run that ran outside the platform.
    ///
    /// It uploads each part of the run through [`Platform::create_upload`]
    /// and a presigned PUT whose read limit grows with the part's size, as
    /// [`Platform::upload_data`] says, in this order: the input, the output,
    /// the logs, the metrics and the assets, wrapped as `{"assets": […]}`.
    /// Then it records the run with `POST A/runs`, an `external` run type,
    /// the content format for the input and the output, and the uploads'
    /// IDs.
    /// Directories are uploaded as a tar.gz. Every file is read, and every
    /// directory packed, before any request. Returns the new run's ID.
    ///
    /// The environment defaults to `latest`, and
    /// [`DEFAULT_ENVIRONMENT_ID`](super::DEFAULT_ENVIRONMENT_ID) is resolved
    /// before the uploads, as [`Platform::create_run`] does.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`], before any request, if
    /// `params.tracked_run` does not set exactly one input, sets more than
    /// one output or more than one source of logs, or sets neither an output
    /// nor logs, which the platform needs; if it has an error message but
    /// did not fail; if `input` is not a JSON object, a file is not a file or
    /// does not hold what it should, or a directory is not one; or if the
    /// content format does not fit the input and output. Returns
    /// [`Error::Io`] if a file or directory cannot be read.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn track_run(
        &self,
        workspace_id: &str,
        params: RunTrackParams,
    ) -> Result<RunCreated> {
        // Checks the workspace ID before any request.
        api_path(&["v1", "applications", workspace_id, "runs"])?;
        let run = &params.tracked_run;
        if non_empty(run.error.as_deref()).is_some() && run.status != TrackedRunStatus::Failed {
            return Err(invalid("`error` is only for runs that failed"));
        }
        let parts = TrackedParts::read(run, params.content_format).await?;
        // Before the uploads, so that a failure leaves none behind.
        let environment_id =
            non_empty(params.environment_id.as_deref()).unwrap_or(LATEST_ENVIRONMENT_ID);
        let environment_id = self
            .resolve_environment(workspace_id, environment_id)
            .await?;
        self.track_parts(workspace_id, &params, parts, environment_id.as_deref())
            .await
    }

    /// As [`Platform::track_run`], with the environment already resolved
    /// by [`Platform::resolve_environment`]: `None` sends none, for the
    /// platform's fallback. `params.environment_id` is not read.
    pub(crate) async fn track_run_in(
        &self,
        workspace_id: &str,
        params: RunTrackParams,
        environment_id: Option<&str>,
    ) -> Result<RunCreated> {
        api_path(&["v1", "applications", workspace_id, "runs"])?;
        let run = &params.tracked_run;
        if non_empty(run.error.as_deref()).is_some() && run.status != TrackedRunStatus::Failed {
            return Err(invalid("`error` is only for runs that failed"));
        }
        let parts = TrackedParts::read(run, params.content_format).await?;
        self.track_parts(workspace_id, &params, parts, environment_id)
            .await
    }

    /// Records `params.tracked_run`, read into `parts`, in `environment_id`,
    /// already resolved, or in the platform's fallback when it is `None`, as
    /// [`Platform::track_run`] does.
    async fn track_parts(
        &self,
        workspace_id: &str,
        params: &RunTrackParams,
        parts: TrackedParts,
        environment_id: Option<&str>,
    ) -> Result<RunCreated> {
        let run = &params.tracked_run;

        let mut result = ExternalRunResult {
            status: Some(run.status),
            execution_duration: run.duration,
            error_message: non_empty(run.error.as_deref()).map(str::to_owned),
            ..Default::default()
        };
        let input_upload_id = self.upload_part(workspace_id, parts.input).await?;
        if let Some(output) = parts.output {
            result.output_upload_id = Some(self.upload_part(workspace_id, output).await?);
        }
        if let Some(logs) = parts.logs {
            result.error_upload_id = Some(self.upload_part(workspace_id, logs).await?);
        }
        if let Some(metrics) = parts.metrics {
            result.metrics_upload_id = Some(self.upload_part(workspace_id, metrics).await?);
        }
        if let Some(assets) = parts.assets {
            result.assets_upload_id = Some(self.upload_part(workspace_id, assets).await?);
        }

        let configuration = RunConfiguration {
            format: Some(symmetric_format(parts.format)),
            run_type: Some(RunTypeConfiguration {
                run_type: Some(RunType::External),
                ..Default::default()
            }),
            ..Default::default()
        };
        let run = NewRun {
            input: BodyInput::UploadId(&input_upload_id),
            environment_id,
            ensemble: false,
            name: run.name.as_deref(),
            description: run.description.as_deref(),
            options: &BTreeMap::new(),
            configuration: &configuration,
            cloned_run_id: None,
            result: Some(&result),
        };
        self.start_run(workspace_id, run).await
    }

    /// The environment to start a run in: none for an ensemble run, which
    /// takes none, or for an unset or empty `environment_id`, and otherwise
    /// `environment_id`, resolved by [`Platform::resolve_environment`].
    async fn run_environment(
        &self,
        workspace_id: &str,
        environment_id: Option<&str>,
        ensemble: bool,
    ) -> Result<Option<String>> {
        match non_empty(environment_id) {
            Some(environment_id) if !ensemble => {
                self.resolve_environment(workspace_id, environment_id).await
            }
            _ => Ok(None),
        }
    }

    /// Uploads `bytes` through a new upload URL, and returns its ID.
    async fn upload_part(&self, workspace_id: &str, bytes: Vec<u8>) -> Result<String> {
        let upload = self.create_upload(workspace_id).await?;
        self.put_upload(&upload.upload_url, bytes).await?;
        Ok(upload.upload_id)
    }

    /// The format that `input` implies, `implied`, unless it is a managed
    /// input, whose own format is read, or JSON when it has none.
    async fn input_format(
        &self,
        workspace_id: &str,
        input: &BodyInput<'_>,
        implied: Option<Format>,
    ) -> Result<Option<Format>> {
        match input {
            BodyInput::ManagedInput(managed_input_id) => Ok(Some(
                self.get_managed_input(workspace_id, managed_input_id)
                    .await?
                    .format
                    .unwrap_or_else(json_format),
            )),
            _ => Ok(implied),
        }
    }

    /// The input of the run `run_id`, whose metadata is `metadata`, for a
    /// clone of it: inline when it is a JSON object of at most 5 MiB, and
    /// otherwise its bytes, downloaded from a presigned URL, to upload again
    /// as they are.
    async fn cloned_input(
        &self,
        workspace_id: &str,
        run_id: &str,
        metadata: &Metadata,
    ) -> Result<ClonedInput> {
        let path = api_path(&["v1", "applications", workspace_id, "runs", run_id, "input"])?;
        let json = metadata.format.format_input.input_type == ContentFormat::Json;
        let bytes = match json && metadata.input_size <= MAX_INLINE_SIZE {
            true => self.client.get(path).send().await?.into_bytes(),
            false => self.download(&path).await?.into_bytes(),
        };
        Ok(match serde_json::from_slice(&bytes) {
            Ok(input) if json && bytes.len() as f64 <= MAX_INLINE_SIZE => ClonedInput::Json(input),
            _ => ClonedInput::Bytes(bytes),
        })
    }

    /// Starts `run` with `POST A/runs`, uploading its input first when it
    /// needs it.
    async fn start_run(&self, workspace_id: &str, run: NewRun<'_>) -> Result<RunCreated> {
        let path = api_path(&["v1", "applications", workspace_id, "runs"])?;
        let mut body = RunCreateBody {
            input: None,
            input_id: None,
            upload_id: None,
            name: run.name,
            description: run.description,
            options: run.options,
            configuration: run.configuration,
            tracking: run
                .cloned_run_id
                .map(|cloned_run_id| RunTracking { cloned_run_id }),
            result: run.result,
        };
        let uploaded;
        match run.input {
            BodyInput::Inline(input) => body.input = Some(input),
            BodyInput::Upload(bytes) => {
                let upload = self.create_upload(workspace_id).await?;
                self.put_upload(&upload.upload_url, bytes).await?;
                uploaded = upload.upload_id;
                body.upload_id = Some(&uploaded);
            }
            BodyInput::UploadId(upload_id) => body.upload_id = Some(upload_id),
            BodyInput::ManagedInput(managed_input_id) => body.input_id = Some(managed_input_id),
        }
        let mut request = self.client.post(path).json(&body);
        // The platform ignores the environment of an ensemble run, and uses
        // the workspace's default one when there is none.
        if let Some(environment_id) = non_empty(run.environment_id)
            && !run.ensemble
        {
            request = request.query(&[("instance_id", environment_id)]);
        }
        request.send_json().await
    }
}

/// The fields that set a new run's input, for [`Error::Invalid`].
const INPUT_FIELDS: &str = "`input`, `input_file`, `input_dir`, `managed_input_id` and `upload_id`";

/// The input that a new run's params set.
enum RunInput<'a> {
    /// A JSON object.
    Json(&'a Map<String, Value>),
    /// A JSON file or a tar.gz.
    File(&'a Path),
    /// A directory of input files.
    Dir(&'a Path),
    /// The ID of a managed input.
    ManagedInput(&'a str),
    /// The ID of an upload.
    UploadId(&'a str),
}

/// A run to start with [`Platform::start_run`].
struct NewRun<'a> {
    /// How the request body carries its input.
    input: BodyInput<'a>,
    /// ID of its environment, if any.
    environment_id: Option<&'a str>,
    /// Whether it is an ensemble run, which takes no environment.
    ensemble: bool,
    name: Option<&'a str>,
    description: Option<&'a str>,
    options: &'a BTreeMap<String, String>,
    configuration: &'a RunConfiguration,
    /// ID of the run it is a clone of, if any.
    cloned_run_id: Option<&'a str>,
    /// The result of a run that ran outside the platform, which makes it a
    /// tracked run.
    result: Option<&'a ExternalRunResult>,
}

/// The body of `POST A/runs`: exactly one of `input`, `input_id` and
/// `upload_id`, and always a configuration.
#[derive(Serialize)]
struct RunCreateBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<&'a Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    upload_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    options: &'a BTreeMap<String, String>,
    configuration: &'a RunConfiguration,
    #[serde(skip_serializing_if = "Option::is_none")]
    tracking: Option<RunTracking<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<&'a ExternalRunResult>,
}

/// Where a new run came from: `tracking` in `POST A/runs`.
#[derive(Serialize)]
struct RunTracking<'a> {
    cloned_run_id: &'a str,
}

/// How a new run's request body carries its input.
enum BodyInput<'a> {
    /// Inline, as `input`.
    Inline(&'a Map<String, Value>),
    /// Uploaded first, then as `upload_id`.
    Upload(Vec<u8>),
    /// As `upload_id`, uploaded by the caller.
    UploadId(&'a str),
    /// As `input_id`.
    ManagedInput(&'a str),
}

/// The input of a cloned run, for its clone.
enum ClonedInput {
    /// A JSON object of at most 5 MiB, to send inline.
    Json(Map<String, Value>),
    /// Anything else, to upload again as it is.
    Bytes(Vec<u8>),
}

/// The input fields of a run's params.
struct InputFields<'a> {
    input: Option<&'a Value>,
    input_file: Option<&'a Path>,
    input_dir: Option<&'a Path>,
    managed_input_id: Option<&'a str>,
    upload_id: Option<&'a str>,
}

/// The one input that `fields` set, or `None` if they set none. Empty
/// strings and paths are unset. `rule`, such as `exactly one`, says in the
/// error how many the params take.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if they set more than one, or `input` is not a
/// JSON object.
fn given_input<'a>(fields: InputFields<'a>, rule: &str) -> Result<Option<RunInput<'a>>> {
    let mut inputs = Vec::new();
    if let Some(input) = fields.input {
        let Value::Object(input) = input else {
            return Err(Error::Invalid {
                reason: "`input` must be a JSON object".to_owned(),
            });
        };
        inputs.push(RunInput::Json(input));
    }
    inputs.extend(set(fields.input_file).map(RunInput::File));
    inputs.extend(set(fields.input_dir).map(RunInput::Dir));
    inputs.extend(non_empty(fields.managed_input_id).map(RunInput::ManagedInput));
    inputs.extend(non_empty(fields.upload_id).map(RunInput::UploadId));
    match (inputs.pop(), inputs.is_empty()) {
        (input, true) => Ok(input),
        _ => Err(Error::Invalid {
            reason: format!("set {rule} of {INPUT_FIELDS}"),
        }),
    }
}

/// Reads `source`, before any request, and returns how the request body
/// carries it, with the format it implies: `None` for a managed input,
/// whose format is read later, and for an upload, whose contents the SDK
/// does not see.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if the input file is neither JSON nor a tar.gz
/// or the input directory is not a directory, and [`Error::Io`] if either
/// cannot be read.
async fn load_input(source: RunInput<'_>) -> Result<(BodyInput<'_>, Option<Format>)> {
    Ok(match source {
        RunInput::Json(input) => {
            let bytes = serde_json::to_vec(input).map_err(|error| Error::Encode {
                reason: "`input` is not serializable".to_owned(),
                source: Some(Box::new(error)),
            })?;
            match bytes.len() as f64 > MAX_INLINE_SIZE {
                true => (BodyInput::Upload(bytes), Some(json_format())),
                false => (BodyInput::Inline(input), Some(json_format())),
            }
        }
        RunInput::File(file) => {
            let bytes = read_file(file).await?;
            let format = file_format(&bytes)?;
            (BodyInput::Upload(bytes), Some(format))
        }
        RunInput::Dir(dir) => (
            BodyInput::Upload(pack_dir(dir, "input_dir").await?),
            Some(multi_file_format()),
        ),
        RunInput::ManagedInput(managed_input_id) => {
            (BodyInput::ManagedInput(managed_input_id), None)
        }
        // The format is left to the platform: it takes the format of the
        // environment or the build, and rejects one that differs from the
        // build's.
        RunInput::UploadId(upload_id) => (BodyInput::UploadId(upload_id), None),
    })
}

/// The configuration of a clone of the run with `metadata`: each field that
/// `overrides` sets, or else the cloned run's, with the queuing priority
/// and whether the run skips the queue each on its own.
///
/// The format is left unset when `overrides` has none, for the caller to
/// infer. The cloned run's run type is kept only for standard and ensemble
/// runs: the platform sets the type of tracked runs and ensemble children
/// itself, and rejects them in a new run. A tracked run's `external`
/// execution class is not kept either, so the clone runs on the
/// environment's.
fn clone_configuration(overrides: RunConfiguration, metadata: &Metadata) -> RunConfiguration {
    let RunConfiguration {
        execution_class,
        format,
        run_type,
        secrets_collection_id,
        queuing,
        integration_id,
    } = overrides;
    let queuing = queuing.unwrap_or_default();
    let cloned_type = metadata
        .run_type
        .as_ref()
        .filter(|cloned| matches!(cloned.run_type, Some(RunType::Standard | RunType::Ensemble)));
    let tracked = cloned_type.is_none()
        && metadata
            .run_type
            .as_ref()
            .is_some_and(|cloned| cloned.run_type == Some(RunType::External));
    let cloned_class = non_empty(metadata.execution_class.as_deref()).filter(|_| !tracked);
    let queuing = RunQueuing {
        // The platform stores 0 for a run that had none.
        priority: queuing
            .priority
            .or(metadata.queuing_priority.filter(|&priority| priority != 0)),
        disabled: queuing.disabled.or(metadata.queuing_disabled),
    };
    let execution_class = non_empty(execution_class.as_deref()).or(cloned_class);
    // The cloned run's integration needs the `integration` execution class:
    // with another one, the platform drops it.
    let integration_id = non_empty(integration_id.as_deref())
        .map(str::to_owned)
        .or_else(|| {
            let integration = metadata.integration.as_ref();
            let integration = integration.filter(|_| {
                execution_class.is_none_or(|execution_class| execution_class == "integration")
            });
            integration.map(|integration| integration.integration_id.clone())
        });
    RunConfiguration {
        execution_class: execution_class.map(str::to_owned),
        format,
        run_type: run_type.or_else(|| cloned_type.map(cloned_run_type)),
        secrets_collection_id: non_empty(secrets_collection_id.as_deref())
            .or_else(|| non_empty(metadata.secrets_collection_id.as_deref()))
            .map(str::to_owned),
        queuing: Some(queuing).filter(|queuing| *queuing != RunQueuing::default()),
        integration_id,
    }
}

/// The run type of a clone of a run of type `cloned`: its type and, for an
/// ensemble, its definition, without the empty IDs the platform sends.
fn cloned_run_type(cloned: &RunTypeConfiguration) -> RunTypeConfiguration {
    RunTypeConfiguration {
        run_type: cloned.run_type,
        definition_id: non_empty(cloned.definition_id.as_deref()).map(str::to_owned),
        reference_id: None,
    }
}

/// The managed input of the run with `metadata`, if it ran on one.
fn cloned_managed_input(metadata: &Metadata) -> Option<&str> {
    let tracking = metadata.tracking.as_ref()?;
    non_empty(tracking.input_id.as_deref())
}

/// `format`, unless its input format is one this SDK does not know.
fn known_format(format: &Format) -> Option<Format> {
    let known = |content_format| content_format != ContentFormat::Unknown;
    let output = format.format_output.as_ref();
    let output_known = output.is_none_or(|output| known(output.output_type));
    (known(format.format_input.input_type) && output_known).then(|| format.clone())
}

/// `id`, unless it is unset or empty.
fn non_empty(id: Option<&str>) -> Option<&str> {
    id.filter(|id| !id.is_empty())
}

/// The parts of a tracked run to upload, read and checked before any
/// request.
struct TrackedParts {
    /// The format of the run's input and output.
    format: ContentFormat,
    input: Vec<u8>,
    output: Option<Vec<u8>>,
    logs: Option<Vec<u8>>,
    metrics: Option<Vec<u8>>,
    assets: Option<Vec<u8>>,
}

impl TrackedParts {
    /// Reads the parts of `run`, as [`Platform::track_run`] checks them, in
    /// the content `format`, if set.
    async fn read(run: &TrackedRun, format: Option<ContentFormat>) -> Result<TrackedParts> {
        let input_file = set(run.input_file.as_deref());
        let input_dir = set(run.input_dir_path.as_deref());
        let output_file = set(run.output_file.as_deref());
        let output_dir = set(run.output_dir_path.as_deref());
        let logs_file = set(run.logs_file.as_deref());
        // An empty output is no output.
        let output = run.output.as_ref().filter(|output| !is_empty_json(output));
        let count = |sources: &[bool]| sources.iter().filter(|&&set| set).count();
        let inputs = count(&[
            run.input.is_some(),
            input_file.is_some(),
            input_dir.is_some(),
        ]);
        if inputs != 1 {
            return Err(invalid(
                "set exactly one of `tracked_run.input`, `tracked_run.input_file` and \
                 `tracked_run.input_dir_path`",
            ));
        }
        let outputs = count(&[
            output.is_some(),
            output_file.is_some(),
            output_dir.is_some(),
        ]);
        if outputs > 1 {
            return Err(invalid(
                "set at most one of `tracked_run.output`, `tracked_run.output_file` and \
                 `tracked_run.output_dir_path`",
            ));
        }
        let logs = non_empty(run.logs.as_deref());
        if logs.is_some() && logs_file.is_some() {
            return Err(invalid(
                "set at most one of `tracked_run.logs` and `tracked_run.logs_file`",
            ));
        }
        if outputs == 0 && logs.is_none() && logs_file.is_none() {
            return Err(invalid(
                "set the run's output or its logs: the platform needs one of them",
            ));
        }
        let format = format.unwrap_or(match input_dir {
            Some(_) => ContentFormat::MultiFile,
            None => ContentFormat::Json,
        });
        let multi_file = match format {
            ContentFormat::Json => false,
            ContentFormat::MultiFile => true,
            ContentFormat::Unknown => {
                return Err(invalid("`content_format` must be `json` or `multi-file`"));
            }
        };
        if multi_file != input_dir.is_some() {
            return Err(invalid(match multi_file {
                true => "`multi-file` input is a directory: set `tracked_run.input_dir_path`",
                false => {
                    "a directory of input files needs the `multi-file` content format: set \
                     `content_format`"
                }
            }));
        }
        if outputs == 1 && multi_file != output_dir.is_some() {
            return Err(invalid(match multi_file {
                true => "`multi-file` output is a directory: set `tracked_run.output_dir_path`",
                false => {
                    "a directory of output files needs the `multi-file` content format: set \
                     `content_format`"
                }
            }));
        }

        let input = match (&run.input, input_file, input_dir) {
            (Some(Value::Object(input)), _, _) => json_bytes(input)?,
            (Some(_), _, _) => return Err(invalid("`tracked_run.input` must be a JSON object")),
            (None, Some(file), _) => json_file(file, "tracked_run.input_file", true).await?,
            (None, None, Some(dir)) => pack_dir(dir, "tracked_run.input_dir_path").await?,
            (None, None, None) => unreachable!("exactly one input is set"),
        };
        let output = match (output, output_file, output_dir) {
            (Some(output), _, _) => Some(json_bytes(output)?),
            (None, Some(file), _) => Some(json_file(file, "tracked_run.output_file", false).await?),
            (None, None, Some(dir)) => Some(pack_dir(dir, "tracked_run.output_dir_path").await?),
            (None, None, None) => None,
        };
        let logs = match (logs, logs_file) {
            (Some(logs), _) => Some(logs.as_bytes().to_vec()),
            (None, Some(file)) => Some(text_file(file, "tracked_run.logs_file").await?),
            (None, None) => None,
        };
        // Empty metrics are not uploaded, and empty assets are.
        let metrics = run.metrics.as_ref().filter(|metrics| !metrics.is_empty());
        let metrics = metrics.map(json_bytes).transpose()?;
        let assets = run.assets.as_ref();
        let assets = assets.map(|assets| json_bytes(&json!({ "assets": assets })));
        Ok(TrackedParts {
            format,
            input,
            output,
            logs,
            metrics,
            assets: assets.transpose()?,
        })
    }
}

/// Whether `value` is empty, as Python's `not value` says: `null`, `""`,
/// `[]` or `{}`.
fn is_empty_json(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(fields) => fields.is_empty(),
        Value::Bool(_) | Value::Number(_) => false,
    }
}

/// `value` as compact JSON.
fn json_bytes<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|error| Error::Encode {
        reason: "the tracked run is not serializable".to_owned(),
        source: Some(Box::new(error)),
    })
}

/// The bytes of the JSON file at `path`, the parameter `field`, which holds
/// an object if `object` is set.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if `path` is not a file or does not hold
/// valid JSON or an object, and [`Error::Io`] if it cannot be read.
async fn json_file(path: &Path, field: &str, object: bool) -> Result<Vec<u8>> {
    let bytes = file_bytes(path, field).await?;
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(_)) => Ok(bytes),
        Ok(_) if !object => Ok(bytes),
        Ok(_) => Err(invalid(&format!(
            "`{field}` must hold a JSON object, and `{}` does not",
            path.display()
        ))),
        Err(error) => Err(invalid(&format!(
            "`{field}` must be a JSON file, and `{}` is not one ({error})",
            path.display()
        ))),
    }
}

/// The bytes of the UTF-8 text file at `path`, the parameter `field`.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if `path` is not a file or not UTF-8, and
/// [`Error::Io`] if it cannot be read.
async fn text_file(path: &Path, field: &str) -> Result<Vec<u8>> {
    let bytes = file_bytes(path, field).await?;
    match std::str::from_utf8(&bytes) {
        Ok(_) => Ok(bytes),
        Err(_) => Err(invalid(&format!(
            "`{field}` must be a UTF-8 text file, and `{}` is not one",
            path.display()
        ))),
    }
}

/// The bytes of the file at `path`, the parameter `field`.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if `path` is not a file, such as a directory,
/// and [`Error::Io`] if it cannot be read.
async fn file_bytes(path: &Path, field: &str) -> Result<Vec<u8>> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|source| Error::Io {
            path: path.to_owned(),
            source,
        })?;
    if !metadata.is_file() {
        return Err(invalid(&format!(
            "`{field}` must be a file, and `{}` is not one",
            path.display()
        )));
    }
    read_file(path).await
}

/// An [`Error::Invalid`] for `reason`.
fn invalid(reason: &str) -> Error {
    Error::Invalid {
        reason: reason.to_owned(),
    }
}

/// Checks `configuration` and completes it: the `integration` execution
/// class for an integration, and the ensemble run type for a definition ID.
/// Returns whether it is an ensemble run.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if the priority is not between 1 and 9, the
/// execution class does not fit the integration, the run type does not fit
/// the definition ID, or the run type or a format is `Unknown`.
fn check_configuration(configuration: &mut RunConfiguration) -> Result<bool> {
    let invalid = |reason: &str| Error::Invalid {
        reason: reason.to_owned(),
    };
    if let Some(format) = &configuration.format {
        let output = format.format_output.as_ref();
        if format.format_input.input_type == ContentFormat::Unknown
            || output.is_some_and(|output| output.output_type == ContentFormat::Unknown)
        {
            return Err(invalid(
                "`configuration.format` must be `json` or `multi-file`",
            ));
        }
    }
    let run_type = configuration.run_type.as_ref();
    if run_type.and_then(|run_type| run_type.run_type) == Some(RunType::Unknown) {
        return Err(invalid(
            "`configuration.run_type.type` must be a known run type",
        ));
    }
    let priority = configuration.queuing.as_ref().and_then(|q| q.priority);
    if priority.is_some_and(|priority| !(1..=9).contains(&priority)) {
        return Err(invalid(
            "`configuration.queuing.priority` must be between 1 and 9",
        ));
    }
    if configuration
        .integration_id
        .as_deref()
        .is_some_and(|id| !id.is_empty())
    {
        const INTEGRATION: &str = "integration";
        match configuration.execution_class.as_deref() {
            None | Some("") | Some(INTEGRATION) => {
                configuration.execution_class = Some(INTEGRATION.to_owned());
            }
            Some(_) => {
                return Err(invalid(
                    "with `configuration.integration_id` set, \
                     `configuration.execution_class` must be `integration` or unset",
                ));
            }
        }
    }
    let Some(run_type) = configuration.run_type.as_mut() else {
        return Ok(false);
    };
    let definition = run_type
        .definition_id
        .as_deref()
        .is_some_and(|id| !id.is_empty());
    match (run_type.run_type, definition) {
        (Some(RunType::Ensemble), true) => Ok(true),
        (Some(RunType::Ensemble), false) => Err(invalid(
            "an ensemble run needs `configuration.run_type.definition_id`",
        )),
        (None, true) => {
            run_type.run_type = Some(RunType::Ensemble);
            Ok(true)
        }
        (Some(_), true) => Err(invalid(
            "`configuration.run_type.definition_id` is only for ensemble runs",
        )),
        (_, false) => Ok(false),
    }
}

/// Format of the input file `bytes`: multi-file for a tar.gz, which starts
/// with the gzip magic bytes, and JSON for valid JSON.
///
/// # Errors
///
/// Returns [`Error::Invalid`] for anything else.
fn file_format(bytes: &[u8]) -> Result<Format> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        return Ok(multi_file_format());
    }
    match serde_json::from_slice::<IgnoredAny>(bytes) {
        Ok(_) => Ok(json_format()),
        Err(_) => Err(Error::Invalid {
            reason: "`input_file` must be a JSON file or a tar.gz of input files".to_owned(),
        }),
    }
}

/// JSON input and output.
fn json_format() -> Format {
    symmetric_format(ContentFormat::Json)
}

/// Multi-file input and output.
fn multi_file_format() -> Format {
    symmetric_format(ContentFormat::MultiFile)
}

/// The same content format for the input and the output.
fn symmetric_format(content_format: ContentFormat) -> Format {
    Format {
        format_input: FormatInput {
            input_type: content_format,
        },
        format_output: Some(FormatOutput {
            output_type: content_format,
        }),
    }
}

/// The error for a succeeded run with multi-file output and no `output_dir`.
fn needs_output_dir() -> Error {
    Error::Invalid {
        reason: "the run's output is multi-file: set `output_dir` to download it, \
            or get the run's information, which leaves the output out"
            .to_owned(),
    }
}

/// Content format of a run's output: JSON when the metadata has none, the
/// default.
fn output_format(metadata: &Metadata) -> ContentFormat {
    metadata
        .format
        .format_output
        .as_ref()
        .map_or(ContentFormat::Json, |format| format.output_type)
}

pub(crate) fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "decision_platform_run_get",
            "Gets a run, including its output.",
            |cx, a: RunRef<RunGetParams>| async move {
                if a.params.output_dir.is_some() {
                    cx.local()?;
                }
                cx.platform()?
                    .get_run(&a.workspace_id, &a.run_id, a.params)
                    .await
            },
        )
        .destructive()
        .read_only_in_remote_mode(),
        Tool::new(
            "decision_platform_run_information_get",
            "Gets a run's information and metadata, without its output.",
            |cx, a: RunRef<NoParams>| async move {
                cx.platform()?
                    .get_run_information(&a.workspace_id, &a.run_id)
                    .await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_run_input_get",
            "Gets the input of a run.",
            |cx, a: RunRef<RunInputGetParams>| async move {
                if a.params.output_dir.is_some() {
                    cx.local()?;
                }
                cx.platform()?
                    .get_run_input(&a.workspace_id, &a.run_id, a.params)
                    .await
            },
        )
        .destructive()
        .read_only_in_remote_mode(),
        Tool::new(
            "decision_platform_run_logs_get",
            "Gets the logs of a run.",
            |cx, a: RunRef<RunLogsGetParams>| async move {
                cx.platform()?
                    .get_run_logs(&a.workspace_id, &a.run_id, a.params)
                    .await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_run_list",
            "Lists the runs of a workspace.",
            |cx, a: InWorkspace<RunListParams>| async move {
                cx.platform()?.list_runs(&a.workspace_id, a.params).await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_run_update",
            "Updates a run's name and description.",
            |cx, a: RunRef<RunUpdateParams>| async move {
                cx.platform()?
                    .update_run(&a.workspace_id, &a.run_id, a.params)
                    .await
            },
        )
        .destructive(),
        Tool::new(
            "decision_platform_run_cancel",
            "Cancels a run.",
            |cx, a: RunRef<NoParams>| async move {
                cx.platform()?.cancel_run(&a.workspace_id, &a.run_id).await
            },
        )
        .destructive(),
        Tool::new(
            "decision_platform_run_delete",
            "Deletes a run.",
            |cx, a: RunRef<RunDeleteParams>| async move {
                cx.platform()?
                    .delete_run(&a.workspace_id, &a.run_id, a.params)
                    .await
            },
        )
        .destructive(),
        Tool::new(
            "decision_platform_run_create",
            "Starts a run.",
            |cx, a: InWorkspace<RunCreateParams>| async move {
                if a.params.input_dir.is_some() || a.params.input_file.is_some() {
                    cx.local()?;
                }
                cx.platform()?.create_run(&a.workspace_id, a.params).await
            },
        ),
        Tool::new(
            "decision_platform_run_clone",
            "Starts a run with the input and configuration of an existing run.",
            |cx, a: RunRef<RunCloneParams>| async move {
                if a.params.input_dir.is_some() || a.params.input_file.is_some() {
                    cx.local()?;
                }
                cx.platform()?
                    .clone_run(&a.workspace_id, &a.run_id, a.params)
                    .await
            },
        ),
        Tool::new(
            "decision_platform_run_compare",
            "Compares runs field by field.",
            |cx, a: InWorkspace<RunCompareParams>| async move {
                cx.platform()?.compare_runs(&a.workspace_id, a.params).await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_run_track",
            "Records a run that ran outside the platform.",
            |cx, a: InWorkspace<RunTrackParams>| async move {
                let run = &a.params.tracked_run;
                let paths = [
                    &run.input_file,
                    &run.output_file,
                    &run.logs_file,
                    &run.input_dir_path,
                    &run.output_dir_path,
                ];
                if paths.iter().any(|path| path.is_some()) {
                    cx.local()?;
                }
                cx.platform()?.track_run(&a.workspace_id, a.params).await
            },
        ),
    ]
}
