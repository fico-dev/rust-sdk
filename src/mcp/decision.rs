//! The curated `decision_*` tools: the workflows that the default surface
//! advertises over the decision operation catalog, each at one risk level.
//! The operation tools reach everything else.
//!
//! Each tool calls the SDK functions that do its work, and adds only the
//! response handling: concise results with names next to IDs (`views`),
//! heavy parts on request with `include` and large content in the cache or
//! behind a resource link (`parts`, `cache`), lists capped by `limit`,
//! bounded waits that return a hint instead of failing, errors that say
//! what does exist, and blank strings read as absent, as models often send
//! `""` for a field they mean to leave out.

mod parts;
#[cfg(test)]
mod tests;
mod views;

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use self::views::{Names, object, wire};
use crate::decision::compare::{check_run_ids, compare};
use crate::decision::local::{self as local_ops, Local, WorkspaceSyncParams};
use crate::decision::platform::{
    self as platform_ops, AcceptanceCreateParams, AcceptanceGetParams, InputSetCreateParams,
    Platform, RunListParams, ScenarioCreateParams, ScenarioGetParams,
};
use crate::decision::polling::Poller;
use crate::decision::{
    ExperimentStatus, LocalWorkspaceRef, Metric, PollingOptions, RunInformation, RunStatus,
    RunSyncOutcome, Scenario, ScenarioConfiguration, ScenarioInput, ScenarioInputType, manifest,
};
use crate::mcp::cache::Part;
use crate::tool::{Context, Tool};
use crate::{Error, Result};

/// How long the tools that start work wait for it by default, in seconds.
const DEFAULT_WAIT_SECONDS: u32 = 60;

/// The longest wait a tool takes, in seconds.
const MAX_WAIT_SECONDS: u32 = 300;

/// How many resources a list returns by default.
const DEFAULT_LIMIT: u32 = 20;

/// The most resources that a list returns.
const MAX_LIMIT: u32 = 200;

/// How many resources an error that lists what exists names.
const MAX_SUGGESTIONS: usize = 20;

/// The longest wait between two checks while a tool waits, in seconds, so
/// that a bounded wait ends close to its limit.
const MAX_POLL_DELAY: f64 = 5.0;

/// Heavy parts of a platform run that a result can include.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum RunPart {
    Output,
    Logs,
    Input,
}

/// Heavy parts of a local run that a result can include.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum LocalRunPart {
    Output,
    Logs,
    Input,
    Visuals,
}

/// Kinds of resources that `decision_list` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ListKind {
    Workspace,
    Build,
    Environment,
    Run,
    Acceptance,
    Scenario,
    Shadow,
    Switchback,
    Ensemble,
    InputSet,
    ManagedInput,
    Secrets,
}

/// Kinds of resources that `decision_get` gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum GetKind {
    Build,
    Environment,
    Acceptance,
    Scenario,
    Shadow,
    Switchback,
    Ensemble,
    InputSet,
    ManagedInput,
    Secrets,
}

/// Kinds of resources that `decision_delete` deletes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum DeleteKind {
    Workspace,
    Build,
    Environment,
    Run,
    Acceptance,
    Scenario,
    Shadow,
    Switchback,
    Ensemble,
    InputSet,
    ManagedInput,
    Secrets,
}

/// Arguments of `decision_workspace_get`.
#[derive(Debug, Deserialize, JsonSchema)]
struct WorkspaceGetArgs {
    /// ID of the workspace.
    workspace_id: String,
}

/// Arguments of `decision_list`.
#[derive(Debug, Deserialize, JsonSchema)]
struct ListArgs {
    /// What to list.
    kind: ListKind,
    /// ID of the workspace to list in. Required for every kind except
    /// `workspace`.
    workspace_id: Option<String>,
    /// Only resources with this status, for runs and tests, e.g. `running`
    /// or `failed`. Scenario test lists know only `started` and `completed`.
    status: Option<String>,
    /// Most resources to return. Defaults to 20.
    limit: Option<u32>,
}

/// Arguments of `decision_get`.
#[derive(Debug, Deserialize, JsonSchema)]
struct GetArgs {
    /// What to get.
    kind: GetKind,
    /// ID of the workspace the resource is in.
    workspace_id: String,
    /// ID of the resource.
    id: String,
    /// For `acceptance` and `scenario`: how long to wait for the test to
    /// finish, in seconds, at most 300. By default, returns its current
    /// state without waiting.
    #[schemars(range(max = 300))]
    wait_seconds: Option<u32>,
}

/// Arguments of `decision_run_get`.
#[derive(Debug, Deserialize, JsonSchema)]
struct RunGetArgs {
    /// ID of the workspace.
    workspace_id: String,
    /// ID of the run.
    run_id: String,
    /// Heavy parts to add to the result. Large ones come back as a file
    /// (local mode) or a resource link (remote mode).
    #[serde(default)]
    include: Vec<RunPart>,
    /// How long to wait for the run to finish, in seconds, at most 300. By
    /// default, returns its current state without waiting.
    #[schemars(range(max = 300))]
    wait_seconds: Option<u32>,
}

/// Arguments of `decision_run_compare`.
#[derive(Debug, Deserialize, JsonSchema)]
struct RunCompareArgs {
    /// IDs of the runs to compare, at least two. Local run IDs start with
    /// `local-`.
    run_ids: Vec<String>,
    /// ID of the workspace of the platform runs. Required when `run_ids`
    /// has platform runs.
    workspace_id: Option<String>,
    /// Directory of the workspace of the local runs. Required when `run_ids` has
    /// local runs. Only available in local mode.
    src: Option<PathBuf>,
}

/// Arguments of `decision_run_create`.
#[derive(Debug, Deserialize, JsonSchema)]
struct RunCreateArgs {
    /// ID of the workspace.
    workspace_id: String,
    /// ID of the environment to run on. Defaults to the workspace's default
    /// environment, which `default` names too.
    environment_id: Option<String>,
    /// The input, as JSON. Set exactly one of `input`, `input_file`,
    /// `input_dir`, `managed_input_id` and `from_run_id`.
    input: Option<Value>,
    /// File to read the input from. Only available in local mode.
    input_file: Option<PathBuf>,
    /// Directory of input files, for workspaces that read several files. Only
    /// available in local mode.
    input_dir: Option<PathBuf>,
    /// ID of a managed input to run on.
    managed_input_id: Option<String>,
    /// ID of a run whose input and options to run with again. The options
    /// set here replace the run's.
    from_run_id: Option<String>,
    /// Options for the workspace, e.g. `{"solve.duration": "10s"}`.
    #[serde(default)]
    options: BTreeMap<String, String>,
    /// Heavy parts to add to the result. Large ones come back as a file
    /// (local mode) or a resource link (remote mode).
    #[serde(default)]
    include: Vec<RunPart>,
    /// How long to wait for the run to finish, in seconds: 60 by default,
    /// at most 300, and 0 to return at once.
    #[schemars(range(max = 300))]
    wait_seconds: Option<u32>,
}

/// Arguments of `decision_acceptance_create`.
#[derive(Debug, Deserialize, JsonSchema)]
struct AcceptanceCreateArgs {
    /// ID of the workspace.
    workspace_id: String,
    /// ID of the environment to test.
    candidate_environment_id: String,
    /// ID of the environment to compare it with.
    baseline_environment_id: String,
    /// ID of the input set to test on. Set it or `run_ids`.
    input_set_id: Option<String>,
    /// IDs of runs whose inputs make a new input set to test on, when there
    /// is no `input_set_id`.
    #[serde(default)]
    run_ids: Vec<String>,
    /// The metrics that decide whether the candidate passes, at least one.
    metrics: Vec<Metric>,
    /// Name of the test.
    name: Option<String>,
    /// How long to wait for the test to finish, in seconds: 60 by default,
    /// at most 300, and 0 to return at once.
    #[schemars(range(max = 300))]
    wait_seconds: Option<u32>,
}

/// Arguments of `decision_scenario_create`.
#[derive(Debug, Deserialize, JsonSchema)]
struct ScenarioCreateArgs {
    /// ID of the workspace.
    workspace_id: String,
    /// ID of the input set that every scenario runs on.
    input_set_id: String,
    /// The scenarios to compare.
    scenarios: Vec<ScenarioArgs>,
    /// How many more times to run each scenario. Defaults to 0.
    repetitions: Option<u32>,
    /// Name of the test.
    name: Option<String>,
    /// How long to wait for the test to finish, in seconds: 60 by default,
    /// at most 300, and 0 to return at once.
    #[schemars(range(max = 300))]
    wait_seconds: Option<u32>,
}

/// A scenario of a scenario test: an environment, and the option values to
/// try on it.
#[derive(Debug, Deserialize, JsonSchema)]
struct ScenarioArgs {
    /// ID of the environment to run on.
    environment_id: String,
    /// Values to try for each option, e.g. `{"solve.duration": ["5s",
    /// "10s"]}`. The scenario runs every combination.
    #[serde(default)]
    options: BTreeMap<String, Vec<String>>,
}

/// Arguments of `decision_run_cancel`.
#[derive(Debug, Deserialize, JsonSchema)]
struct RunCancelArgs {
    /// ID of the workspace.
    workspace_id: String,
    /// ID of the run.
    run_id: String,
}

/// Arguments of `decision_delete`.
#[derive(Debug, Deserialize, JsonSchema)]
struct DeleteArgs {
    /// What to delete.
    kind: DeleteKind,
    /// ID of the workspace: the one to delete for kind `workspace`, and the
    /// one the resource is in for the others.
    workspace_id: String,
    /// ID of the resource. Required for every kind except `workspace`.
    id: Option<String>,
}

/// Arguments of `decision_guide`: none.
#[derive(Debug, Deserialize, JsonSchema)]
struct GuideArgs {}

