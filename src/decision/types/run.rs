//! Run types, shared by platform and local runs.
//!
//! Fields use the API's names, except for the decision nouns
//! (`workspace_id`, `environment_id`, `build_id`, `status`) and the fields
//! whose docs give an API name. On the wire they keep the API's names.

use std::collections::BTreeMap;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::de::{Deserializer, IntoDeserializer};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::OrUnknown;

/// Status of a run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum RunStatus {
    /// The run was canceled.
    Canceled,
    /// The run failed.
    Failed,
    /// The run has no status.
    #[default]
    None,
    /// The run is waiting to start.
    Queued,
    /// The run is running.
    Running,
    /// The run succeeded.
    Succeeded,
    /// A value this version of the SDK does not know, read from a response
    /// field. Requests should not use it: tool arguments reject it, and tool
    /// schemas leave it out.
    #[serde(skip_deserializing)]
    #[schemars(skip)]
    Unknown,
}

impl OrUnknown for RunStatus {
    const UNKNOWN: Self = RunStatus::Unknown;
}

impl RunStatus {
    /// Whether a run with this status is done: it succeeded, failed or was
    /// canceled. A status this SDK does not know is not terminal.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RunStatus::Succeeded | RunStatus::Failed | RunStatus::Canceled
        )
    }
}

/// Format of a run's input or output content.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ContentFormat {
    /// A single JSON document.
    #[default]
    Json,
    /// A directory of files.
    MultiFile,
    /// A value this version of the SDK does not know, read from a response
    /// field. Requests should not use it: tool arguments reject it, and tool
    /// schemas leave it out.
    #[serde(skip_deserializing)]
    #[schemars(skip)]
    Unknown,
}

impl OrUnknown for ContentFormat {
    const UNKNOWN: Self = ContentFormat::Unknown;
}

/// Kind of run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum RunType {
    /// A run of the workspace on the platform.
    #[default]
    Standard,
    /// A run that ran outside the platform and was tracked.
    External,
    /// A run of an ensemble definition.
    Ensemble,
    /// A run started by an ensemble run.
    EnsembleChild,
    /// A value this version of the SDK does not know, read from a response
    /// field. Requests should not use it: tool arguments reject it, and tool
    /// schemas leave it out.
    #[serde(skip_deserializing)]
    #[schemars(skip)]
    Unknown,
}

impl OrUnknown for RunType {
    const UNKNOWN: Self = RunType::Unknown;
}

/// Outcome of a tracked run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum TrackedRunStatus {
    /// The run succeeded.
    #[default]
    Succeeded,
    /// The run failed.
    Failed,
}

/// Format of a run's input and output.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Format {
    /// Format of the run's input (API: `input`).
    #[serde(rename = "input")]
    pub format_input: FormatInput,
    /// Format of the run's output (API: `output`).
    #[serde(rename = "output", default, skip_serializing_if = "Option::is_none")]
    pub format_output: Option<FormatOutput>,
}

impl From<ContentFormat> for Format {
    /// A format with `content_format` as the input type and no output type.
    fn from(content_format: ContentFormat) -> Format {
        Format {
            format_input: FormatInput {
                input_type: content_format,
            },
            format_output: None,
        }
    }
}

/// Format of a run's input.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FormatInput {
    /// Content format of the input (API: `type`). Defaults to JSON.
    #[serde(rename = "type", default, deserialize_with = "super::or_unknown")]
    pub input_type: ContentFormat,
}

/// Format of a run's output.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FormatOutput {
    /// Content format of the output (API: `type`). Defaults to JSON.
    #[serde(rename = "type", default, deserialize_with = "super::or_unknown")]
    pub output_type: ContentFormat,
}

/// Kind of a run, and what it refers to, such as an ensemble definition.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunTypeConfiguration {
    /// Kind of run (API: `type`). An empty value reads as unset.
    #[serde(
        rename = "type",
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "run_type_or_none"
    )]
    pub run_type: Option<RunType>,
    /// ID of the definition the run runs, e.g. the ensemble definition of
    /// an ensemble run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_id: Option<String>,
    /// ID of what the run refers to, e.g. the parent run of an ensemble
    /// child run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_id: Option<String>,
}

/// Reads a run type that a response can carry, as `or_unknown` does, with
/// the empty string as no type, which the API sends for runs without one.
fn run_type_or_none<'de, D: Deserializer<'de>>(d: D) -> Result<Option<RunType>, D::Error> {
    match Option::<String>::deserialize(d)? {
        Some(raw) if !raw.is_empty() => {
            super::or_unknown(IntoDeserializer::<D::Error>::into_deserializer(raw)).map(Some)
        }
        _ => Ok(None),
    }
}

/// Queuing settings of a run: its priority, and whether it is queued.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunQueuing {
    /// Priority of the run in the queue, from 1 (the highest) to 9 (the
    /// lowest).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u32>,
    /// Whether the run skips the queue: true runs it without queuing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
}

