//! The concise results of the curated tools: the fields of each resource
//! that an agent needs to decide its next step, with names next to IDs, in
//! the platform API's names.
//!
//! Each view takes what the SDK returns and leaves out what is empty, so
//! that results stay small.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::decision::compare::unwrapped_metrics;
use crate::decision::{
    AcceptanceTest, Build, EnsembleDefinition, Environment, InputSet, ManagedInput, Metric,
    MetricResult, Run, RunComparison, RunInformation, RunType, ScenarioTest, ScenarioTestMetadata,
    SecretsCollectionSummary, ShadowTest, SwitchbackTest, Workspace,
};

/// Names of the environments or builds of a workspace, by ID.
pub(super) type Names = BTreeMap<String, String>;

/// Most runs of a test that a result lists one by one.
const MAX_LISTED_RUNS: usize = 20;

/// Most characters of a test's grouped summaries that a result holds.
const MAX_SUMMARIES_CHARS: usize = 20_000;

/// `value` as JSON, e.g. an enum as its name on the wire.
pub(super) fn wire<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// An object with the `fields` that are set: not `null`, and not an empty
/// string, array or object.
pub(super) fn object<const N: usize>(fields: [(&str, Value); N]) -> Map<String, Value> {
    let mut object = Map::new();
    for (key, value) in fields {
        let empty = match &value {
            Value::Null => true,
            Value::String(text) => text.is_empty(),
            Value::Array(items) => items.is_empty(),
            Value::Object(fields) => fields.is_empty(),
            _ => false,
        };
        if !empty {
            object.insert(key.to_owned(), value);
        }
    }
    object
}

/// The name of `id` in `names`, as JSON, or `null`.
fn name_of(names: &Names, id: &str) -> Value {
    names.get(id).map_or(Value::Null, |name| json!(name))
}

/// A workspace, as lists show it.
pub(super) fn workspace(workspace: &Workspace) -> Value {
    Value::Object(object([
        ("workspace_id", json!(workspace.workspace_id)),
        ("name", json!(workspace.name)),
        ("description", json!(workspace.description)),
        (
            "default_environment_id",
            json!(workspace.default_environment_id),
        ),
    ]))
}

/// A build.
pub(super) fn build(build: &Build) -> Value {
    Value::Object(object([
        ("build_id", json!(build.build_id)),
        ("name", json!(build.name)),
        ("description", json!(build.description)),
        ("created_at", json!(build.created_at)),
    ]))
}

/// An environment, with the name of the build it runs from `builds`.
///
/// Its `options` are always there, `{}` when it sets none, so that an
/// agent never has to look further to tell that it has none.
pub(super) fn environment(environment: &Environment, builds: &Names) -> Value {
    let configuration = &environment.configuration;
    let mut result = object([
        ("environment_id", json!(environment.environment_id)),
        ("name", json!(environment.name)),
        ("description", json!(environment.description)),
        ("build_id", json!(environment.build_id)),
        ("build_name", name_of(builds, &environment.build_id)),
        ("execution_class", json!(configuration.execution_class)),
        (
            "secrets_collection_id",
            json!(configuration.secrets_collection_id),
        ),
        ("locked", json!(environment.locked.then_some(true))),
    ]);
    let options = configuration.options.clone().unwrap_or_default();
    result.insert("options".to_owned(), json!(options));
    Value::Object(result)
}

/// A run's metrics, out of the `{"metrics": {…}}` that wraps them when it is
/// the only key.
fn metrics(metrics: Option<&Map<String, Value>>) -> Value {
    unwrapped_metrics(metrics).map_or(Value::Null, |metrics| json!(metrics))
}