/// Arguments of `decision_manifest_validate`.
#[derive(Debug, Deserialize, JsonSchema)]
struct ManifestValidateArgs {
    /// Directory of the workspace: the one that holds its `workspace.yaml`.
    src: PathBuf,
}

/// Arguments of `decision_local_run_create`.
#[derive(Debug, Deserialize, JsonSchema)]
struct LocalRunCreateArgs {
    /// Directory of the workspace: the one that holds its `workspace.yaml`.
    src: PathBuf,
    /// The input, as JSON. Set exactly one of `input`, `input_file`,
    /// `input_dir` and `from_run_id`.
    input: Option<Value>,
    /// File to read the input from.
    input_file: Option<PathBuf>,
    /// Directory of input files, for workspaces that read several files.
    input_dir: Option<PathBuf>,
    /// ID of a local run whose input and options to run with again. The
    /// options set here replace the run's.
    from_run_id: Option<String>,
    /// Options for the workspace, e.g. `{"solve.duration": "10s"}`.
    #[serde(default)]
    options: BTreeMap<String, String>,
    /// Heavy parts to add to the result. Large ones come back as a file.
    #[serde(default)]
    include: Vec<LocalRunPart>,
    /// How long to wait for the run to finish, in seconds: 60 by default,
    /// at most 300, and 0 to return at once.
    #[schemars(range(max = 300))]
    wait_seconds: Option<u32>,
}

/// Arguments of `decision_local_run_get`.
#[derive(Debug, Deserialize, JsonSchema)]
struct LocalRunGetArgs {
    /// Directory of the workspace: the one that holds its `workspace.yaml`.
    src: PathBuf,
    /// ID of the local run.
    run_id: String,
    /// Heavy parts to add to the result. Large ones come back as a file.
    #[serde(default)]
    include: Vec<LocalRunPart>,
    /// How long to wait for the run to finish, in seconds, at most 300. By
    /// default, returns its current state without waiting.
    #[schemars(range(max = 300))]
    wait_seconds: Option<u32>,
}

/// Arguments of `decision_local_run_list`.
#[derive(Debug, Deserialize, JsonSchema)]
struct LocalRunListArgs {
    /// Directory of the workspace: the one that holds its `workspace.yaml`.
    src: PathBuf,
    /// Only runs with this status.
    status: Option<RunStatus>,
    /// Most runs to return, newest first. Defaults to 20.
    limit: Option<u32>,
}

/// Arguments of `decision_local_workspace_sync`.
#[derive(Debug, Deserialize, JsonSchema)]
struct LocalWorkspaceSyncArgs {
    /// Directory of the workspace: the one that holds its `workspace.yaml`.
    src: PathBuf,
    /// ID of the platform workspace to upload the runs to.
    workspace_id: String,
    /// ID of the platform environment to record the runs under. Defaults to
    /// the workspace's default environment, which `default` also names.
    environment_id: Option<String>,
    /// IDs of the local runs to upload. Defaults to every run not uploaded
    /// yet.
    #[serde(default)]
    run_ids: Vec<String>,
}

impl From<RunPart> for Part {
    fn from(part: RunPart) -> Part {
        match part {
            RunPart::Output => Part::Output,
            RunPart::Logs => Part::Logs,
            RunPart::Input => Part::Input,
        }
    }
}

/// The parts of `include` that `parts` adds, each once, and whether it asks
/// for visuals.
fn local_parts(include: &[LocalRunPart]) -> (Vec<Part>, bool) {
    let mut parts = Vec::new();
    for part in include {
        let part = match part {
            LocalRunPart::Output => Part::Output,
            LocalRunPart::Logs => Part::Logs,
            LocalRunPart::Input => Part::Input,
            LocalRunPart::Visuals => continue,
        };
        if !parts.contains(&part) {
            parts.push(part);
        }
    }
    (parts, include.contains(&LocalRunPart::Visuals))
}

/// The parts of `include`, each once.
fn platform_parts(include: &[RunPart]) -> Vec<Part> {
    let mut parts = Vec::new();
    for part in include.iter().copied().map(Part::from) {
        if !parts.contains(&part) {
            parts.push(part);
        }
    }
    parts
}

/// `value` trimmed, or `None` if it is blank: models often send `""` for a
/// field they mean to leave out.
fn given(value: Option<String>) -> Option<String> {
    let value = value?;
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// `path`, or `None` if it is blank.
fn given_path(path: Option<PathBuf>) -> Option<PathBuf> {
    path.filter(|path| !path.to_string_lossy().trim().is_empty())
}

/// `input`, or `None` if it is `null` or a blank string.
fn given_input(input: Option<Value>) -> Option<Value> {
    match input? {
        Value::Null => None,
        Value::String(text) if text.trim().is_empty() => None,
        input => Some(input),
    }
}

/// `value` trimmed.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if it is blank.
fn required(value: String, field: &str) -> Result<String> {
    given(Some(value)).ok_or_else(|| Error::Invalid {
        reason: format!("`{field}` is required and must not be empty"),
    })
}

/// The directory of a workspace, from a `src` argument.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if it is blank.
fn required_src(src: PathBuf) -> Result<LocalWorkspaceRef> {
    let src = given_path(Some(src)).ok_or_else(|| Error::Invalid {
        reason: "`src` is required and must not be empty".to_owned(),
    })?;
    Ok(LocalWorkspaceRef {
        src: Some(src),
        workspace_id: None,
    })
}

/// How many seconds to wait: `requested`, or `default` without it, at most
/// [`MAX_WAIT_SECONDS`].
fn wait_seconds(requested: Option<u32>, default: u32) -> u32 {
    requested.unwrap_or(default).min(MAX_WAIT_SECONDS)
}

/// How many resources to list: `requested`, or [`DEFAULT_LIMIT`] without
/// it, from 1 to [`MAX_LIMIT`].
fn limit(requested: Option<u32>) -> usize {
    requested.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as usize
}

/// The SDK's polling options for a wait of `seconds`, with checks at most
/// [`MAX_POLL_DELAY`] apart.
fn polling(seconds: u32) -> PollingOptions {
    PollingOptions {
        initial_delay: 0.5,
        max_delay: MAX_POLL_DELAY,
        max_duration: Some(f64::from(seconds)),
        jitter: 0.5,
        ..PollingOptions::default()
    }
}

/// Checks with `check` until it says it is done, for up to `seconds`, and
/// returns the last value it got and whether it was done. The first check
/// comes at once, so a wait of 0 checks once.
///
/// This is the SDK's polling with a time limit, but the last state survives
/// the limit: running out of time is no error.
async fn wait_until<T, F, Fut>(seconds: u32, mut check: F) -> Result<(T, bool)>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(T, bool)>>,
{
    let (mut value, mut done) = check().await?;
    if done || seconds == 0 {
        return Ok((value, done));
    }
    let options = polling(seconds);
    let mut poller = Poller::start(&options).await?;
    loop {
        (value, done) = check().await?;
        if done {
            return Ok((value, true));
        }
        match poller.wait(None).await {
            Ok(()) => {}
            Err(Error::Wait { .. }) => return Ok((value, false)),
            Err(error) => return Err(error),
        }
    }
}

/// A platform run's information, after waiting up to `seconds` for it to
/// end.
async fn wait_for_run(
    platform: &Platform,
    workspace_id: &str,
    run_id: &str,
    seconds: u32,
) -> Result<RunInformation> {
    let check = || async move {
        let information = platform.get_run_information(workspace_id, run_id).await?;
        let done = information.metadata.status.is_terminal();
        Ok((information, done))
    };
    Ok(wait_until(seconds, check).await?.0)
}

/// A local run's information, after waiting up to `seconds` for it to end.
async fn wait_for_local_run(
    local: &Local,
    workspace: &LocalWorkspaceRef,
    run_id: &str,
    seconds: u32,
) -> Result<RunInformation> {
    let check = || async move {
        let information = local.get_run_information(workspace, run_id).await?;
        let done = information.metadata.status.is_terminal();
        Ok((information, done))
    };
    Ok(wait_until(seconds, check).await?.0)
}

/// What to do next with a run in `information`, by its status, given the
/// tool that gets it and the parts already included.
fn run_hint(
    information: &RunInformation,
    get_tool: &str,
    included: &[Part],
    waited: u32,
) -> Option<String> {
    let status = information.metadata.status;
    let status_name = wire(&status);
    let status_name = status_name.as_str().unwrap_or("not done");
    if !status.is_terminal() {
        let waited = match waited {
            0 => String::new(),
            seconds => format!(" after {seconds} s"),
        };
        return Some(format!(
            "The run is still {status_name}{waited}. Call `{get_tool}` with `wait_seconds` to wait \
             for it."
        ));
    }
    match status {
        RunStatus::Failed if !included.contains(&Part::Logs) => Some(format!(
            "The run failed. Call `{get_tool}` with `include: [\"logs\"]` to see why."
        )),
        RunStatus::Succeeded if !included.contains(&Part::Output) => Some(format!(
            "The run succeeded. Call `{get_tool}` with `include: [\"output\"]` for its output."
        )),
        _ => None,
    }
}

/// A platform run's result: its information with its environment's name,
/// the parts in `include`, and a hint.
async fn platform_run_result(
    cx: &Context,
    platform: &Platform,
    information: RunInformation,
    include: &[RunPart],
    waited: u32,
) -> Result<Value> {
    let metadata = &information.metadata;
    let mut environments = Names::new();
    if !metadata.environment_id.is_empty()
        && let Ok(environment) = platform
            .get_environment(&metadata.workspace_id, &metadata.environment_id)
            .await
        && !environment.name.is_empty()
    {
        environments.insert(environment.environment_id, environment.name);
    }
    let mut result = views::run(&information, &environments);
    let parts = platform_parts(include);
    parts::add_platform(platform, cx.local().ok(), &information, &parts, &mut result).await?;
    if let Some(hint) = run_hint(&information, "decision_run_get", &parts, waited) {
        result.insert("hint".to_owned(), json!(hint));
    }
    Ok(Value::Object(result))
}

/// A local run's result: its information, the parts in `include`, and a
/// hint.
async fn local_run_result(
    local: &Local,
    workspace: &LocalWorkspaceRef,
    information: RunInformation,
    include: &[LocalRunPart],
    waited: u32,
) -> Result<Value> {
    let mut result = views::run(&information, &Names::new());
    let (parts, visuals) = local_parts(include);
    parts::add_local(local, workspace, &information, &parts, visuals, &mut result).await?;
    if let Some(hint) = run_hint(&information, "decision_local_run_get", &parts, waited) {
        result.insert("hint".to_owned(), json!(hint));
    }
    Ok(Value::Object(result))
}

/// A kind of platform resource, for errors that say what exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Workspace,
    Build,
    Environment,
    Run,
    Acceptance,
    Scenario,
    Shadow,
    Switchback,
    Ensemble,
    InputSet,
    ManagedInput,
    Secrets,
}

