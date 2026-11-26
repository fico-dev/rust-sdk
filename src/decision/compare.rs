//! The field-by-field comparison of runs, shared by platform and local runs.

use std::collections::{BTreeMap, HashSet};

use serde_json::{Map, Value, json};

use crate::decision::{ComparisonValues, Metadata, RunComparison, RunInformation};
use crate::{Error, Result};

/// The most runs that a comparison takes. [`check_run_ids`] enforces it
/// before any run is read.
pub(crate) const MAX_COMPARED_RUNS: usize = 20;

/// Checks the IDs of the runs to compare: from 1 to [`MAX_COMPARED_RUNS`],
/// each one once. `kind` names the runs in the error, e.g. `local run`.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if there are too few or too many, or one is
/// there twice.
pub(crate) fn check_run_ids(run_ids: &[String], kind: &str) -> Result<()> {
    if !(1..=MAX_COMPARED_RUNS).contains(&run_ids.len()) {
        return Err(Error::Invalid {
            reason: format!(
                "compare from 1 to {MAX_COMPARED_RUNS} runs, not {}",
                run_ids.len()
            ),
        });
    }
    let mut seen = HashSet::new();
    if let Some(twice) = run_ids.iter().find(|&run_id| !seen.insert(run_id)) {
        return Err(Error::Invalid {
            reason: format!("{kind} `{twice}` is in `run_ids` more than once"),
        });
    }
    Ok(())
}

/// The field-by-field comparison of `runs`, whose IDs are `run_ids`.
///
/// It compares the runs' information fields but `metadata` and
/// `console_url`; their metadata fields, as [`add_metadata`] reads them;
/// their metrics, out of a lone `metrics` key that wraps them, with decimals
/// rounded to 5 digits; and their active options. Each field has the value
/// of every run that has it, by run ID, and `has_differences` says whether
/// those values are not all equal as JSON, numbers compared by value.
/// `metrics` and `options` are unset when no run has any.
pub(crate) fn compare(run_ids: Vec<String>, runs: &[RunInformation]) -> RunComparison {
    let mut information = Fields::default();
    let mut metadata = Fields::default();
    let mut metrics = Fields::default();
    let mut options = Fields::default();
    for (run_id, run) in run_ids.iter().zip(runs) {
        // `RunInformation` always serializes to an object.
        let Ok(Value::Object(fields)) = serde_json::to_value(run) else {
            continue;
        };
        for (name, value) in fields {
            if !matches!(name.as_str(), "metadata" | "console_url") {
                information.add(name, run_id, value);
            }
        }
        add_metadata(&mut metadata, run_id, &run.metadata);
        for (name, value) in unwrapped_metrics(run.metadata.metrics.as_ref())
            .into_iter()
            .flatten()
        {
            metrics.add(name.clone(), run_id, round_metric(value));
        }
        let active = run.metadata.options.as_ref();
        let active = active.and_then(|options| options.active_options.as_ref());
        for (name, value) in active.into_iter().flatten() {
            options.add(name.clone(), run_id, value.clone());
        }
    }
    RunComparison {
        run_ids,
        information: information.finish(),
        metadata: metadata.finish(),
        metrics: Some(metrics.finish()).filter(|metrics| !metrics.is_empty()),
        options: Some(options.finish()).filter(|options| !options.is_empty()),
    }
}

/// A run's metrics, out of the `{"metrics": {…}}` that wraps them when it is
/// the only key.
pub(crate) fn unwrapped_metrics(
    metrics: Option<&Map<String, Value>>,
) -> Option<&Map<String, Value>> {
    let metrics = metrics?;
    match (metrics.len(), metrics.get("metrics")) {
        (1, Some(Value::Object(inner))) => Some(inner),
        _ => Some(metrics),
    }
}

/// Adds the metadata fields of the run `run_id` to `fields`: every field
/// with a value, with the API's names, but metrics and options; the input
/// format as `content_format`; the integration's and the tracking's fields
/// on their own; the run type as its type; and `status_v2` as `status`.
fn add_metadata(fields: &mut Fields, run_id: &str, metadata: &Metadata) {
    // `Metadata` always serializes to an object.
    let Ok(Value::Object(metadata_fields)) = serde_json::to_value(metadata) else {
        return;
    };
    for (name, value) in metadata_fields {
        match (name.as_str(), value) {
            ("metrics" | "options", _) => {}
            ("format", _) => {
                let format = json!(metadata.format.format_input.input_type);
                fields.add("content_format".to_owned(), run_id, format);
            }
            ("integration" | "tracking", Value::Object(nested)) => {
                for (name, value) in nested {
                    fields.add(name, run_id, value);
                }
            }
            ("run_type", value) => {
                let run_type = value.get("type").cloned().unwrap_or(Value::Null);
                fields.add(name, run_id, run_type);
            }
            ("status_v2", value) => fields.add("status".to_owned(), run_id, value),
            (_, value) => fields.add(name, run_id, value),
        }
    }
}