/// Configuration of a run: execution class, format, secrets, queuing and more.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunConfiguration {
    /// Execution class to run on, i.e. the machine size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_class: Option<String>,
    /// Format of the run's input and output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Format>,
    /// Kind of run, e.g. an ensemble run and its definition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_type: Option<RunTypeConfiguration>,
    /// ID of the secrets collection to give the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets_collection_id: Option<String>,
    /// Queuing settings of the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queuing: Option<RunQueuing>,
    /// ID of the integration to run through. Needs the `integration`
    /// execution class, or none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration_id: Option<String>,
}

/// Result of a run that ran outside the platform, as uploaded artifacts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExternalRunResult {
    /// Upload ID of the run's output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_upload_id: Option<String>,
    /// Upload ID of the run's error output, i.e. its logs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_upload_id: Option<String>,
    /// Whether the run succeeded or failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TrackedRunStatus>,
    /// Error message of the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// How long the run took, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_duration: Option<u64>,
    /// Upload ID of the run's metrics, for multi-file output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_upload_id: Option<String>,
    /// Upload ID of the run's assets, for multi-file output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assets_upload_id: Option<String>,
}

/// A run that ran outside the platform, to record with
/// `Platform::track_run`.
///
/// Its input is exactly one of `input`, `input_file` and `input_dir_path`.
/// Its output is at most one of `output`, `output_file` and
/// `output_dir_path`, and its logs at most one of `logs` and `logs_file`;
/// it needs an output or logs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TrackedRun {
    /// Whether the run succeeded or failed.
    pub status: TrackedRunStatus,
    /// Input of the run, as a JSON object, for JSON input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    /// Output of the run, as JSON, for JSON output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    /// How long the run took, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
    /// Error message of the run. Only for runs that failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Logs of the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs: Option<String>,
    /// Name of the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Description of the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON file whose contents are the run's input, a JSON object, for JSON
    /// input. Not available in remote mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_file: Option<PathBuf>,
    /// JSON file whose contents are the run's output, for JSON output. Not
    /// available in remote mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_file: Option<PathBuf>,
    /// UTF-8 text file whose contents are the run's logs. Not available in
    /// remote mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs_file: Option<PathBuf>,
    /// Directory whose files are the run's input, uploaded as an archive,
    /// for multi-file input. Not available in remote mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_dir_path: Option<PathBuf>,
    /// Directory whose files are the run's output, uploaded as an archive,
    /// for multi-file output. Not available in remote mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_dir_path: Option<PathBuf>,
    /// Metrics of the run, for any output format. When they are set, the
    /// platform keeps them, and not the `metrics` of a JSON `output`. When
    /// they are not, it reads a JSON output's own `metrics`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Map<String, Value>>,
    /// Assets of the run, for multi-file output. The platform reads a JSON
    /// output's assets from its `assets`, and ignores these.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assets: Option<Vec<Value>>,
}

/// A run, as run lists return it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Run {
    /// ID of the run.
    pub id: String,
    /// Email of the user who started the run.
    pub user_email: String,
    /// Name of the run.
    pub name: String,
    /// Description of the run.
    pub description: String,
    /// When the run was created, in RFC 3339.
    pub created_at: String,
    /// ID of the workspace of the run (API: `application_id`).
    #[serde(rename = "application_id")]
    pub workspace_id: String,
    /// ID of the environment the run ran in (API:
    /// `application_instance_id`).
    #[serde(rename = "application_instance_id")]
    pub environment_id: String,
    /// ID of the build the run ran (API: `application_version_id`).
    #[serde(rename = "application_version_id")]
    pub build_id: String,
    /// Kind of run.
    pub run_type: RunTypeConfiguration,
    /// Execution class the run ran on.
    pub execution_class: String,
    /// Runtime the run ran in.
    pub runtime: String,
    /// Status of the run (API: `status_v2`).
    #[serde(rename = "status_v2", deserialize_with = "super::or_unknown")]
    pub status: RunStatus,
    /// Priority of the run in the queue, from 1 (the highest) to 9 (the
    /// lowest).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queuing_priority: Option<u32>,
    /// Whether the run skipped the queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queuing_disabled: Option<bool>,
    /// ID of the experiment the run belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment_id: Option<String>,
    /// Metrics of the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<RunInfoMetrics>,
    /// ID of the managed input the run used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_id: Option<String>,
    /// Option set of the run, in an experiment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_set: Option<String>,
    /// Options the run ran with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<BTreeMap<String, String>>,
    /// Options the run was requested with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_options: Option<BTreeMap<String, String>>,
    /// Options the run ran with, and where each value came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options_summary: Option<Vec<OptionsSummaryItem>>,
    /// The switchback test that routed the run, and the unit of its plan
    /// the run fell in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switchback_experiment: Option<SwitchbackExperimentRef>,
    /// ID of the scenario the run belongs to, in a scenario test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    /// Repetition number of the run, in a scenario test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repetition: Option<u32>,
    /// ID of the input set the run's input came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_set_id: Option<String>,
}