impl Kind {
    /// What the resource is called in a sentence.
    fn noun(self) -> &'static str {
        match self {
            Kind::Workspace => "workspace",
            Kind::Build => "build",
            Kind::Environment => "environment",
            Kind::Run => "run",
            Kind::Acceptance => "acceptance test",
            Kind::Scenario => "scenario test",
            Kind::Shadow => "shadow test",
            Kind::Switchback => "switchback test",
            Kind::Ensemble => "ensemble",
            Kind::InputSet => "input set",
            Kind::ManagedInput => "managed input",
            Kind::Secrets => "secrets collection",
        }
    }

    /// The `kind` argument of `decision_list` that lists the resource.
    fn list_kind(self) -> &'static str {
        match self {
            Kind::Workspace => "workspace",
            Kind::Build => "build",
            Kind::Environment => "environment",
            Kind::Run => "run",
            Kind::Acceptance => "acceptance",
            Kind::Scenario => "scenario",
            Kind::Shadow => "shadow",
            Kind::Switchback => "switchback",
            Kind::Ensemble => "ensemble",
            Kind::InputSet => "input_set",
            Kind::ManagedInput => "managed_input",
            Kind::Secrets => "secrets",
        }
    }
}

impl From<GetKind> for Kind {
    fn from(kind: GetKind) -> Kind {
        match kind {
            GetKind::Build => Kind::Build,
            GetKind::Environment => Kind::Environment,
            GetKind::Acceptance => Kind::Acceptance,
            GetKind::Scenario => Kind::Scenario,
            GetKind::Shadow => Kind::Shadow,
            GetKind::Switchback => Kind::Switchback,
            GetKind::Ensemble => Kind::Ensemble,
            GetKind::InputSet => Kind::InputSet,
            GetKind::ManagedInput => Kind::ManagedInput,
            GetKind::Secrets => Kind::Secrets,
        }
    }
}

impl From<DeleteKind> for Kind {
    fn from(kind: DeleteKind) -> Kind {
        match kind {
            DeleteKind::Workspace => Kind::Workspace,
            DeleteKind::Build => Kind::Build,
            DeleteKind::Environment => Kind::Environment,
            DeleteKind::Run => Kind::Run,
            DeleteKind::Acceptance => Kind::Acceptance,
            DeleteKind::Scenario => Kind::Scenario,
            DeleteKind::Shadow => Kind::Shadow,
            DeleteKind::Switchback => Kind::Switchback,
            DeleteKind::Ensemble => Kind::Ensemble,
            DeleteKind::InputSet => Kind::InputSet,
            DeleteKind::ManagedInput => Kind::ManagedInput,
            DeleteKind::Secrets => Kind::Secrets,
        }
    }
}

/// The resources of `kind` in the workspace `workspace_id` (or the
/// workspaces, for [`Kind::Workspace`]), newest first, as their IDs and a
/// label: their name, or for runs their status. Runs are only the latest
/// ones.
async fn existing(
    platform: &Platform,
    workspace_id: &str,
    kind: Kind,
) -> Result<Vec<(String, String)>> {
    let ws = workspace_id;
    Ok(match kind {
        Kind::Workspace => platform
            .list_workspaces()
            .await?
            .into_iter()
            .map(|w| (w.workspace_id, w.name.unwrap_or_default()))
            .collect(),
        Kind::Build => newest(platform.list_builds(ws).await?, |b| &b.created_at)
            .into_iter()
            .map(|b| (b.build_id, b.name))
            .collect(),
        Kind::Environment => platform
            .list_environments(ws)
            .await?
            .into_iter()
            .map(|e| (e.environment_id, e.name))
            .collect(),
        Kind::Run => {
            let params = RunListParams {
                limit: Some(MAX_SUGGESTIONS as u32),
                ..RunListParams::default()
            };
            let runs = platform.list_runs(ws, params).await?;
            runs.into_iter()
                .map(|r| {
                    (
                        r.id,
                        wire(&r.status).as_str().unwrap_or_default().to_owned(),
                    )
                })
                .collect()
        }
        Kind::Acceptance => newest(platform.list_acceptances(ws).await?, |t| &t.created_at)
            .into_iter()
            .map(|t| (t.acceptance_id, t.name))
            .collect(),
        Kind::Scenario => newest(platform.list_scenarios(ws).await?, |t| &t.created_at)
            .into_iter()
            .map(|t| (t.scenario_id, t.name))
            .collect(),
        Kind::Shadow => platform
            .list_shadows(ws)
            .await?
            .into_iter()
            .filter_map(|t| Some((t.shadow_id?, t.name.unwrap_or_default())))
            .collect(),
        Kind::Switchback => platform
            .list_switchbacks(ws)
            .await?
            .into_iter()
            .filter_map(|t| Some((t.switchback_id?, t.name.unwrap_or_default())))
            .collect(),
        Kind::Ensemble => newest(platform.list_ensembles(ws).await?, |e| &e.created_at)
            .into_iter()
            .map(|e| (e.ensemble_id, e.name))
            .collect(),
        Kind::InputSet => newest(platform.list_input_sets(ws).await?, |i| &i.created_at)
            .into_iter()
            .map(|i| (i.input_set_id, i.name))
            .collect(),
        Kind::ManagedInput => platform
            .list_managed_inputs(ws)
            .await?
            .into_iter()
            .map(|i| (i.managed_input_id, i.name.unwrap_or_default()))
            .collect(),
        Kind::Secrets => newest(platform.list_secrets(ws).await?, |s| &s.created_at)
            .into_iter()
            .map(|s| (s.collection_id, s.name))
            .collect(),
    })
}

/// Whether the resource `id` of `kind` exists in the workspace
/// `workspace_id` (or is a workspace, for [`Kind::Workspace`]).
async fn exists(platform: &Platform, workspace_id: &str, kind: Kind, id: &str) -> Result<bool> {
    match kind {
        Kind::Workspace => platform.workspace_exists(id).await,
        Kind::Build => platform.build_exists(workspace_id, id).await,
        Kind::Environment => platform.environment_exists(workspace_id, id).await,
        Kind::Run => match platform.get_run_information(workspace_id, id).await {
            Ok(_) => Ok(true),
            Err(error) if missing(&error) => Ok(false),
            Err(error) => Err(error),
        },
        kind => {
            let existing = existing(platform, workspace_id, kind).await?;
            Ok(existing.iter().any(|(existing_id, _)| existing_id == id))
        }
    }
}

/// Whether `error` may mean that something the request named does not
/// exist: the platform answers `404`, and `400` for a run.
fn missing(error: &Error) -> bool {
    matches!(
        error,
        Error::Status {
            status: 400 | 404,
            ..
        }
    )
}

/// A sentence that lists `existing`, the resources of `kind` that exist,
/// as `(ID, label)`: `lead` and the first [`MAX_SUGGESTIONS`] of them, and,
/// when there may be more, how to see them with `more_tool`.
fn listing(lead: &str, existing: &[(String, String)], more_tool: &str, partial: bool) -> String {
    let names: Vec<String> = existing
        .iter()
        .take(MAX_SUGGESTIONS)
        .map(|(id, label)| match label.is_empty() || label == id {
            true => format!("`{id}`"),
            false => format!("`{id}` ({label})"),
        })
        .collect();
    let mut sentence = format!("{lead}: {}.", names.join(", "));
    let more = existing.len().saturating_sub(MAX_SUGGESTIONS);
    if more > 0 || partial {
        let more = match more {
            0 => "more".to_owned(),
            more => format!("{more} more"),
        };
        sentence.push_str(&format!(" For {more}, use {more_tool}."));
    }
    sentence
}

/// A sentence that lists `existing`, the resources of `kind` that exist in
/// a workspace, or the workspaces.
fn platform_listing(kind: Kind, existing: &[(String, String)]) -> String {
    let plural = match kind {
        Kind::Secrets => "secrets collections".to_owned(),
        kind => format!("{}s", kind.noun()),
    };
    if existing.is_empty() {
        return match kind {
            Kind::Workspace => "There are no workspaces.".to_owned(),
            _ => format!("It has no {plural}."),
        };
    }
    let lead = match kind {
        Kind::Workspace => "Workspaces".to_owned(),
        Kind::Run => "Its latest runs".to_owned(),
        _ => format!("Its {plural}"),
    };
    let more_tool = format!("`decision_list` with kind `{}`", kind.list_kind());
    // Runs are only the latest ones.
    listing(&lead, existing, &more_tool, kind == Kind::Run)
}