/// `metric`, with a decimal rounded to 5 digits.
fn round_metric(metric: &Value) -> Value {
    match metric {
        Value::Number(number) if number.is_f64() => {
            let rounded = number.as_f64().map(|number| format!("{number:.5}"));
            let rounded = rounded.and_then(|text| text.parse::<f64>().ok());
            rounded.map_or_else(|| metric.clone(), Value::from)
        }
        _ => metric.clone(),
    }
}

/// The values of compared fields, by field name and then run ID.
#[derive(Debug, Default)]
struct Fields(BTreeMap<String, Map<String, Value>>);

impl Fields {
    /// Records `value` as the run `run_id`'s value of the field `name`.
    fn add(&mut self, name: String, run_id: &str, value: Value) {
        self.0
            .entry(name)
            .or_default()
            .insert(run_id.to_owned(), value);
    }

    /// The comparison of each field: its values, and whether they differ.
    fn finish(self) -> BTreeMap<String, ComparisonValues> {
        let fields = self.0.into_iter().map(|(name, values)| {
            let mut all = values.values();
            let first = all.next();
            let has_differences = all.any(|value| first.is_some_and(|first| !same(first, value)));
            let values = ComparisonValues {
                has_differences,
                values,
            };
            (name, values)
        });
        fields.collect()
    }
}

/// Whether `a` and `b` are the same JSON, with numbers compared by value,
/// so that `1` and `1.0` are the same.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => a == b || a.as_f64() == b.as_f64(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(name, a)| b.get(name).is_some_and(|b| same(a, b)))
        }
        (a, b) => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(run_ids: &[&str]) -> Vec<String> {
        run_ids.iter().map(|&run_id| run_id.to_owned()).collect()
    }

    #[test]
    fn checks_the_run_ids() {
        assert!(check_run_ids(&ids(&["run-1"]), "run").is_ok());
        let twenty: Vec<String> = (0..20).map(|n| format!("run-{n}")).collect();
        assert!(check_run_ids(&twenty, "run").is_ok());
        let twenty_one: Vec<String> = (0..21).map(|n| format!("run-{n}")).collect();
        for run_ids in [Vec::new(), twenty_one, ids(&["run-1", "run-2", "run-1"])] {
            let err = check_run_ids(&run_ids, "run").unwrap_err();
            assert!(matches!(err, Error::Invalid { .. }), "{run_ids:?}: {err:?}");
        }
        let err = check_run_ids(&ids(&["a", "a"]), "local run").unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid argument: local run `a` is in `run_ids` more than once"
        );
    }

    #[test]
    fn unwraps_lone_wrapped_metrics() {
        let wrapped = json!({ "metrics": { "cost": 1 } });
        let unwrapped = unwrapped_metrics(wrapped.as_object()).unwrap();
        assert_eq!(Value::Object(unwrapped.clone()), json!({ "cost": 1 }));
        // Not when other metrics are next to it, nor when it holds no object.
        for metrics in [
            json!({ "metrics": { "cost": 1 }, "time": 2 }),
            json!({ "metrics": 3 }),
            json!({ "cost": 1 }),
        ] {
            let kept = unwrapped_metrics(metrics.as_object()).unwrap();
            assert_eq!(Value::Object(kept.clone()), metrics);
        }
        assert_eq!(unwrapped_metrics(None), None);
    }

    #[test]
    fn rounds_decimals_to_5_digits() {
        assert_eq!(round_metric(&json!(1.234567)), json!(1.23457));
        assert_eq!(round_metric(&json!(7)), json!(7));
        assert_eq!(round_metric(&json!("1.234567")), json!("1.234567"));
    }

    #[test]
    fn compares_numbers_by_value() {
        assert!(same(&json!(1), &json!(1.0)));
        assert!(same(&json!({ "a": [1, 2.0] }), &json!({ "a": [1.0, 2] })));
        assert!(!same(&json!({ "a": 1 }), &json!({ "a": 1, "b": 2 })));
        assert!(!same(&json!("1"), &json!(1)));
    }
}