/// The switchback test that routed a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct SwitchbackExperimentRef {
    /// ID of the switchback test.
    pub experiment_id: String,
    /// Index of the unit of the test's plan that the run fell in.
    pub plan_unit_index: u32,
}

/// An asset of a platform run, as its list and get return it: its metadata,
/// not its content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RunAsset {
    /// ID of the asset, a UUID (API: `id`).
    #[serde(rename = "id")]
    pub asset_id: String,
    /// Name of the asset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Description of the asset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Content type of the asset, e.g. `json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// How the console shows the asset, for a visual.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visual: Option<RunAssetVisual>,
    /// Why the platform could not store the asset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RunAssetError>,
    /// When the asset was created, in RFC 3339.
    pub created_at: String,
    /// Size of the asset's content, in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Presigned URL of the asset's content. Only a get has it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,
}

/// How the console shows a visual asset of a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RunAssetVisual {
    /// Format of the visual's content: `plotly`, `geojson`, `chartjs` or
    /// `json`.
    pub schema: String,
    /// Where the console shows the visual, e.g. `custom-tab`.
    #[serde(rename = "type")]
    pub visual_type: String,
    /// Label of the visual in the console.
    pub label: String,
    /// The view the console shows the visual in, if not its default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_override: Option<String>,
    /// Position of the visual's tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_order: Option<i32>,
    /// Position of the visual in a grid layout, from 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<i32>,
}

/// Why the platform could not store an asset of a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RunAssetError {
    /// The platform's code for the error.
    pub code: String,
}

/// An asset of a platform run with its content, as
/// [`Platform::get_run_asset`](crate::decision::platform::Platform::get_run_asset)
/// returns it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RunAssetContent {
    /// The asset.
    pub asset: RunAsset,
    /// The asset's content, for a JSON asset that was not written to a file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
}

/// A named value among a run's metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct MetricsIndicator {
    /// Name of the metric.
    pub name: String,
    /// Value of the metric.
    pub value: Value,
}

/// Summary of a run's metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RunInfoMetrics {
    /// Status of the metrics, e.g. whether they could be read.
    pub status: String,
    /// Why the metrics could not be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The metrics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indicators: Option<Vec<MetricsIndicator>>,
}

/// An option of a run, its value and where the value came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct OptionsSummaryItem {
    /// Name of the option.
    pub name: String,
    /// Value of the option.
    pub value: Value,
    /// Where the value came from: the run, its environment or its build.
    pub source: String,
}

/// Where a run came from, such as the run it was cloned from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RunTrackingMetadata {
    /// ID of the run this run was cloned from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloned_run_id: Option<String>,
    /// ID of the managed input the run used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_id: Option<String>,
}

/// An option of a run, its value and where the value came from, in the run's
/// metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct OptionSummary {
    /// Name of the option.
    pub name: String,
    /// Where the value came from: the run, its environment or its build.
    pub source: String,
    /// Value the option had when the run ran.
    pub value: String,
}

/// Options of a run: the ones it was requested with and the ones it ran with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RunOptions {
    /// Options the run ran with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_options: Option<Map<String, Value>>,
    /// Options the run ran with, and where each value came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options_summary: Option<Vec<OptionSummary>>,
    /// Options the run was requested with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_options: Option<Map<String, Value>>,
}

/// The integration a run ran through.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RunIntegration {
    /// ID of the integration (API: `id`).
    #[serde(rename = "id")]
    pub integration_id: String,
    /// Provider of the integration.
    pub provider: String,
    /// ID of the run in the integration's system.
    pub run_id: String,
    /// More about the run in the integration's system.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// Metadata of a run: status, timing, format, sizes and options.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Metadata {
    /// ID of the workspace of the run (API: `application_id`).
    #[serde(rename = "application_id")]
    pub workspace_id: String,
    /// ID of the environment the run ran in (API:
    /// `application_instance_id`).
    #[serde(rename = "application_instance_id")]
    pub environment_id: String,
    /// ID of the build the run ran (API: `application_version_id`).
    #[serde(rename = "application_version_id")]
    pub build_id: String,
    /// When the run was created, in RFC 3339.
    pub created_at: String,
    /// How long the run took, in milliseconds.
    pub duration: f64,
    /// Error message of the run, empty if it did not fail.
    pub error: String,
    /// Execution class the run ran on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_class: Option<String>,
    /// How long the run's execution took, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_duration: Option<f64>,
    /// ID of the experiment the run belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment_id: Option<String>,
    /// Kind of the experiment the run belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment_type: Option<String>,
    /// Format of the run's input and output.
    pub format: Format,
    /// When the run started, in RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initiated_at: Option<String>,
    /// Size of the run's input, in bytes.
    pub input_size: f64,
    /// The integration the run ran through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration: Option<RunIntegration>,
    /// Metrics of the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Map<String, Value>>,
    /// Options of the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<RunOptions>,
    /// Size of the run's output, in bytes.
    pub output_size: f64,
    /// Whether the run skipped the queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queuing_disabled: Option<bool>,
    /// Priority of the run in the queue, from 1 (the highest) to 9 (the
    /// lowest).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queuing_priority: Option<u32>,
    /// Kind of run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_type: Option<RunTypeConfiguration>,
    /// Runtime the run ran in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// ID of the secrets collection the run got.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets_collection_id: Option<String>,
    /// Status of the run (API: `status_v2`).
    #[serde(rename = "status_v2", deserialize_with = "super::or_unknown")]
    pub status: RunStatus,
    /// Where the run came from, such as the run it was cloned from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking: Option<RunTrackingMetadata>,
}