/// The error for the resource `id` of `kind` that does not exist in the
/// workspace `workspace_id`, which lists the ones that do.
async fn not_found(platform: &Platform, workspace_id: &str, kind: Kind, id: &str) -> Error {
    let lead = match kind {
        Kind::Workspace => format!("workspace `{id}` does not exist."),
        kind => format!(
            "{} `{id}` does not exist in workspace `{workspace_id}`.",
            kind.noun()
        ),
    };
    let reason = match existing(platform, workspace_id, kind).await {
        Ok(existing) => format!("{lead} {}", platform_listing(kind, &existing)),
        Err(_) => lead,
    };
    Error::Invalid { reason }
}

/// `error`, the error of a request about the workspace `workspace_id` and
/// the resources in `ids`, or, if it is because one of them does not exist,
/// an error that says which and lists the ones that do. The workspace is
/// checked first.
async fn explain(
    platform: &Platform,
    workspace_id: &str,
    error: Error,
    ids: &[(Kind, &str)],
) -> Error {
    if !missing(&error) {
        return error;
    }
    let workspace = [(Kind::Workspace, workspace_id)];
    for (kind, id) in workspace.iter().chain(ids) {
        match exists(platform, workspace_id, *kind, id).await {
            Ok(false) => return not_found(platform, workspace_id, *kind, id).await,
            Ok(true) => {}
            Err(_) => return error,
        }
    }
    error
}

/// `error`, the error of a local call about `workspace`, or an error that
/// says what to do: the runs that exist when a run does not, and how to
/// create a manifest when there is none.
async fn explain_local(local: &Local, workspace: &LocalWorkspaceRef, error: Error) -> Error {
    match &error {
        Error::NotFound { kind, id, .. } if *kind == "local run" => {
            let params = local_ops::RunListParams::default();
            let mut reason = format!("local run `{id}` does not exist.");
            if let Ok(runs) = local.list_runs(workspace, params).await {
                let runs: Vec<(String, String)> = runs
                    .into_iter()
                    .map(|run| {
                        let status = wire(&run.status).as_str().unwrap_or_default().to_owned();
                        (run.id, status)
                    })
                    .collect();
                let listing = match runs.is_empty() {
                    true => "The workspace has no local runs.".to_owned(),
                    false => listing(
                        "Its local runs, newest first",
                        &runs,
                        "`decision_local_run_list`",
                        false,
                    ),
                };
                reason = format!("{reason} {listing}");
            }
            Error::Invalid { reason }
        }
        Error::Io { path, source }
            if source.kind() == std::io::ErrorKind::NotFound
                && path.file_name() == Some("workspace.yaml".as_ref()) =>
        {
            Error::Invalid {
                reason: format!(
                    "there is no `workspace.yaml` in `{}`: write one with the \
                     `decision_manifest_init` operation, or point `src` at the directory that \
                     holds it",
                    path.parent().unwrap_or(path).display()
                ),
            }
        }
        _ => error,
    }
}

/// `items`, newest first by their `created_at`, which are RFC 3339
/// timestamps.
fn newest<T>(mut items: Vec<T>, created_at: impl Fn(&T) -> &str) -> Vec<T> {
    items.sort_by(|a, b| created_at(b).cmp(created_at(a)));
    items
}

/// Names by ID, from `(id, name)` pairs, without the empty names.
fn names(pairs: impl IntoIterator<Item = (String, String)>) -> Names {
    pairs
        .into_iter()
        .filter(|(_, name)| !name.is_empty())
        .collect()
}

async fn workspace_get(cx: Context, a: WorkspaceGetArgs) -> Result<Value> {
    let platform = cx.platform()?;
    let workspace_id = required(a.workspace_id, "workspace_id")?;
    let workspace = match platform.get_workspace(&workspace_id).await {
        Ok(workspace) => workspace,
        Err(error) => return Err(explain(platform, &workspace_id, error, &[]).await),
    };
    let environments = platform.list_environments(&workspace_id).await?;
    let mut builds = platform.list_builds(&workspace_id).await?;
    builds.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let params = RunListParams {
        limit: Some(RECENT_RUNS),
        ..RunListParams::default()
    };
    let runs = platform.list_runs(&workspace_id, params).await?;
    let build_names = names(builds.iter().map(|b| (b.build_id.clone(), b.name.clone())));
    let environment_names = names(
        environments
            .iter()
            .map(|e| (e.environment_id.clone(), e.name.clone())),
    );
    let mut by_status: BTreeMap<String, u64> = BTreeMap::new();
    for run in &runs {
        let status = wire(&run.status).as_str().unwrap_or("unknown").to_owned();
        *by_status.entry(status).or_default() += 1;
    }
    let mut running_tests = Vec::new();
    let running =
        |status: Option<ExperimentStatus>| status.is_some_and(|status| !status.is_terminal());
    for test in platform.list_acceptances(&workspace_id).await? {
        if running(test.status) {
            running_tests.push(test_item(
                "acceptance",
                &test.acceptance_id,
                &test.name,
                &test.status,
            ));
        }
    }
    for test in platform.list_scenarios(&workspace_id).await? {
        // A scenario list reports every status but `started` and
        // `completed` as `unknown`, which is not running.
        if test.status == Some(ExperimentStatus::Started) {
            running_tests.push(test_item(
                "scenario",
                &test.scenario_id,
                &test.name,
                &test.status,
            ));
        }
    }
    for test in platform.list_shadows(&workspace_id).await? {
        if running(test.status) {
            let id = test.shadow_id.unwrap_or_default();
            let name = test.name.unwrap_or_default();
            running_tests.push(test_item("shadow", &id, &name, &test.status));
        }
    }
    for test in platform.list_switchbacks(&workspace_id).await? {
        if running(test.status) {
            let id = test.switchback_id.unwrap_or_default();
            let name = test.name.unwrap_or_default();
            running_tests.push(test_item("switchback", &id, &name, &test.status));
        }
    }
    let environments: Vec<Value> = environments
        .iter()
        .map(|environment| views::environment(environment, &build_names))
        .collect();
    let latest_builds: Vec<Value> = builds
        .iter()
        .take(LATEST_BUILDS)
        .map(views::build)
        .collect();
    let recent_runs: Vec<Value> = runs
        .iter()
        .take(LATEST_RUNS)
        .map(|run| views::run_item(run, &environment_names))
        .collect();
    let mut result = match views::workspace(&workspace) {
        Value::Object(fields) => fields,
        _ => Map::new(),
    };
    let fields = object([
        (
            "console_url",
            json!(platform.workspace_console_url(&workspace_id)),
        ),
        ("environments", json!(environments)),
        (
            "builds",
            json!({ "count": builds.len(), "latest": latest_builds }),
        ),
        (
            "recent_runs",
            json!({
                "by_status": by_status,
                "of_latest": runs.len(),
                "latest": recent_runs,
            }),
        ),
        ("running_tests", json!(running_tests)),
    ]);
    result.extend(fields);
    if environments.is_empty() {
        result.insert(
            "hint".to_owned(),
            json!(
                "The workspace has no environments yet: push it, create a build and an \
                 environment through the operation tools, as `decision_guide` describes."
            ),
        );
    }
    Ok(Value::Object(result))
}

/// How many of a workspace's latest runs `decision_workspace_get` counts by
/// status, how many of them it lists, and how many of its latest builds.
const RECENT_RUNS: u32 = 50;
const LATEST_RUNS: usize = 5;
const LATEST_BUILDS: usize = 5;

/// A test, as `decision_workspace_get` lists the running ones.
fn test_item(kind: &str, id: &str, name: &str, status: &Option<ExperimentStatus>) -> Value {
    Value::Object(object([
        ("kind", json!(kind)),
        ("id", json!(id)),
        ("name", json!(name)),
        ("status", wire(status)),
    ]))
}

/// The run statuses a list can filter by.
const RUN_STATUSES: &str = "`queued`, `running`, `succeeded`, `failed` or `canceled`";

/// The test statuses a list can filter by.
const TEST_STATUSES: &str =
    "`started` (or `running`), `completed`, `failed`, `draft`, `canceled` or `stopping`";

/// The run status named `status`.
fn run_status(status: &str) -> Result<RunStatus> {
    serde_json::from_value(json!(status.to_lowercase())).map_err(|_| Error::Invalid {
        reason: format!("`{status}` is not a run status: use {RUN_STATUSES}"),
    })
}

/// The test status named `status`; `running` stands for `started`.
fn test_status(status: &str) -> Result<ExperimentStatus> {
    let status_name = match status.to_lowercase().as_str() {
        "running" => "started".to_owned(),
        status => status.to_owned(),
    };
    match serde_json::from_value(json!(status_name)) {
        Ok(ExperimentStatus::Unknown) | Err(_) => Err(Error::Invalid {
            reason: format!("`{status}` is not a test status: use {TEST_STATUSES}"),
        }),
        Ok(status) => Ok(status),
    }
}