/// A run's information, platform or local, with the name of its environment
/// from `environments`.
pub(super) fn run(information: &RunInformation, environments: &Names) -> Map<String, Value> {
    let metadata = &information.metadata;
    let format = &metadata.format;
    let input_format = format.format_input.input_type;
    let output_format = format
        .format_output
        .as_ref()
        .map_or(input_format, |output| output.output_type);
    let tracking = metadata.tracking.as_ref();
    let run_type = metadata
        .run_type
        .as_ref()
        .and_then(|run_type| run_type.run_type)
        .filter(|run_type| *run_type != RunType::Standard);
    let options = metadata
        .options
        .as_ref()
        .and_then(|options| options.active_options.as_ref());
    object([
        ("run_id", json!(information.id)),
        ("name", json!(information.name)),
        ("description", json!(information.description)),
        ("status", wire(&metadata.status)),
        (
            "run_type",
            run_type.map_or(Value::Null, |run_type| wire(&run_type)),
        ),
        ("workspace_id", json!(metadata.workspace_id)),
        ("environment_id", json!(metadata.environment_id)),
        (
            "environment_name",
            name_of(environments, &metadata.environment_id),
        ),
        ("build_id", json!(metadata.build_id)),
        ("created_at", json!(metadata.created_at)),
        ("duration_ms", json!(metadata.duration)),
        ("error", json!(metadata.error)),
        ("metrics", self::metrics(metadata.metrics.as_ref())),
        ("options", json!(options)),
        ("input_format", wire(&input_format)),
        ("output_format", wire(&output_format)),
        ("input_bytes", json!(metadata.input_size as u64)),
        ("output_bytes", json!(metadata.output_size as u64)),
        (
            "cloned_from_run_id",
            json!(tracking.and_then(|tracking| tracking.cloned_run_id.as_deref())),
        ),
        (
            "managed_input_id",
            json!(tracking.and_then(|tracking| tracking.input_id.as_deref())),
        ),
        ("console_url", json!(information.console_url)),
        ("synced_runs", json!(information.synced_runs)),
    ])
}

/// A run, as run lists show it, with the name of its environment from
/// `environments`.
pub(super) fn run_item(run: &Run, environments: &Names) -> Value {
    let metrics = run.metrics.as_ref().and_then(|metrics| {
        let indicators = metrics.indicators.as_ref()?;
        let values = indicators
            .iter()
            .map(|indicator| (indicator.name.clone(), indicator.value.clone()));
        Some(Map::from_iter(values))
    });
    Value::Object(object([
        ("run_id", json!(run.id)),
        ("name", json!(run.name)),
        ("status", wire(&run.status)),
        ("created_at", json!(run.created_at)),
        ("environment_id", json!(run.environment_id)),
        (
            "environment_name",
            name_of(environments, &run.environment_id),
        ),
        ("build_id", json!(run.build_id)),
        ("metrics", json!(metrics)),
    ]))
}

/// A run comparison: the metrics and options of every run, and the other
/// fields only where the runs differ.
pub(super) fn comparison(comparison: &RunComparison) -> Value {
    let differences: Map<String, Value> = comparison
        .information
        .iter()
        .chain(&comparison.metadata)
        .filter(|(_, values)| values.has_differences)
        .map(|(field, values)| (field.clone(), json!(values.values)))
        .collect();
    let all = |fields: &Option<BTreeMap<String, crate::decision::ComparisonValues>>| {
        let fields = fields.as_ref()?;
        let fields = fields.iter().map(|(field, values)| {
            let value = json!({
                "values": values.values,
                "differ": values.has_differences,
            });
            (field.clone(), value)
        });
        Some(Map::from_iter(fields))
    };
    Value::Object(object([
        ("run_ids", json!(comparison.run_ids)),
        ("metrics", json!(all(&comparison.metrics))),
        ("options", json!(all(&comparison.options))),
        ("other_differences", Value::Object(differences)),
    ]))
}

/// The value of a metric's statistic in `statistics`, the statistics of one
/// environment of an acceptance test.
fn statistic(statistics: &Value, statistic: &Value) -> Value {
    let Some(name) = statistic.as_str() else {
        return Value::Null;
    };
    [
        "distribution_summary_statistics",
        "distribution_percentiles",
    ]
    .iter()
    .find_map(|group| statistics[group].get(name).cloned())
    .unwrap_or(Value::Null)
}