/// Record of a local run that was synced to the platform.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct SyncedRun {
    /// ID of the platform run the local run was synced to.
    pub run_id: String,
    /// When the run was synced, in RFC 3339.
    pub synced_at: String,
    /// ID of the platform workspace the run was synced to (API: `app_id`).
    #[serde(rename = "app_id")]
    pub workspace_id: String,
    /// ID of the environment the run was synced to, if not the workspace's
    /// default (API: `instance_id`).
    #[serde(
        rename = "instance_id",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub environment_id: Option<String>,
}

/// A run's information and metadata, without its output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RunInformation {
    /// Description of the run.
    pub description: String,
    /// ID of the run.
    pub id: String,
    /// Metadata of the run.
    pub metadata: Metadata,
    /// Name of the run.
    pub name: String,
    /// Email of the user who started the run.
    pub user_email: String,
    /// URL of the run in the console, empty if unknown.
    #[serde(default)]
    pub console_url: String,
    /// The platform runs a local run was synced to, one per workspace and
    /// environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced_runs: Option<Vec<SyncedRun>>,
}

/// Error output of a failed run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ErrorLog {
    /// Error message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// What the run wrote to stdout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    /// What the run wrote to stderr.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

/// A run's information, metadata and output.
///
/// On the wire it is one object: the fields of `information` plus the
/// output fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RunResult {
    /// Information and metadata of the run.
    #[serde(flatten)]
    pub information: RunInformation,
    /// Error output of the run, if it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_log: Option<ErrorLog>,
    /// Output of the run, if it succeeded and its output is returned
    /// inline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Map<String, Value>>,
    /// Result of the ensemble, for an ensemble run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ensemble: Option<Map<String, Value>>,
}

/// Logs of a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RunLog {
    /// What the run wrote to stderr.
    pub log: String,
}

/// One timestamped line of a platform run's logs, as following them
/// returns it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct TimestampedRunLog {
    /// When the run wrote the line (RFC 3339).
    pub timestamp: String,
    /// The line.
    pub log: String,
}

/// Values of one field across compared runs, and whether they differ.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ComparisonValues {
    /// Whether the runs have different values.
    #[serde(default)]
    pub has_differences: bool,
    /// The value of each run, by run ID.
    #[serde(default)]
    pub values: Map<String, Value>,
}

/// Field-by-field comparison of runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RunComparison {
    /// IDs of the compared runs.
    pub run_ids: Vec<String>,
    /// Comparison of the runs' information fields, by field name.
    pub information: BTreeMap<String, ComparisonValues>,
    /// Comparison of the runs' metadata fields, by field name.
    pub metadata: BTreeMap<String, ComparisonValues>,
    /// Comparison of the runs' metrics, by metric name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<BTreeMap<String, ComparisonValues>>,
    /// Comparison of the runs' options, by option name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<BTreeMap<String, ComparisonValues>>,
}

/// ID of a run that was just started.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RunCreated {
    /// ID of the run.
    pub run_id: String,
    /// Whether starting the run registered its local workspace, which was
    /// not registered before. Always `false` for platform runs, and left out
    /// of the JSON when `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub workspace_registered: bool,
}

/// What a sync of a local workspace to the platform did, run by run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct WorkspaceSynced {
    /// The outcome of each local run the sync looked at, in the order it
    /// looked at them.
    pub runs: Vec<RunSyncResult>,
    /// Whether the sync registered its local workspace, which was not
    /// registered before. Left out of the JSON when `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub workspace_registered: bool,
}

/// The outcome of syncing one local run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RunSyncResult {
    /// ID of the local run.
    pub run_id: String,
    /// What the sync did with it.
    #[serde(flatten)]
    pub outcome: RunSyncOutcome,
}

/// What syncing a local run did. Serialized, its `outcome` key is the
/// variant's name in snake case, next to the variant's fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "outcome", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RunSyncOutcome {
    /// The run was tracked on the platform now.
    #[non_exhaustive]
    Synced {
        /// The record the sync added to the run's information.
        synced_run: SyncedRun,
    },
    /// The run was synced to this workspace, and environment, before, so
    /// it was not tracked again.
    #[non_exhaustive]
    AlreadySynced {
        /// The record of the earlier sync.
        synced_run: SyncedRun,
    },
    /// The run was not synced, for example because it has not ended or its
    /// input is gone.
    #[non_exhaustive]
    Skipped {
        /// Why the run was not synced.
        reason: String,
    },
}