async fn list(cx: Context, a: ListArgs) -> Result<Value> {
    let platform = cx.platform()?;
    let limit = limit(a.limit);
    let status = given(a.status);
    let workspace_id = given(a.workspace_id);
    let needs_workspace = a.kind != ListKind::Workspace;
    let ws = match (needs_workspace, &workspace_id) {
        (true, Some(workspace_id)) => workspace_id.as_str(),
        (true, None) => {
            return Err(Error::Invalid {
                reason: "`workspace_id` is required for every kind except `workspace`: find \
                         workspaces with kind `workspace`"
                    .to_owned(),
            });
        }
        (false, _) => "",
    };
    let filters_status = matches!(
        a.kind,
        ListKind::Run
            | ListKind::Acceptance
            | ListKind::Scenario
            | ListKind::Shadow
            | ListKind::Switchback
    );
    if status.is_some() && !filters_status {
        return Err(Error::Invalid {
            reason: "`status` only filters runs and tests".to_owned(),
        });
    }
    let test_filter = match (&status, a.kind) {
        (Some(status), kind) if kind != ListKind::Run => Some(test_status(status)?),
        _ => None,
    };
    let scenario_statuses = [ExperimentStatus::Started, ExperimentStatus::Completed];
    if a.kind == ListKind::Scenario
        && let Some(filter) = test_filter
        && !scenario_statuses.contains(&filter)
    {
        return Err(Error::Invalid {
            reason: "a scenario test list reports only `started` and `completed`, and every \
                     other status as `unknown`: list them without `status`, and get a test's \
                     status with `decision_get` and kind `scenario`"
                .to_owned(),
        });
    }
    let keep = |test_status: Option<ExperimentStatus>| {
        test_filter.is_none_or(|wanted| test_status == Some(wanted))
    };
    let items = match list_items(platform, ws, a.kind, status.as_deref(), limit, keep).await {
        Ok(items) => items,
        Err(error) if needs_workspace => return Err(explain(platform, ws, error, &[]).await),
        Err(error) => return Err(error),
    };
    let total = items.len();
    let more = total > limit;
    let items: Vec<Value> = items.into_iter().take(limit).collect();
    let mut result = object([
        ("kind", wire(&a.kind)),
        ("workspace_id", json!(workspace_id)),
        ("count", json!(items.len())),
        ("items", json!(items)),
    ]);
    if more {
        result.insert("more".to_owned(), json!(true));
        result.insert(
            "hint".to_owned(),
            json!(format!(
                "There are more than {limit}: pass a larger `limit`, at most {MAX_LIMIT}, to see \
                 more."
            )),
        );
    }
    Ok(Value::Object(result))
}

/// The resources of `kind` in the workspace `ws`, newest first, as lists
/// show them: at least `limit + 1` if there are that many, so the caller
/// knows when there are more. `keep` filters tests by status.
async fn list_items(
    platform: &Platform,
    ws: &str,
    kind: ListKind,
    status: Option<&str>,
    limit: usize,
    keep: impl Fn(Option<ExperimentStatus>) -> bool,
) -> Result<Vec<Value>> {
    Ok(match kind {
        ListKind::Workspace => newest(platform.list_workspaces().await?, |w| {
            w.created_at.as_deref().unwrap_or_default()
        })
        .iter()
        .map(views::workspace)
        .collect(),
        ListKind::Build => newest(platform.list_builds(ws).await?, |b| &b.created_at)
            .iter()
            .map(views::build)
            .collect(),
        ListKind::Environment => {
            let builds = platform.list_builds(ws).await?;
            let builds = names(builds.into_iter().map(|b| (b.build_id, b.name)));
            platform
                .list_environments(ws)
                .await?
                .iter()
                .map(|environment| views::environment(environment, &builds))
                .collect()
        }
        ListKind::Run => {
            let params = RunListParams {
                status: status.map(run_status).transpose()?,
                limit: Some(u32::try_from(limit + 1).unwrap_or(u32::MAX)),
                ..RunListParams::default()
            };
            let runs = platform.list_runs(ws, params).await?;
            let environments = platform.list_environments(ws).await?;
            let environments = names(environments.into_iter().map(|e| (e.environment_id, e.name)));
            runs.iter()
                .map(|run| views::run_item(run, &environments))
                .collect()
        }
        ListKind::Acceptance => newest(platform.list_acceptances(ws).await?, |t| &t.created_at)
            .iter()
            .filter(|test| keep(test.status))
            .map(views::acceptance_item)
            .collect(),
        ListKind::Scenario => newest(platform.list_scenarios(ws).await?, |t| &t.created_at)
            .iter()
            .filter(|test| keep(test.status))
            .map(views::scenario_item)
            .collect(),
        ListKind::Shadow => newest(platform.list_shadows(ws).await?, |t| {
            t.created_at.as_deref().unwrap_or_default()
        })
        .iter()
        .filter(|test| keep(test.status))
        .map(views::shadow_item)
        .collect(),
        ListKind::Switchback => newest(platform.list_switchbacks(ws).await?, |t| {
            t.created_at.as_deref().unwrap_or_default()
        })
        .iter()
        .filter(|test| keep(test.status))
        .map(views::switchback_item)
        .collect(),
        ListKind::Ensemble => newest(platform.list_ensembles(ws).await?, |e| &e.created_at)
            .iter()
            .map(views::ensemble_item)
            .collect(),
        ListKind::InputSet => newest(platform.list_input_sets(ws).await?, |i| &i.created_at)
            .iter()
            .map(views::input_set_item)
            .collect(),
        ListKind::ManagedInput => newest(platform.list_managed_inputs(ws).await?, |i| {
            i.created_at.as_deref().unwrap_or_default()
        })
        .iter()
        .map(views::managed_input)
        .collect(),
        ListKind::Secrets => newest(platform.list_secrets(ws).await?, |s| &s.created_at)
            .iter()
            .map(views::secrets_item)
            .collect(),
    })
}

async fn get(cx: Context, a: GetArgs) -> Result<Value> {
    let platform = cx.platform()?;
    let ws = required(a.workspace_id, "workspace_id")?;
    let id = required(a.id, "id")?;
    let (ws, id) = (ws.as_str(), id.as_str());
    let kind = Kind::from(a.kind);
    let waited = match a.kind {
        GetKind::Acceptance | GetKind::Scenario => wait_seconds(a.wait_seconds, 0),
        _ => 0,
    };
    let result = get_resource(platform, ws, id, a.kind, waited).await;
    match result {
        Ok(value) => Ok(value),
        Err(error) => Err(explain(platform, ws, error, &[(kind, id)]).await),
    }
}

/// The resource `id` of `kind` in the workspace `ws`, as `decision_get`
/// returns it, after waiting up to `waited` seconds for a test to end.
async fn get_resource(
    platform: &Platform,
    ws: &str,
    id: &str,
    kind: GetKind,
    waited: u32,
) -> Result<Value> {
    Ok(match kind {
        GetKind::Build => views::build(&platform.get_build(ws, id).await?),
        GetKind::Environment => {
            let environment = platform.get_environment(ws, id).await?;
            let mut builds = Names::new();
            if let Ok(build) = platform.get_build(ws, &environment.build_id).await {
                builds.insert(build.build_id, build.name);
            }
            views::environment(&environment, &builds)
        }
        GetKind::Acceptance => {
            // The view leaves the runs out, so no check reads them.
            let check = || async move {
                let params = AcceptanceGetParams::default();
                let test = platform.get_acceptance_without_runs(ws, id, params).await?;
                let done = test.status.is_some_and(|status| status.is_terminal());
                Ok((test, done))
            };
            let (test, done) = wait_until(waited, check).await?;
            let environments = platform.list_environments(ws).await?;
            let environments = names(environments.into_iter().map(|e| (e.environment_id, e.name)));
            with_test_hint(
                views::acceptance(&test, &environments),
                done,
                "acceptance",
                waited,
            )
        }
        GetKind::Scenario => {
            let check = || async move {
                let metadata = platform.get_scenario_metadata(ws, id).await?;
                let done = metadata.status.is_some_and(|status| status.is_terminal());
                Ok(((), done))
            };
            let ((), done) = wait_until(waited, check).await?;
            let test = platform
                .get_scenario(ws, id, ScenarioGetParams::default())
                .await?;
            with_test_hint(views::scenario(&test), done, "scenario", waited)
        }
        GetKind::Shadow => views::shadow(&platform.get_shadow(ws, id).await?),
        GetKind::Switchback => views::switchback(&platform.get_switchback(ws, id).await?),
        GetKind::Ensemble => wire(&platform.get_ensemble(ws, id).await?),
        GetKind::InputSet => views::input_set(&platform.get_input_set(ws, id).await?),
        GetKind::ManagedInput => views::managed_input(&platform.get_managed_input(ws, id).await?),
        GetKind::Secrets => wire(&platform.get_secrets(ws, id).await?.without_values()),
    })
}

/// `result`, a test's, with a hint to wait for it unless it is `done`.
fn with_test_hint(mut result: Value, done: bool, kind: &str, waited: u32) -> Value {
    if !done && let Value::Object(fields) = &mut result {
        let waited = match waited {
            0 => String::new(),
            seconds => format!(" after {seconds} s"),
        };
        let hint = format!(
            "The test has not finished{waited}. Call `decision_get` with kind `{kind}` and \
             `wait_seconds` to wait for its results."
        );
        fields.insert("hint".to_owned(), json!(hint));
    }
    result
}

async fn run_get(cx: Context, a: RunGetArgs) -> Result<Value> {
    let platform = cx.platform()?;
    let ws = required(a.workspace_id, "workspace_id")?;
    let run_id = required(a.run_id, "run_id")?;
    let waited = wait_seconds(a.wait_seconds, 0);
    let information = match wait_for_run(platform, &ws, &run_id, waited).await {
        Ok(information) => information,
        Err(error) => return Err(explain(platform, &ws, error, &[(Kind::Run, &run_id)]).await),
    };
    platform_run_result(&cx, platform, information, &a.include, waited).await
}

/// Whether `run_id` names a local run.
fn is_local_run(run_id: &str) -> bool {
    run_id.starts_with(&format!("{}-", local_ops::run_dir::RUN_ID_PREFIX))
}