/// What a metric of an acceptance test checks.
fn metric(metric: &Metric) -> Map<String, Value> {
    object([
        ("field", json!(metric.field)),
        ("metric_type", wire(&metric.metric_type)),
        ("statistic", wire(&metric.statistic)),
        ("operator", wire(&metric.params.operator)),
        ("tolerance", wire(&metric.params.tolerance)),
    ])
}

/// The result of a metric of an acceptance test: the statistic of each
/// environment, and whether the candidate passed.
fn metric_result(result: &MetricResult) -> Value {
    let mut fields = metric(&result.metric);
    let statistic_name = wire(&result.metric.statistic);
    let candidate = wire(&result.statistics.candidate);
    let baseline = wire(&result.statistics.control);
    fields.insert(
        "candidate".to_owned(),
        statistic(&candidate, &statistic_name),
    );
    fields.insert("baseline".to_owned(), statistic(&baseline, &statistic_name));
    fields.insert(
        "runs".to_owned(),
        json!({
            "candidate": candidate["number_of_runs_total"],
            "baseline": baseline["number_of_runs_total"],
        }),
    );
    fields.insert("passed".to_owned(), json!(result.passed));
    Value::Object(fields)
}

/// An acceptance test, as lists show it.
pub(super) fn acceptance_item(test: &AcceptanceTest) -> Value {
    Value::Object(object([
        ("acceptance_id", json!(test.acceptance_id)),
        ("name", json!(test.name)),
        ("status", wire(&test.status)),
        ("created_at", json!(test.created_at)),
        (
            "candidate_environment_id",
            json!(test.candidate.as_ref().map(|env| &env.environment_id)),
        ),
        (
            "baseline_environment_id",
            json!(test.control.as_ref().map(|env| &env.environment_id)),
        ),
        (
            "passed",
            json!(test.results.as_ref().map(|results| results.passed)),
        ),
    ]))
}

/// An acceptance test with its results, with environment names from
/// `environments`.
pub(super) fn acceptance(test: &AcceptanceTest, environments: &Names) -> Value {
    let side = |env: Option<&crate::decision::ComparisonEnvironment>| {
        env.map(|env| {
            Value::Object(object([
                ("environment_id", json!(env.environment_id)),
                (
                    "environment_name",
                    name_of(environments, &env.environment_id),
                ),
                ("build_id", json!(env.build_id)),
            ]))
        })
    };
    let results = test.results.as_ref();
    let metric_results = results.and_then(|results| results.metric_results.as_ref());
    let metrics: Vec<Value> = match metric_results {
        Some(results) => results.iter().map(metric_result).collect(),
        None => test
            .metrics
            .iter()
            .flatten()
            .map(|test_metric| Value::Object(metric(test_metric)))
            .collect(),
    };
    Value::Object(object([
        ("acceptance_id", json!(test.acceptance_id)),
        ("name", json!(test.name)),
        ("description", json!(test.description)),
        ("status", wire(&test.status)),
        ("created_at", json!(test.created_at)),
        ("candidate", json!(side(test.candidate.as_ref()))),
        ("baseline", json!(side(test.control.as_ref()))),
        ("passed", json!(results.map(|results| results.passed))),
        (
            "error",
            json!(results.and_then(|results| results.error.as_deref())),
        ),
        ("metrics", json!(metrics)),
    ]))
}

/// A count of `runs` by status, and the first [`MAX_LISTED_RUNS`] of them.
fn test_runs(runs: Option<&Vec<Run>>) -> Value {
    let Some(runs) = runs else {
        return Value::Null;
    };
    let mut by_status: BTreeMap<String, u64> = BTreeMap::new();
    for run in runs {
        let status = wire(&run.status).as_str().unwrap_or("unknown").to_owned();
        *by_status.entry(status).or_default() += 1;
    }
    let listed: Vec<Value> = runs
        .iter()
        .take(MAX_LISTED_RUNS)
        .map(|run| {
            Value::Object(object([
                ("run_id", json!(run.id)),
                ("status", wire(&run.status)),
                ("environment_id", json!(run.environment_id)),
                ("scenario_id", json!(run.scenario_id)),
                ("option_set", json!(run.option_set)),
                ("repetition", json!(run.repetition)),
            ]))
        })
        .collect();
    let mut result = object([
        ("count", json!(runs.len())),
        ("by_status", json!(by_status)),
        ("listed", json!(listed)),
    ]);
    if runs.len() > MAX_LISTED_RUNS {
        result.insert(
            "hint".to_owned(),
            json!(format!(
                "Only the first {MAX_LISTED_RUNS} runs are listed; get any run with \
                 `decision_run_get`."
            )),
        );
    }
    Value::Object(result)
}