/// Presigned URL to download a large run output or input from.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct DownloadUrl {
    /// The presigned URL. It needs no authorization.
    pub(crate) url: String,
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use serde::de::DeserializeOwned;
    use serde_json::json;

    use super::*;
    use crate::tool::{Context, Tool, ToolError};

    fn read<T: DeserializeOwned>(wire: Value) -> T {
        serde_json::from_value(wire).unwrap()
    }

    /// `value` serializes to `wire`, and `wire` reads back as `value`.
    fn assert_round_trip<T: Serialize + DeserializeOwned + PartialEq + Debug>(
        value: T,
        wire: Value,
    ) {
        assert_eq!(serde_json::to_value(&value).unwrap(), wire);
        assert_eq!(read::<T>(wire), value);
    }

    /// Metadata of a succeeded run, as `GET A/runs/{id}/metadata` returns
    /// it, with the legacy `status` next to `status_v2`.
    fn metadata_wire() -> Value {
        json!({
            "status": "succeeded",
            "status_v2": "succeeded",
            "created_at": "2026-10-07T14:19:02.123456789Z",
            "initiated_at": "2026-10-07T14:19:03Z",
            "duration": 5021,
            "execution_duration": 4890,
            "input_size": 1024,
            "output_size": 2048,
            "error": "",
            "application_id": "routing",
            "application_instance_id": "production",
            "application_version_id": "v1",
            "secrets_collection_id": "secrets-1",
            "execution_class": "6c9500mb870s",
            "runtime": "ghcr.io/nextmv-io/runtime/python:3.11",
            "run_type": { "type": "standard", "definition_id": "", "reference_id": "" },
            "format": { "input": { "type": "json" }, "output": { "type": "json" } },
            "options": {
                "active_options": { "duration": "30s" },
                "request_options": { "duration": "30s" },
                "options_summary": [{ "name": "duration", "value": "30s", "source": "run" }]
            },
            "queuing_priority": 3,
            "queuing_disabled": false,
            "statistics": { "schema": "v1" },
            "metrics": { "cost": 1250.75, "optimal": true },
            "integration": {
                "id": "dbx",
                "provider": "databricks",
                "run_id": "123",
                "details": { "job": 7 }
            },
            "tracking": { "cloned_run_id": "run-0", "input_id": "input-1" }
        })
    }

    #[test]
    fn reads_run_metadata() {
        let info: RunInformation = read(json!({
            "id": "run-1",
            "user_email": "ana@example.com",
            "name": "Monday",
            "description": "",
            "metadata": metadata_wire(),
        }));
        assert_eq!(info.id, "run-1");
        assert_eq!(info.console_url, "");
        assert_eq!(info.synced_runs, None);
        let metadata = &info.metadata;
        assert_eq!(metadata.workspace_id, "routing");
        assert_eq!(metadata.environment_id, "production");
        assert_eq!(metadata.build_id, "v1");
        assert_eq!(metadata.status, RunStatus::Succeeded);
        assert_eq!(metadata.duration, 5021.0);
        assert_eq!(metadata.execution_duration, Some(4890.0));
        assert_eq!(metadata.output_size, 2048.0);
        assert_eq!(
            metadata.format.format_output,
            Some(FormatOutput {
                output_type: ContentFormat::Json
            })
        );
        let run_type = metadata.run_type.as_ref().unwrap();
        assert_eq!(run_type.run_type, Some(RunType::Standard));
        assert_eq!(run_type.definition_id.as_deref(), Some(""));
        let options = metadata.options.as_ref().unwrap();
        assert_eq!(options.options_summary.as_ref().unwrap()[0].source, "run");
        assert_eq!(metadata.metrics.as_ref().unwrap()["optimal"], json!(true));
        let integration = metadata.integration.as_ref().unwrap();
        assert_eq!(integration.integration_id, "dbx");
        let tracking = metadata.tracking.as_ref().unwrap();
        assert_eq!(tracking.cloned_run_id.as_deref(), Some("run-0"));
        // Emitted JSON keeps the API names, without the legacy `status` and
        // the statistics, which FICO does not support.
        let wire = serde_json::to_value(metadata).unwrap();
        assert_eq!(wire["application_instance_id"], "production");
        assert_eq!(wire["status_v2"], "succeeded");
        assert_eq!(wire["integration"]["id"], "dbx");
        assert!(wire.get("status").is_none() && wire.get("statistics").is_none());
    }

    #[test]
    fn reads_a_run_list_item() {
        let run: Run = read(json!({
            "id": "run-1",
            "user_email": "ana@example.com",
            "name": "Monday",
            "description": "Weekly plan",
            "created_at": "2026-10-07T14:19:02Z",
            "application_id": "routing",
            "application_instance_id": "production",
            "application_version_id": "v1",
            "run_type": { "type": "", "definition_id": "", "reference_id": "" },
            "execution_class": "6c9500mb870s",
            "runtime": "ghcr.io/nextmv-io/runtime/default:latest",
            "status": "running",
            "status_v2": "running",
            "experiment_id": "bench",
            "metrics": {
                "status": "succeeded",
                "indicators": [{ "name": "cost", "value": 12.5 }]
            },
            "input_id": "input-1",
            "option_set": "set-1",
            "options": { "duration": "30s" },
            "request_options": { "duration": "30s" },
            "options_summary": [{ "name": "duration", "value": "30s", "source": "instance" }],
            "switchback_experiment": { "experiment_id": "sw", "plan_unit_index": 2 },
            "scenario_id": "rainy",
            "repetition": 1,
            "input_set_id": "set"
        }));
        assert_eq!(run.workspace_id, "routing");
        assert_eq!(run.environment_id, "production");
        assert_eq!(run.build_id, "v1");
        assert_eq!(run.status, RunStatus::Running);
        assert_eq!(run.run_type.run_type, None);
        let metrics = run.metrics.as_ref().unwrap();
        assert_eq!(metrics.indicators.as_ref().unwrap()[0].value, json!(12.5));
        assert_eq!(run.options.as_ref().unwrap()["duration"], "30s");
        assert_eq!(run.options_summary.as_ref().unwrap()[0].value, json!("30s"));
        assert_eq!(run.repetition, Some(1));
        let switchback = run.switchback_experiment.as_ref().unwrap();
        assert_eq!(switchback.experiment_id, "sw");
        assert_eq!(switchback.plan_unit_index, 2);
    }

    #[test]
    fn reads_values_it_does_not_know_as_unknown() {
        let mut metadata = metadata_wire();
        metadata["status_v2"] = json!("paused");
        metadata["run_type"]["type"] = json!("pipeline");
        metadata["format"]["input"]["type"] = json!("csv-archive");
        let metadata: Metadata = read(metadata);
        assert_eq!(metadata.status, RunStatus::Unknown);
        assert_eq!(metadata.run_type.unwrap().run_type, Some(RunType::Unknown));
        assert_eq!(
            metadata.format.format_input.input_type,
            ContentFormat::Unknown
        );
    }

    #[test]
    fn reads_a_run_result() {
        let wire = json!({
            "id": "run-1",
            "user_email": "ana@example.com",
            "name": "Monday",
            "description": "",
            "metadata": metadata_wire(),
            "output": { "solutions": [{ "routes": [] }], "metrics": { "cost": 12.5 } },
            "error_log": { "error": "", "stdout": "", "stderr": "solving" },
            "ensemble": { "best_run_id": "run-2" }
        });
        let result: RunResult = read(wire);
        assert_eq!(result.information.id, "run-1");
        assert_eq!(result.information.metadata.status, RunStatus::Succeeded);
        assert_eq!(
            result.output.as_ref().unwrap()["metrics"]["cost"],
            json!(12.5)
        );
        assert_eq!(
            result.error_log.as_ref().unwrap().stderr.as_deref(),
            Some("solving")
        );
        assert_eq!(result.ensemble.as_ref().unwrap()["best_run_id"], "run-2");
        // The information is flattened back into one object.
        let emitted = serde_json::to_value(&result).unwrap();
        assert_eq!(emitted["id"], "run-1");
        assert_eq!(emitted["metadata"]["status_v2"], "succeeded");
        assert_eq!(emitted["error_log"]["stderr"], "solving");
        assert!(emitted.get("information").is_none());
        assert_eq!(read::<RunResult>(emitted), result);
    }

    #[test]
    fn reads_a_failed_run_result_without_output() {
        let mut metadata = metadata_wire();
        metadata["status_v2"] = json!("failed");
        metadata["error"] = json!("exit status 1");
        let result: RunResult = read(json!({
            "id": "run-1",
            "user_email": "",
            "name": "",
            "description": "",
            "metadata": metadata,
            "error_log": { "error": "exit status 1", "stdout": "", "stderr": "panic" }
        }));
        assert!(result.information.metadata.status.is_terminal());
        assert_eq!(result.output, None);
        assert_eq!(result.ensemble, None);
    }

    /// A local run's `<run_id>.json`, as local runs write it.
    #[test]
    fn round_trips_a_local_run_file() {
        let wire = json!({
            "description": "Local run created at 2026-10-07T14:19:02.123456Z",
            "id": "local-3k9x0q2a",
            "metadata": {
                "application_id": "routing",
                "application_instance_id": "",
                "application_version_id": "",
                "created_at": "2026-10-07T14:19:02.123456Z",
                "duration": 1234.5,
                "error": "",
                "execution_class": "local",
                "execution_duration": 1200.0,
                "format": { "input": { "type": "multi-file" }, "output": { "type": "multi-file" } },
                "initiated_at": "2026-10-07T14:19:02.123456Z",
                "input_size": 0.0,
                "output_size": 0.0,
                "queuing_disabled": true,
                "queuing_priority": 0,
                "run_type": { "type": "standard", "definition_id": "", "reference_id": "" },
                "runtime": "local",
                "status_v2": "succeeded",
                "tracking": { "cloned_run_id": "", "input_id": "" }
            },
            "name": "local run local-3k9x0q2a",
            "user_email": "",
            "console_url": "",
            "synced_runs": [
                {
                    "run_id": "run-9",
                    "synced_at": "2026-10-08T09:00:00.000000Z",
                    "app_id": "routing",
                    "instance_id": "staging"
                },
                {
                    "run_id": "run-10",
                    "synced_at": "2026-10-08T09:05:00.000000Z",
                    "app_id": "routing-copy"
                }
            ]
        });
        let info: RunInformation = read(wire.clone());
        let synced = info.synced_runs.as_ref().unwrap();
        assert_eq!(synced[0].workspace_id, "routing");
        assert_eq!(synced[0].environment_id.as_deref(), Some("staging"));
        assert_eq!(synced[1].environment_id, None);
        assert_eq!(
            info.metadata.format.format_input.input_type,
            ContentFormat::MultiFile
        );
        assert_eq!(serde_json::to_value(&info).unwrap(), wire);
    }

    #[test]
    fn reads_small_responses() {
        let log: RunLog = read(json!({ "log": "solving\ndone\n" }));
        assert_eq!(log.log, "solving\ndone\n");
        let entry: TimestampedRunLog = read(json!({
            "timestamp": "2026-10-07T14:19:02.5Z",
            "log": "solving",
        }));
        assert_eq!(entry.timestamp, "2026-10-07T14:19:02.5Z");
        assert_eq!(entry.log, "solving");
        let created: RunCreated = read(json!({ "run_id": "run-1" }));
        assert_eq!(created.run_id, "run-1");
        assert!(!created.workspace_registered);
        assert_eq!(
            serde_json::to_value(&created).unwrap(),
            json!({ "run_id": "run-1" })
        );
        let registered = json!({ "run_id": "local-1", "workspace_registered": true });
        let created: RunCreated = read(registered.clone());
        assert!(created.workspace_registered);
        assert_eq!(serde_json::to_value(&created).unwrap(), registered);
        let url: DownloadUrl = read(json!({ "url": "https://bucket.example.com/out?sig=x" }));
        assert_eq!(url.url, "https://bucket.example.com/out?sig=x");
        let errors: ErrorLog = read(json!({}));
        assert_eq!(errors.error, None);
    }

    #[test]
    fn round_trips_a_workspace_sync() {
        let synced_run = SyncedRun {
            run_id: "run-9".to_owned(),
            synced_at: "2026-10-08T09:00:00.000000Z".to_owned(),
            workspace_id: "routing".to_owned(),
            environment_id: None,
        };
        let synced = WorkspaceSynced {
            runs: vec![
                RunSyncResult {
                    run_id: "local-1".to_owned(),
                    outcome: RunSyncOutcome::Synced {
                        synced_run: synced_run.clone(),
                    },
                },
                RunSyncResult {
                    run_id: "local-2".to_owned(),
                    outcome: RunSyncOutcome::AlreadySynced { synced_run },
                },
                RunSyncResult {
                    run_id: "local-3".to_owned(),
                    outcome: RunSyncOutcome::Skipped {
                        reason: "it is running".to_owned(),
                    },
                },
            ],
            workspace_registered: false,
        };
        let record = json!({
            "run_id": "run-9",
            "synced_at": "2026-10-08T09:00:00.000000Z",
            "app_id": "routing",
        });
        assert_round_trip(
            synced,
            json!({
                "runs": [
                    { "run_id": "local-1", "outcome": "synced", "synced_run": record },
                    { "run_id": "local-2", "outcome": "already_synced", "synced_run": record },
                    { "run_id": "local-3", "outcome": "skipped", "reason": "it is running" },
                ],
            }),
        );
        let registered = json!({ "runs": [], "workspace_registered": true });
        let synced: WorkspaceSynced = read(registered.clone());
        assert!(synced.workspace_registered);
        assert_eq!(serde_json::to_value(&synced).unwrap(), registered);
    }

    #[test]
    fn round_trips_a_run_comparison() {
        let comparison: RunComparison = read(json!({
            "run_ids": ["run-1", "run-2"],
            "information": {
                "name": { "has_differences": true, "values": { "run-1": "a", "run-2": "b" } }
            },
            "metadata": { "execution_class": { "values": { "run-1": "s", "run-2": "s" } } },
            "metrics": { "cost": { "has_differences": true, "values": { "run-1": 1, "run-2": 2 } } }
        }));
        assert!(comparison.information["name"].has_differences);
        assert!(!comparison.metadata["execution_class"].has_differences);
        assert_eq!(comparison.options, None);
        let emitted = serde_json::to_value(&comparison).unwrap();
        assert_eq!(
            emitted["metadata"]["execution_class"]["has_differences"],
            false
        );
        assert_eq!(read::<RunComparison>(emitted), comparison);
    }

    #[test]
    fn round_trips_input_types_with_api_names() {
        assert_round_trip(
            Format {
                format_input: FormatInput {
                    input_type: ContentFormat::MultiFile,
                },
                format_output: Some(FormatOutput::default()),
            },
            json!({ "input": { "type": "multi-file" }, "output": { "type": "json" } }),
        );
        assert_eq!(read::<Format>(json!({ "input": {} })), Format::default());
        assert_round_trip(
            RunConfiguration {
                execution_class: Some("integration".to_owned()),
                format: Some(Format::default()),
                run_type: Some(RunTypeConfiguration {
                    run_type: Some(RunType::Ensemble),
                    definition_id: Some("ens-1".to_owned()),
                    reference_id: None,
                }),
                secrets_collection_id: Some("secrets-1".to_owned()),
                queuing: Some(RunQueuing {
                    priority: Some(1),
                    disabled: Some(false),
                }),
                integration_id: Some("dbx".to_owned()),
            },
            json!({
                "execution_class": "integration",
                "format": { "input": { "type": "json" } },
                "run_type": { "type": "ensemble", "definition_id": "ens-1" },
                "secrets_collection_id": "secrets-1",
                "queuing": { "priority": 1, "disabled": false },
                "integration_id": "dbx"
            }),
        );
        assert_round_trip(RunConfiguration::default(), json!({}));
        assert_round_trip(
            ExternalRunResult {
                output_upload_id: Some("up-1".to_owned()),
                error_upload_id: Some("up-2".to_owned()),
                status: Some(TrackedRunStatus::Failed),
                error_message: Some("infeasible".to_owned()),
                execution_duration: Some(2_000),
                metrics_upload_id: Some("up-3".to_owned()),
                assets_upload_id: Some("up-4".to_owned()),
            },
            json!({
                "output_upload_id": "up-1",
                "error_upload_id": "up-2",
                "status": "failed",
                "error_message": "infeasible",
                "execution_duration": 2000,
                "metrics_upload_id": "up-3",
                "assets_upload_id": "up-4"
            }),
        );
        assert_round_trip(
            TrackedRun {
                status: TrackedRunStatus::Succeeded,
                input: Some(json!({ "vehicles": 5 })),
                output: Some(json!("routes")),
                duration: Some(5_000),
                error: None,
                logs: Some("solving\ndone".to_owned()),
                name: Some("test-run".to_owned()),
                description: Some("A test run".to_owned()),
                input_file: Some(PathBuf::from("input.json")),
                output_file: Some(PathBuf::from("output.json")),
                logs_file: Some(PathBuf::from("logs.txt")),
                input_dir_path: Some(PathBuf::from("inputs")),
                output_dir_path: Some(PathBuf::from("outputs")),
                metrics: Some(Map::from_iter([("cost".to_owned(), json!(12.5))])),
                assets: Some(vec![json!({ "name": "map" })]),
            },
            json!({
                "status": "succeeded",
                "input": { "vehicles": 5 },
                "output": "routes",
                "duration": 5000,
                "logs": "solving\ndone",
                "name": "test-run",
                "description": "A test run",
                "input_file": "input.json",
                "output_file": "output.json",
                "logs_file": "logs.txt",
                "input_dir_path": "inputs",
                "output_dir_path": "outputs",
                "metrics": { "cost": 12.5 },
                "assets": [{ "name": "map" }]
            }),
        );
        assert_round_trip(TrackedRun::default(), json!({ "status": "succeeded" }));
    }

    #[test]
    fn says_which_run_statuses_are_terminal() {
        let terminal = [RunStatus::Succeeded, RunStatus::Failed, RunStatus::Canceled];
        let not_terminal = [
            RunStatus::None,
            RunStatus::Queued,
            RunStatus::Running,
            RunStatus::Unknown,
        ];
        assert!(terminal.into_iter().all(RunStatus::is_terminal));
        assert!(!not_terminal.into_iter().any(RunStatus::is_terminal));
    }

    /// Arguments of a test tool that takes a run configuration.
    #[derive(Debug, Deserialize, JsonSchema)]
    struct CreateParams {
        /// Configuration of the run.
        configuration: RunConfiguration,
    }

    #[tokio::test]
    async fn reads_run_configurations_strictly_in_tool_arguments() {
        let tool = Tool::new(
            "create",
            "Creates a run.",
            |_cx, a: CreateParams| async move { Ok(a.configuration) },
        );
        let ok = tool
            .call(
                Context::new(),
                json!({ "configuration": { "run_type": { "type": "" } } }),
            )
            .await;
        assert_eq!(ok.unwrap(), json!({ "run_type": {} }));
        for configuration in [
            json!({ "run_type": { "type": "pipeline" } }),
            json!({ "format": { "input": { "type": "csv-archive" } } }),
            json!({ "queuing": { "priority": 1, "disable": true } }),
        ] {
            let err = tool
                .call(Context::new(), json!({ "configuration": configuration }))
                .await
                .unwrap_err();
            assert!(matches!(err, ToolError::InvalidArguments(_)), "{err}");
        }
    }

    #[test]
    fn builds_a_format_from_a_content_format() {
        let format = Format::from(ContentFormat::MultiFile);
        assert_round_trip(format, json!({ "input": { "type": "multi-file" } }));
    }
}