async fn run_compare(cx: Context, a: RunCompareArgs) -> Result<Value> {
    let src = given_path(a.src);
    if src.is_some() {
        cx.local()?;
    }
    let workspace_id = given(a.workspace_id);
    if workspace_id.is_some() {
        cx.platform()?;
    }
    let run_ids: Vec<String> = a
        .run_ids
        .into_iter()
        .filter_map(|id| given(Some(id)))
        .collect();
    check_run_ids(&run_ids, "run")?;
    let has_local = run_ids.iter().any(|run_id| is_local_run(run_id));
    let has_platform = run_ids.iter().any(|run_id| !is_local_run(run_id));
    let workspace = match (has_local, src) {
        (true, Some(src)) => Some(LocalWorkspaceRef {
            src: Some(src),
            workspace_id: None,
        }),
        (true, None) => {
            return Err(Error::Invalid {
                reason: "`src` is required to compare local runs (IDs that start with `local-`): \
                         set it to the directory of their workspace"
                    .to_owned(),
            });
        }
        (false, _) => None,
    };
    let ws = match (has_platform, &workspace_id) {
        (true, Some(ws)) => ws.as_str(),
        (true, None) => {
            return Err(Error::Invalid {
                reason: "`workspace_id` is required to compare platform runs".to_owned(),
            });
        }
        (false, _) => "",
    };
    let comparison = match (&workspace, has_platform) {
        (None, _) => {
            let platform = cx.platform()?;
            let params = platform_ops::RunCompareParams {
                run_ids: run_ids.clone(),
            };
            match platform.compare_runs(ws, params).await {
                Ok(comparison) => comparison,
                Err(error) => {
                    let ids: Vec<(Kind, &str)> =
                        run_ids.iter().map(|id| (Kind::Run, id.as_str())).collect();
                    return Err(explain(platform, ws, error, &ids).await);
                }
            }
        }
        (Some(workspace), false) => {
            let local = cx.local()?;
            let params = local_ops::RunCompareParams {
                run_ids: run_ids.clone(),
            };
            match local.compare_runs(workspace, params).await {
                Ok(comparison) => comparison,
                Err(error) => return Err(explain_local(local, workspace, error).await),
            }
        }
        (Some(workspace), true) => {
            // Platform and local runs together: their information compares
            // as either one's does.
            let (platform, local) = (cx.platform()?, cx.local()?);
            let mut runs = Vec::with_capacity(run_ids.len());
            for run_id in &run_ids {
                let information = match is_local_run(run_id) {
                    true => local
                        .get_run_information(workspace, run_id)
                        .await
                        .map_err(|error| (error, true)),
                    false => platform
                        .get_run_information(ws, run_id)
                        .await
                        .map_err(|error| (error, false)),
                };
                match information {
                    Ok(information) => runs.push(information),
                    Err((error, true)) => return Err(explain_local(local, workspace, error).await),
                    Err((error, false)) => {
                        return Err(explain(platform, ws, error, &[(Kind::Run, run_id)]).await);
                    }
                }
            }
            compare(run_ids, &runs)
        }
    };
    Ok(views::comparison(&comparison))
}

/// The error for a run create that does not set exactly one input.
fn one_input(fields: &str) -> Error {
    Error::Invalid {
        reason: format!("set exactly one of {fields}"),
    }
}

async fn run_create(cx: Context, a: RunCreateArgs) -> Result<Value> {
    let input_file = given_path(a.input_file);
    let input_dir = given_path(a.input_dir);
    if input_file.is_some() || input_dir.is_some() {
        cx.local()?;
    }
    let platform = cx.platform()?;
    let ws = required(a.workspace_id, "workspace_id")?;
    let environment_id = given(a.environment_id);
    let managed_input_id = given(a.managed_input_id);
    let from_run_id = given(a.from_run_id);
    let input = given_input(a.input);
    let inputs = [
        input.is_some(),
        input_file.is_some(),
        input_dir.is_some(),
        managed_input_id.is_some(),
        from_run_id.is_some(),
    ];
    if inputs.iter().filter(|set| **set).count() != 1 {
        return Err(one_input(
            "`input`, `input_file`, `input_dir`, `managed_input_id` and `from_run_id`",
        ));
    }
    let created = match &from_run_id {
        Some(run_id) => {
            let params = platform_ops::RunCloneParams {
                environment_id: environment_id.clone(),
                options: a.options,
                ..platform_ops::RunCloneParams::default()
            };
            platform.clone_run(&ws, run_id, params).await
        }
        None => {
            let params = platform_ops::RunCreateParams {
                input,
                input_file,
                input_dir,
                managed_input_id: managed_input_id.clone(),
                environment_id: environment_id.clone(),
                options: a.options,
                ..platform_ops::RunCreateParams::default()
            };
            platform.create_run(&ws, params).await
        }
    };
    let created = match created {
        Ok(created) => created,
        Err(error) => {
            let mut ids = Vec::new();
            if let Some(environment_id) = &environment_id {
                ids.push((Kind::Environment, environment_id.as_str()));
            }
            if let Some(managed_input_id) = &managed_input_id {
                ids.push((Kind::ManagedInput, managed_input_id.as_str()));
            }
            if let Some(run_id) = &from_run_id {
                ids.push((Kind::Run, run_id.as_str()));
            }
            return Err(explain(platform, &ws, error, &ids).await);
        }
    };
    let waited = wait_seconds(a.wait_seconds, DEFAULT_WAIT_SECONDS);
    match wait_for_run(platform, &ws, &created.run_id, waited).await {
        Ok(information) => {
            platform_run_result(&cx, platform, information, &a.include, waited).await
        }
        Err(error) => Ok(started_but(&created.run_id, "decision_run_get", &error)),
    }
}

/// The result of a tool that started the run `run_id` and then failed to
/// get it: the run's ID, and what to do.
fn started_but(run_id: &str, get_tool: &str, error: &Error) -> Value {
    json!({
        "run_id": run_id,
        "hint": format!(
            "The run started, but getting it failed: {}. Get it with `{get_tool}`.",
            crate::mcp::message(error)
        ),
    })
}

async fn acceptance_create(cx: Context, a: AcceptanceCreateArgs) -> Result<Value> {
    let platform = cx.platform()?;
    let ws = required(a.workspace_id, "workspace_id")?;
    let candidate = required(a.candidate_environment_id, "candidate_environment_id")?;
    let baseline = required(a.baseline_environment_id, "baseline_environment_id")?;
    let input_set_id = given(a.input_set_id);
    let run_ids: Vec<String> = a
        .run_ids
        .into_iter()
        .filter_map(|id| given(Some(id)))
        .collect();
    let name = given(a.name);
    // Checked before the input set is made, so that a test the SDK would
    // reject leaves nothing behind. `create_acceptance` checks it too.
    if a.metrics.is_empty() {
        return Err(Error::Invalid {
            reason: "`metrics` needs at least one metric".to_owned(),
        });
    }
    let mut created_input_set = false;
    let input_set_id = match (input_set_id, run_ids.is_empty()) {
        (Some(input_set_id), true) => input_set_id,
        (None, false) => {
            let params = InputSetCreateParams {
                description: Some(format!(
                    "Inputs of {} runs, for an acceptance test of `{candidate}` against \
                     `{baseline}`.",
                    run_ids.len()
                )),
                run_ids: Some(run_ids.clone()),
                ..InputSetCreateParams::default()
            };
            match platform.create_input_set(&ws, params).await {
                Ok(input_set) => {
                    created_input_set = true;
                    input_set.input_set_id
                }
                Err(error) => {
                    let ids: Vec<(Kind, &str)> =
                        run_ids.iter().map(|id| (Kind::Run, id.as_str())).collect();
                    return Err(explain(platform, &ws, error, &ids).await);
                }
            }
        }
        _ => {
            return Err(Error::Invalid {
                reason: "set exactly one of `input_set_id` and `run_ids`: an input set to test \
                         on, or the runs whose inputs make a new one"
                    .to_owned(),
            });
        }
    };
    let params = AcceptanceCreateParams {
        name,
        baseline_environment_id: baseline.clone(),
        candidate_environment_id: candidate.clone(),
        input_set_id: input_set_id.clone(),
        metrics: a.metrics,
        ..AcceptanceCreateParams::default()
    };
    let test = match platform.create_acceptance(&ws, params).await {
        Ok(test) => test,
        Err(error) => {
            let ids = [
                (Kind::Environment, candidate.as_str()),
                (Kind::Environment, baseline.as_str()),
                (Kind::InputSet, input_set_id.as_str()),
            ];
            let error = explain(platform, &ws, error, &ids).await;
            if !created_input_set {
                return Err(error);
            }
            // Say what the call made, so that a retry reuses it. A reason
            // that `explain` wrote goes in as it is, not as the message of
            // its `Error::Invalid`, which would repeat the prefix.
            let cause = match &error {
                Error::Invalid { reason } => reason.trim_end_matches('.').to_owned(),
                error => crate::mcp::message(error),
            };
            return Err(Error::Invalid {
                reason: format!(
                    "{cause}. The input set `{input_set_id}` was made from `run_ids` before the \
                     test failed: pass it as `input_set_id` to try again"
                ),
            });
        }
    };
    let waited = wait_seconds(a.wait_seconds, DEFAULT_WAIT_SECONDS);
    let acceptance_id = test.acceptance_id.clone();
    let mut result = get_resource(platform, &ws, &acceptance_id, GetKind::Acceptance, waited)
        .await
        .unwrap_or_else(|error| {
            json!({
                "acceptance_id": acceptance_id,
                "hint": format!(
                    "The test started, but getting it failed: {}. Get it with `decision_get` and \
                     kind `acceptance`.",
                    crate::mcp::message(&error)
                ),
            })
        });
    if let Value::Object(fields) = &mut result {
        fields.insert("input_set_id".to_owned(), json!(input_set_id));
        if created_input_set {
            fields.insert("input_set_created".to_owned(), json!(true));
        }
    }
    Ok(result)
}