/// A test's grouped summaries, unless they are too large to return inline.
fn summaries(summaries: Option<&Vec<Map<String, Value>>>, operation: &str) -> (Value, Value) {
    let Some(summaries) = summaries.filter(|summaries| !summaries.is_empty()) else {
        return (Value::Null, Value::Null);
    };
    let value = json!(summaries);
    if value.to_string().chars().count() <= MAX_SUMMARIES_CHARS {
        return (value, Value::Null);
    }
    let note = format!(
        "{} grouped summaries are too large to return here; call `fico_operation_read` with \
         `{operation}` for them.",
        summaries.len()
    );
    (Value::Null, json!(note))
}

/// A scenario test, as lists show it.
pub(super) fn scenario_item(test: &ScenarioTestMetadata) -> Value {
    Value::Object(object([
        ("scenario_id", json!(test.scenario_id)),
        ("name", json!(test.name)),
        ("status", wire(&test.status)),
        ("created_at", json!(test.created_at)),
        ("requested_runs", json!(test.number_of_requested_runs)),
        ("completed_runs", json!(test.number_of_completed_runs)),
    ]))
}

/// A scenario test with its results.
pub(super) fn scenario(test: &ScenarioTest) -> Value {
    let (summaries, omitted) = summaries(
        test.grouped_distributional_summaries.as_ref(),
        "decision_platform_scenario_get",
    );
    Value::Object(object([
        ("scenario_id", json!(test.scenario_id)),
        ("name", json!(test.name)),
        ("description", json!(test.description)),
        ("status", wire(&test.status)),
        ("created_at", json!(test.created_at)),
        ("input_set_id", json!(test.input_set_id)),
        ("environment_ids", json!(test.environment_ids)),
        ("option_sets", json!(test.option_sets)),
        ("requested_runs", json!(test.number_of_requested_runs)),
        ("completed_runs", json!(test.number_of_completed_runs)),
        ("grouped_distributional_summaries", summaries),
        ("grouped_distributional_summaries_omitted", omitted),
        ("runs", test_runs(test.runs.as_ref())),
    ]))
}

/// A shadow test, as lists show it.
pub(super) fn shadow_item(test: &ShadowTest) -> Value {
    Value::Object(object([
        ("shadow_id", json!(test.shadow_id)),
        ("name", json!(test.name)),
        ("status", wire(&test.status)),
        ("created_at", json!(test.created_at)),
    ]))
}

/// A shadow test with its results.
pub(super) fn shadow(test: &ShadowTest) -> Value {
    let (summaries, omitted) = summaries(
        test.grouped_distributional_summaries.as_ref(),
        "decision_platform_shadow_get",
    );
    Value::Object(object([
        ("shadow_id", json!(test.shadow_id)),
        ("name", json!(test.name)),
        ("description", json!(test.description)),
        ("status", wire(&test.status)),
        ("created_at", json!(test.created_at)),
        ("completed_at", json!(test.completed_at)),
        ("comparisons", json!(test.comparisons)),
        ("start_events", json!(test.start_events)),
        ("termination_events", json!(test.termination_events)),
        ("grouped_distributional_summaries", summaries),
        ("grouped_distributional_summaries_omitted", omitted),
        ("runs", test_runs(test.runs.as_ref())),
    ]))
}

/// A switchback test, as lists show it.
pub(super) fn switchback_item(test: &SwitchbackTest) -> Value {
    Value::Object(object([
        ("switchback_id", json!(test.switchback_id)),
        ("name", json!(test.name)),
        ("status", wire(&test.status)),
        ("created_at", json!(test.created_at)),
    ]))
}

/// A switchback test with its results.
pub(super) fn switchback(test: &SwitchbackTest) -> Value {
    Value::Object(object([
        ("switchback_id", json!(test.switchback_id)),
        ("name", json!(test.name)),
        ("description", json!(test.description)),
        ("status", wire(&test.status)),
        ("created_at", json!(test.created_at)),
        ("started_at", json!(test.started_at)),
        ("completed_at", json!(test.completed_at)),
        ("comparison", json!(test.comparison)),
        ("plan", json!(test.plan)),
        ("runs", test_runs(test.runs.as_ref())),
    ]))
}

/// An ensemble definition, as lists show it.
pub(super) fn ensemble_item(ensemble: &EnsembleDefinition) -> Value {
    Value::Object(object([
        ("ensemble_id", json!(ensemble.ensemble_id)),
        ("name", json!(ensemble.name)),
        ("description", json!(ensemble.description)),
        ("created_at", json!(ensemble.created_at)),
    ]))
}

/// An input set, as lists show it.
pub(super) fn input_set_item(input_set: &InputSet) -> Value {
    let inputs = input_set.input_ids.len().max(input_set.inputs.len());
    Value::Object(object([
        ("input_set_id", json!(input_set.input_set_id)),
        ("name", json!(input_set.name)),
        ("description", json!(input_set.description)),
        ("created_at", json!(input_set.created_at)),
        ("inputs", json!(inputs)),
    ]))
}

/// An input set, with the first of its inputs.
pub(super) fn input_set(input_set: &InputSet) -> Value {
    let mut result = match input_set_item(input_set) {
        Value::Object(fields) => fields,
        _ => Map::new(),
    };
    let inputs: Vec<Value> = input_set
        .inputs
        .iter()
        .take(MAX_LISTED_RUNS)
        .map(managed_input)
        .collect();
    let input_ids: Vec<&String> = input_set.input_ids.iter().take(MAX_LISTED_RUNS).collect();
    cache_put(&mut result, "managed_inputs", json!(inputs));
    cache_put(&mut result, "input_ids", json!(input_ids));
    Value::Object(result)
}

/// A managed input.
pub(super) fn managed_input(input: &ManagedInput) -> Value {
    Value::Object(object([
        ("managed_input_id", json!(input.managed_input_id)),
        ("name", json!(input.name)),
        ("description", json!(input.description)),
        ("run_id", json!(input.run_id)),
        ("format", json!(input.format)),
        ("created_at", json!(input.created_at)),
    ]))
}

/// A secrets collection, without its secrets.
pub(super) fn secrets_item(summary: &SecretsCollectionSummary) -> Value {
    Value::Object(object([
        ("collection_id", json!(summary.collection_id)),
        ("name", json!(summary.name)),
        ("description", json!(summary.description)),
        ("created_at", json!(summary.created_at)),
    ]))
}

/// Inserts `value` at `key`, unless it is empty.
fn cache_put(object: &mut Map<String, Value>, key: &str, value: Value) {
    let empty = matches!(&value, Value::Array(items) if items.is_empty());
    if !empty {
        object.insert(key.to_owned(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_out_what_is_empty() {
        let object = object([
            ("a", json!("x")),
            ("b", json!("")),
            ("c", Value::Null),
            ("d", json!([])),
            ("e", json!({})),
            ("f", json!(0)),
            ("g", json!(false)),
        ]);
        assert_eq!(
            Value::Object(object),
            json!({ "a": "x", "f": 0, "g": false })
        );
    }

    #[test]
    fn finds_the_statistic_of_a_metric() {
        let statistics = json!({
            "distribution_summary_statistics": { "mean": 2.5, "min": 1.0 },
            "distribution_percentiles": { "p50": 2.0 },
        });
        assert_eq!(statistic(&statistics, &json!("mean")), json!(2.5));
        assert_eq!(statistic(&statistics, &json!("p50")), json!(2.0));
        assert_eq!(statistic(&statistics, &json!("p99")), Value::Null);
    }
}