async fn scenario_create(cx: Context, a: ScenarioCreateArgs) -> Result<Value> {
    let platform = cx.platform()?;
    let ws = required(a.workspace_id, "workspace_id")?;
    let input_set_id = required(a.input_set_id, "input_set_id")?;
    let mut environment_ids = Vec::new();
    let mut scenarios = Vec::with_capacity(a.scenarios.len());
    for (index, scenario) in a.scenarios.into_iter().enumerate() {
        let field = format!("scenarios[{index}].environment_id");
        let environment_id = required(scenario.environment_id, &field)?;
        environment_ids.push(environment_id.clone());
        let configuration: Vec<ScenarioConfiguration> = scenario
            .options
            .into_iter()
            .map(|(name, values)| ScenarioConfiguration { name, values })
            .collect();
        scenarios.push(Scenario {
            scenario_id: None,
            environment_id,
            scenario_input: ScenarioInput {
                scenario_input_type: ScenarioInputType::InputSet,
                scenario_input_data: json!(input_set_id),
            },
            configuration: (!configuration.is_empty()).then_some(configuration),
        });
    }
    let params = ScenarioCreateParams {
        name: given(a.name),
        scenarios,
        repetitions: a.repetitions,
        ..ScenarioCreateParams::default()
    };
    let created = match platform.create_scenario(&ws, params).await {
        Ok(created) => created,
        Err(error) => {
            let mut ids: Vec<(Kind, &str)> = environment_ids
                .iter()
                .map(|id| (Kind::Environment, id.as_str()))
                .collect();
            ids.push((Kind::InputSet, input_set_id.as_str()));
            return Err(explain(platform, &ws, error, &ids).await);
        }
    };
    let waited = wait_seconds(a.wait_seconds, DEFAULT_WAIT_SECONDS);
    let scenario_id = created.scenario_id;
    Ok(
        get_resource(platform, &ws, &scenario_id, GetKind::Scenario, waited)
            .await
            .unwrap_or_else(|error| {
                json!({
                    "scenario_id": scenario_id,
                    "hint": format!(
                        "The test started, but getting it failed: {}. Get it with \
                         `decision_get` and kind `scenario`.",
                        crate::mcp::message(&error)
                    ),
                })
            }),
    )
}

async fn run_cancel(cx: Context, a: RunCancelArgs) -> Result<Value> {
    let platform = cx.platform()?;
    let ws = required(a.workspace_id, "workspace_id")?;
    let run_id = required(a.run_id, "run_id")?;
    if let Err(error) = platform.cancel_run(&ws, &run_id).await {
        return Err(explain(platform, &ws, error, &[(Kind::Run, &run_id)]).await);
    }
    let mut result = object([("run_id", json!(run_id)), ("canceled", json!(true))]);
    if let Ok(information) = platform.get_run_information(&ws, &run_id).await {
        result.insert("status".to_owned(), wire(&information.metadata.status));
    }
    Ok(Value::Object(result))
}

async fn delete(cx: Context, a: DeleteArgs) -> Result<Value> {
    let platform = cx.platform()?;
    let ws = required(a.workspace_id, "workspace_id")?;
    let id = given(a.id);
    let kind = Kind::from(a.kind);
    let id = match (a.kind, id) {
        (DeleteKind::Workspace, None) => ws.clone(),
        (DeleteKind::Workspace, Some(id)) if id == ws => id,
        (DeleteKind::Workspace, Some(_)) => {
            return Err(Error::Invalid {
                reason: "to delete a workspace, set `workspace_id` to it and leave `id` out"
                    .to_owned(),
            });
        }
        (_, Some(id)) => id,
        (_, None) => {
            return Err(Error::Invalid {
                reason: format!("`id` is required to delete a {}", kind.noun()),
            });
        }
    };
    let (ws, id) = (ws.as_str(), id.as_str());
    // The background deletes don't wait: the platform finishes them later.
    let deleted = match a.kind {
        DeleteKind::Workspace => platform.delete_workspace(ws, Default::default()).await,
        DeleteKind::Build => platform.delete_build(ws, id).await,
        DeleteKind::Environment => platform.delete_environment(ws, id).await,
        DeleteKind::Run => platform.delete_run(ws, id, Default::default()).await,
        DeleteKind::Acceptance => platform.delete_acceptance(ws, id, Default::default()).await,
        DeleteKind::Scenario => platform.delete_scenario(ws, id, Default::default()).await,
        DeleteKind::Shadow => platform.delete_shadow(ws, id, Default::default()).await,
        DeleteKind::Switchback => platform.delete_switchback(ws, id, Default::default()).await,
        DeleteKind::Ensemble => platform.delete_ensemble(ws, id).await,
        DeleteKind::InputSet => platform.delete_input_set(ws, id, Default::default()).await,
        DeleteKind::ManagedInput => platform.delete_managed_input(ws, id).await,
        DeleteKind::Secrets => platform.delete_secrets(ws, id).await,
    };
    if let Err(error) = deleted {
        let ids: &[(Kind, &str)] = match kind {
            Kind::Workspace => &[],
            _ => &[(kind, id)],
        };
        return Err(explain(platform, ws, error, ids).await);
    }
    let mut result = object([("deleted", json!(true)), ("kind", wire(&a.kind))]);
    match kind {
        Kind::Workspace => result.insert("workspace_id".to_owned(), json!(ws)),
        _ => {
            result.insert("workspace_id".to_owned(), json!(ws));
            result.insert("id".to_owned(), json!(id))
        }
    };
    Ok(Value::Object(result))
}

/// The decision workflow guide that `decision_guide` returns.
const GUIDE: &str = include_str!("decision_guide.md");

async fn guide(_cx: Context, _: GuideArgs) -> Result<Value> {
    Ok(json!({ "guide": GUIDE }))
}

async fn manifest_validate(cx: Context, a: ManifestValidateArgs) -> Result<Value> {
    cx.local()?;
    let workspace = required_src(a.src)?;
    let params = manifest::ManifestValidateParams {
        workspace_dir: workspace.src,
    };
    match manifest::validate(params).await {
        Ok(manifest) => Ok(json!({ "valid": true, "manifest": manifest })),
        Err(Error::Manifest { path, reason, .. }) => Ok(json!({
            "valid": false,
            "path": path,
            "problem": reason,
            "hint": "Fix the manifest, then validate it again. `decision_guide` describes what it \
                     holds.",
        })),
        Err(Error::Io { path, source })
            if source.kind() == std::io::ErrorKind::NotFound
                && path.file_name() == Some("workspace.yaml".as_ref()) =>
        {
            Ok(json!({
                "valid": false,
                "path": path,
                "problem": "there is no `workspace.yaml` here",
                "hint": "Write one with the `decision_manifest_init` operation (see \
                         `decision_guide`), or point `src` at the directory that holds it.",
            }))
        }
        Err(error) => Err(error),
    }
}

async fn local_run_create(cx: Context, a: LocalRunCreateArgs) -> Result<Value> {
    let local = cx.local()?;
    let workspace = required_src(a.src)?;
    let input = given_input(a.input);
    let input_file = given_path(a.input_file);
    let input_dir = given_path(a.input_dir);
    let from_run_id = given(a.from_run_id);
    let inputs = [
        input.is_some(),
        input_file.is_some(),
        input_dir.is_some(),
        from_run_id.is_some(),
    ];
    if inputs.iter().filter(|set| **set).count() != 1 {
        return Err(one_input(
            "`input`, `input_file`, `input_dir` and `from_run_id`",
        ));
    }
    let created = match &from_run_id {
        Some(run_id) => {
            let params = local_ops::RunCloneParams {
                options: a.options,
                ..local_ops::RunCloneParams::default()
            };
            local.clone_run(&workspace, run_id, params).await
        }
        None => {
            let params = local_ops::RunCreateParams {
                input,
                input_file,
                input_dir,
                options: a.options,
                ..local_ops::RunCreateParams::default()
            };
            local.create_run(&workspace, params).await
        }
    };
    let created = match created {
        Ok(created) => created,
        Err(error) => return Err(explain_local(local, &workspace, error).await),
    };
    let waited = wait_seconds(a.wait_seconds, DEFAULT_WAIT_SECONDS);
    let information = match wait_for_local_run(local, &workspace, &created.run_id, waited).await {
        Ok(information) => information,
        Err(error) => {
            return Ok(started_but(
                &created.run_id,
                "decision_local_run_get",
                &error,
            ));
        }
    };
    let mut result = local_run_result(local, &workspace, information, &a.include, waited).await?;
    if created.workspace_registered
        && let Value::Object(fields) = &mut result
    {
        fields.insert("workspace_registered".to_owned(), json!(true));
    }
    Ok(result)
}

async fn local_run_get(cx: Context, a: LocalRunGetArgs) -> Result<Value> {
    let local = cx.local()?;
    let workspace = required_src(a.src)?;
    let run_id = required(a.run_id, "run_id")?;
    let waited = wait_seconds(a.wait_seconds, 0);
    let information = match wait_for_local_run(local, &workspace, &run_id, waited).await {
        Ok(information) => information,
        Err(error) => return Err(explain_local(local, &workspace, error).await),
    };
    local_run_result(local, &workspace, information, &a.include, waited).await
}

async fn local_run_list(cx: Context, a: LocalRunListArgs) -> Result<Value> {
    let local = cx.local()?;
    let workspace = required_src(a.src)?;
    let limit = limit(a.limit);
    let params = local_ops::RunListParams { status: a.status };
    let runs = match local.list_runs(&workspace, params).await {
        Ok(runs) => runs,
        Err(error) => return Err(explain_local(local, &workspace, error).await),
    };
    let more = runs.len() > limit;
    let items: Vec<Value> = runs
        .iter()
        .take(limit)
        .map(|run| views::run_item(run, &Names::new()))
        .collect();
    let mut result = object([("count", json!(items.len())), ("items", json!(items))]);
    if more {
        result.insert("more".to_owned(), json!(true));
        result.insert(
            "hint".to_owned(),
            json!(format!(
                "There are more than {limit}: pass a larger `limit`, at most {MAX_LIMIT}, to see \
                 more."
            )),
        );
    }
    Ok(Value::Object(result))
}

async fn local_workspace_sync(cx: Context, a: LocalWorkspaceSyncArgs) -> Result<Value> {
    let (local, platform) = (cx.local()?, cx.platform()?);
    let workspace = required_src(a.src)?;
    let ws = required(a.workspace_id, "workspace_id")?;
    let environment_id = given(a.environment_id);
    let params = WorkspaceSyncParams {
        run_ids: a
            .run_ids
            .into_iter()
            .filter_map(|id| given(Some(id)))
            .collect(),
        environment_id: environment_id.clone(),
    };
    let synced = match local
        .sync_workspace(&workspace, platform, &ws, params)
        .await
    {
        Ok(synced) => synced,
        Err(error @ Error::Status { .. }) => {
            let ids: Vec<(Kind, &str)> = environment_id
                .iter()
                .map(|id| (Kind::Environment, id.as_str()))
                .collect();
            return Err(explain(platform, &ws, error, &ids).await);
        }
        Err(error) => return Err(explain_local(local, &workspace, error).await),
    };
    let mut counts: BTreeMap<&str, u64> = BTreeMap::new();
    let runs: Vec<Value> = synced
        .runs
        .iter()
        .map(|run| {
            let (outcome, details) = match &run.outcome {
                RunSyncOutcome::Synced { synced_run, .. } => ("synced", json!(synced_run.run_id)),
                RunSyncOutcome::AlreadySynced { synced_run, .. } => {
                    ("already_synced", json!(synced_run.run_id))
                }
                RunSyncOutcome::Skipped { reason, .. } => ("skipped", json!(reason)),
            };
            *counts.entry(outcome).or_default() += 1;
            let key = match outcome {
                "skipped" => "reason",
                _ => "platform_run_id",
            };
            Value::Object(object([
                ("run_id", json!(run.run_id)),
                ("outcome", json!(outcome)),
                (key, details),
            ]))
        })
        .collect();
    let result = object([
        ("workspace_id", json!(ws)),
        ("environment_id", json!(environment_id)),
        ("counts", json!(counts)),
        ("runs", json!(runs)),
        (
            "workspace_registered",
            json!(synced.workspace_registered.then_some(true)),
        ),
        ("console_url", json!(platform.workspace_console_url(&ws))),
    ]);
    Ok(Value::Object(result))
}

/// The content of `part` of the platform run `run_id`, for
/// `resources/read`.
pub(crate) async fn read_resource(
    platform: &Platform,
    workspace_id: &str,
    run_id: &str,
    part: Part,
) -> Result<String> {
    match parts::read_resource(platform, workspace_id, run_id, part).await {
        Ok(text) => Ok(text),
        Err(error) => Err(explain(platform, workspace_id, error, &[(Kind::Run, run_id)]).await),
    }
}

/// The curated decision tools.
pub(crate) fn tools() -> Vec<Tool> {
    vec![
        // Platform tools, served in both modes.
        Tool::new(
            "decision_workspace_get",
            "Gets a workspace with what you need to work on it: its environments and the build \
             each one runs, its latest builds, its recent runs by status and its running tests. \
             Call it first when you work on an existing workspace. To find a workspace, use \
             `decision_list` with kind `workspace`; for one build, environment or test in \
             full, use `decision_get`.",
            workspace_get,
        )
        .with_title("Get a workspace overview")
        .read_only(),
        Tool::new(
            "decision_list",
            "Lists the resources of one kind, newest first, with their names next to their IDs: \
             workspaces, builds, environments, runs, tests, ensembles, input sets, managed \
             inputs or secrets. Use `status` to find e.g. failed runs or running \
             tests, and `limit` to see more than 20. To get one resource in full, use \
             `decision_get`, or `decision_run_get` for a run.",
            list,
        )
        .with_title("List resources")
        .read_only(),
        Tool::new(
            "decision_get",
            "Gets one resource of a workspace by kind and ID: a build, an environment, a test \
             with its results, an ensemble, an input set, a managed input or a secrets \
             collection, without the secret values. For acceptance and scenario tests, \
             `wait_seconds` waits for them to finish. For a workspace, use \
             `decision_workspace_get`, and for a run, `decision_run_get`.",
            get,
        )
        .with_title("Get a resource")
        .read_only(),
        Tool::new(
            "decision_run_get",
            "Gets a platform run: its status, metrics and summary. Add its output, logs or \
             input with `include`, and wait for it to finish with `wait_seconds`. To find runs, \
             use `decision_list` with kind `run`; to compare them, `decision_run_compare`.",
            run_get,
        )
        .with_title("Get a run")
        .read_only(),
        Tool::new(
            "decision_run_compare",
            "Compares the metrics and options of two or more runs side by side, platform runs \
             and local runs alike. Use it to choose between builds, options or inputs after \
             running them. To see how a candidate environment did against a baseline on many \
             inputs, get its acceptance test with `decision_get` instead.",
            run_compare,
        )
        .with_title("Compare runs")
        .read_only(),
        Tool::new(
            "decision_run_create",
            "Starts a run on a platform environment, with the input inline, from a file or \
             directory (local mode only), from a managed input or from an earlier run \
             (`from_run_id`), and uploads it when needed. It waits up to `wait_seconds` (60 by \
             default) and returns what `decision_run_get` returns, or the current status if \
             the run is not done yet, so call `decision_run_get` to wait more. To run on several \
             environments or option values at once, use `decision_scenario_create`.",
            run_create,
        )
        .with_title("Start a run"),
        Tool::new(
            "decision_acceptance_create",
            "Starts an acceptance test: runs a candidate environment and a baseline environment \
             on the same inputs, and checks the candidate against the baseline with metrics. \
             The inputs are an input set (`input_set_id`), or the inputs of runs (`run_ids`), \
             which make a new input set. It waits up to `wait_seconds`; get its results later \
             with `decision_get` and kind `acceptance`.",
            acceptance_create,
        )
        .with_title("Start an acceptance test"),
        Tool::new(
            "decision_scenario_create",
            "Starts a scenario test: runs every scenario, an environment with option values to \
             try, on the same input set, to compare setups side by side. Every combination of a \
             scenario's option values becomes its own run. It waits up to `wait_seconds`; get \
             its results later with `decision_get` and kind `scenario`.",
            scenario_create,
        )
        .with_title("Start a scenario test"),
        Tool::new(
            "decision_run_cancel",
            "Cancels a queued or running platform run. A run that already finished cannot be \
             canceled, and a canceled run keeps its input and logs. To remove a run, use \
             `decision_delete` with kind `run`.",
            run_cancel,
        )
        .with_title("Cancel a run")
        .destructive(),
        Tool::new(
            "decision_delete",
            "Deletes one resource by kind and ID: a workspace with everything in it, a build, \
             an environment, a run, a test, an ensemble, an input set, a managed input or a \
             secrets collection. Deleting cannot be undone. To stop a run instead, use \
             `decision_run_cancel`.",
            delete,
        )
        .with_title("Delete a resource")
        .destructive(),
        Tool::new(
            "decision_guide",
            "Returns the decision workflow guide: how a workspace is laid out, its \
             `workspace.yaml` manifest, local runs, pushing, and testing with acceptance and \
             scenario tests. Call it before you create, change or push a workspace. It takes no \
             arguments.",
            guide,
        )
        .with_title("Get the decision workflow guide")
        .read_only(),
        // Local tools, served only in local mode.
        Tool::new(
            "decision_manifest_validate",
            "Validates the `workspace.yaml` manifest of the workspace at `src`, and returns it \
             or says what is wrong with it. Call it after you change the manifest, before you \
             run or push the workspace. For what a manifest holds, call `decision_guide`.",
            manifest_validate,
        )
        .with_title("Validate a workspace manifest")
        .read_only()
        .local_only(),
        Tool::new(
            "decision_local_run_create",
            "Runs the workspace at `src` on this machine, with the input inline, from a file or \
             directory, or from an earlier local run (`from_run_id`), registering the \
             workspace if needed. It waits up to `wait_seconds` (60 by default) and returns what \
             `decision_local_run_get` returns, or the current status if the run is not done \
             yet. To run on the platform, use `decision_run_create`.",
            local_run_create,
        )
        .with_title("Run a workspace locally")
        .local_only(),
        Tool::new(
            "decision_local_run_get",
            "Gets a run on this machine: its status, metrics and summary. Add its output, logs, \
             input or visuals with `include`, and wait for it to finish with `wait_seconds`. To \
             find a run, use `decision_local_run_list`; to compare runs, \
             `decision_run_compare`.",
            local_run_get,
        )
        .with_title("Get a local run")
        .read_only()
        .local_only(),
        Tool::new(
            "decision_local_run_list",
            "Lists the runs of the workspace at `src` on this machine, newest first, with their \
             status. Use `status` to find e.g. failed runs, and `limit` to see more than 20. To \
             get one run in full, use `decision_local_run_get`.",
            local_run_list,
        )
        .with_title("List local runs")
        .read_only()
        .local_only(),
        Tool::new(
            "decision_local_workspace_sync",
            "Uploads the runs of the workspace at `src` on this machine to a platform workspace, \
             as tracked runs that platform tools can list, compare and test with. It uploads every \
             run not uploaded yet, or only `run_ids`. To run on the platform instead, use \
             `decision_run_create`.",
            local_workspace_sync,
        )
        .with_title("Sync local runs to the platform")
        .local_only(),
    ]
}
