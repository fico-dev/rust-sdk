//! Acceptance test types.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::OrUnknown;
use super::experiment::ExperimentStatus;
use super::run::Run;

/// How an acceptance test evaluates a metric.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum MetricType {
    /// Compare the candidate's value with the baseline's.
    #[default]
    DirectComparison,
    /// A value this version of the SDK does not know, read from a response
    /// field. Requests should not use it: tool arguments reject it, and tool
    /// schemas leave it out.
    #[serde(skip_deserializing)]
    #[schemars(skip)]
    Unknown,
}

impl OrUnknown for MetricType {
    const UNKNOWN: Self = MetricType::Unknown;
}

/// Statistic of a metric's distribution that an acceptance test compares.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StatisticType {
    /// Minimum.
    #[default]
    Min,
    /// Maximum.
    Max,
    /// Mean.
    Mean,
    /// Standard deviation.
    Std,
    /// Shifted geometric mean.
    ShiftedGeometricMean,
    /// 1st percentile.
    P01,
    /// 5th percentile.
    P05,
    /// 10th percentile.
    P10,
    /// 25th percentile.
    P25,
    /// 50th percentile (the median).
    P50,
    /// 75th percentile.
    P75,
    /// 90th percentile.
    P90,
    /// 95th percentile.
    P95,
    /// 99th percentile.
    P99,
    /// A value this version of the SDK does not know, read from a response
    /// field. Requests should not use it: tool arguments reject it, and tool
    /// schemas leave it out.
    #[serde(skip_deserializing)]
    #[schemars(skip)]
    Unknown,
}

impl OrUnknown for StatisticType {
    const UNKNOWN: Self = StatisticType::Unknown;
}

/// Operator that compares the candidate's statistic with the baseline's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum Comparison {
    /// Equal to.
    #[default]
    #[serde(rename = "eq")]
    EqualTo,
    /// Greater than.
    #[serde(rename = "gt")]
    GreaterThan,
    /// Greater than or equal to.
    #[serde(rename = "ge")]
    GreaterThanOrEqualTo,
    /// Less than.
    #[serde(rename = "lt")]
    LessThan,
    /// Less than or equal to.
    #[serde(rename = "le")]
    LessThanOrEqualTo,
    /// Not equal to.
    #[serde(rename = "ne")]
    NotEqualTo,
    /// A value this version of the SDK does not know, read from a response
    /// field. Requests should not use it: tool arguments reject it, and tool
    /// schemas leave it out.
    #[serde(skip_deserializing)]
    #[serde(rename = "unknown")]
    #[schemars(skip)]
    Unknown,
}

impl OrUnknown for Comparison {
    const UNKNOWN: Self = Comparison::Unknown;
}

/// How a metric's tolerance is measured.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum MetricToleranceType {
    /// No tolerance type.
    #[default]
    #[serde(rename = "")]
    Undefined,
    /// An absolute difference.
    Absolute,
    /// A relative difference.
    Relative,
    /// A value this version of the SDK does not know, read from a response
    /// field. Requests should not use it: tool arguments reject it, and tool
    /// schemas leave it out.
    #[serde(skip_deserializing)]
    #[schemars(skip)]
    Unknown,
}

impl OrUnknown for MetricToleranceType {
    const UNKNOWN: Self = MetricToleranceType::Unknown;
}

/// How far the candidate may be from the baseline and still pass.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MetricTolerance {
    /// How the tolerance is measured (API: `type`).
    #[serde(rename = "type", deserialize_with = "super::or_unknown")]
    pub tolerance_type: MetricToleranceType,
    /// Size of the tolerance, at least 0: an absolute difference, or for
    /// `relative`, a percentage of the baseline's value, where `5` is 5%.
    pub value: f64,
}

/// Comparison operator and tolerance of an acceptance test metric.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MetricParams {
    /// Operator that compares the candidate's statistic with the baseline's.
    #[serde(deserialize_with = "super::or_unknown")]
    pub operator: Comparison,
    /// How far the candidate may be from the baseline and still pass. The
    /// platform takes no tolerance for `ne`, so set its value to 0.
    pub tolerance: MetricTolerance,
}

/// A metric that an acceptance test checks.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Metric {
    /// Key of the metric in the runs' metrics, such as `result.value`.
    pub field: String,
    /// How the metric is evaluated.
    #[serde(deserialize_with = "super::or_unknown")]
    pub metric_type: MetricType,
    /// Comparison operator and tolerance of the metric.
    pub params: MetricParams,
    /// Statistic that sums up the metric's values over the runs of each
    /// environment, for the comparison.
    #[serde(deserialize_with = "super::or_unknown")]
    pub statistic: StatisticType,
}

/// An environment of an acceptance test, and the build it ran.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ComparisonEnvironment {
    /// ID of the environment (API: `instance_id`).
    #[serde(rename = "instance_id")]
    pub environment_id: String,
    /// ID of the build the environment ran (API: `version_id`). The
    /// platform leaves it out when it has none.
    #[serde(
        rename = "version_id",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub build_id: Option<String>,
}

/// Summary statistics of a metric's distribution.
///
/// A statistic that is not a finite number, such as the mean of no values,
/// is `NaN` or an infinity. In JSON it is the string `"nan"`, `"+inf"` or
/// `"-inf"`, as the platform sends it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct DistributionSummaryStatistics {
    /// Smallest value.
    #[serde(with = "non_finite")]
    #[schemars(with = "non_finite::Schema")]
    pub min: f64,
    /// Largest value.
    #[serde(with = "non_finite")]
    #[schemars(with = "non_finite::Schema")]
    pub max: f64,
    /// Number of values.
    pub count: u64,
    /// Mean.
    #[serde(with = "non_finite")]
    #[schemars(with = "non_finite::Schema")]
    pub mean: f64,
    /// Standard deviation.
    #[serde(with = "non_finite")]
    #[schemars(with = "non_finite::Schema")]
    pub std: f64,
    /// Shifted geometric mean.
    #[serde(with = "non_finite")]
    #[schemars(with = "non_finite::Schema")]
    pub shifted_geometric_mean: f64,
    /// Shift of the shifted geometric mean.
    #[serde(with = "non_finite")]
    #[schemars(with = "non_finite::Schema")]
    pub shift_parameter: f64,
}

/// Percentiles of a metric's distribution.
///
/// A percentile that is not a finite number, such as one of no values, is
/// `NaN` or an infinity. In JSON it is the string `"nan"`, `"+inf"` or
/// `"-inf"`, as the platform sends it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct DistributionPercentiles {
    /// 1st percentile.
    #[serde(with = "non_finite")]
    #[schemars(with = "non_finite::Schema")]
    pub p01: f64,
    /// 5th percentile.
    #[serde(with = "non_finite")]
    #[schemars(with = "non_finite::Schema")]
    pub p05: f64,
    /// 10th percentile.
    #[serde(with = "non_finite")]
    #[schemars(with = "non_finite::Schema")]
    pub p10: f64,
    /// 25th percentile.
    #[serde(with = "non_finite")]
    #[schemars(with = "non_finite::Schema")]
    pub p25: f64,
    /// 50th percentile (the median).
    #[serde(with = "non_finite")]
    #[schemars(with = "non_finite::Schema")]
    pub p50: f64,
    /// 75th percentile.
    #[serde(with = "non_finite")]
    #[schemars(with = "non_finite::Schema")]
    pub p75: f64,
    /// 90th percentile.
    #[serde(with = "non_finite")]
    #[schemars(with = "non_finite::Schema")]
    pub p90: f64,
    /// 95th percentile.
    #[serde(with = "non_finite")]
    #[schemars(with = "non_finite::Schema")]
    pub p95: f64,
    /// 99th percentile.
    #[serde(with = "non_finite")]
    #[schemars(with = "non_finite::Schema")]
    pub p99: f64,
}

/// Statistics of a metric for one environment of an acceptance test.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ResultStatistics {
    /// ID of the environment (API: `instance_id`).
    #[serde(rename = "instance_id")]
    pub environment_id: String,
    /// ID of the build the environment ran (API: `version_id`).
    #[serde(rename = "version_id")]
    pub build_id: String,
    /// Number of runs of the environment.
    pub number_of_runs_total: u64,
    /// Summary statistics of the metric's values.
    pub distribution_summary_statistics: DistributionSummaryStatistics,
    /// Percentiles of the metric's values.
    pub distribution_percentiles: DistributionPercentiles,
}

/// Statistics of a metric for the baseline and the candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct MetricStatistics {
    /// Statistics of the baseline environment.
    pub control: ResultStatistics,
    /// Statistics of the candidate environment.
    pub candidate: ResultStatistics,
}

/// Result of one metric of an acceptance test.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct MetricResult {
    /// The metric.
    pub metric: Metric,
    /// Statistics of the metric for the baseline and the candidate.
    pub statistics: MetricStatistics,
    /// The metric's values that the test compared, by the ID of the input
    /// of the runs: the baseline's value, then the candidate's. A side
    /// whose run lacks the metric has no value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric_pairs: Option<BTreeMap<String, [Option<f64>; 2]>>,
    /// Whether the candidate passed the metric.
    pub passed: bool,
}

/// Results of an acceptance test.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct AcceptanceTestResults {
    /// Whether the candidate passed every metric.
    pub passed: bool,
    /// Result of each metric.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric_results: Option<Vec<MetricResult>>,
    /// Why the results could not be computed, such as runs that did not
    /// succeed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// An acceptance test: compares a candidate environment with a baseline on
/// metrics.
///
/// The baseline is `control`, as in the API. An update returns only the ID,
/// name, description and timestamps, and only a get returns `status` and
/// `results`, which come once the test has completed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct AcceptanceTest {
    /// ID of the acceptance test (API: `id`).
    #[serde(rename = "id")]
    pub acceptance_id: String,
    /// Name of the acceptance test.
    pub name: String,
    /// Description of the acceptance test.
    pub description: String,
    /// When the acceptance test was created, in RFC 3339.
    pub created_at: String,
    /// When the acceptance test was last updated, in RFC 3339.
    pub updated_at: String,
    /// ID of the workspace the acceptance test belongs to (API: `app_id`).
    #[serde(rename = "app_id", default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// ID of the experiment that runs the acceptance test's runs. It is the
    /// acceptance test's ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment_id: Option<String>,
    /// The baseline environment, and the build it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<ComparisonEnvironment>,
    /// The candidate environment, and the build it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<ComparisonEnvironment>,
    /// The metrics that the acceptance test checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Vec<Metric>>,
    /// Status of the acceptance test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ExperimentStatus>,
    /// Status of the acceptance test's results. `None` until the platform
    /// computes them.
    #[serde(
        default,
        deserialize_with = "super::empty_as_none",
        skip_serializing_if = "Option::is_none"
    )]
    pub results_status: Option<AcceptanceResultsStatus>,
    /// Results of the acceptance test, once it has completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub results: Option<AcceptanceTestResults>,
    /// Runs of the acceptance test. Only
    /// [`Platform::get_acceptance`](crate::decision::platform::Platform::get_acceptance)
    /// fills them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs: Option<Vec<Run>>,
}

/// Status of an acceptance test's results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum AcceptanceResultsStatus {
    /// The results were computed.
    Succeeded,
    /// The results could not be computed.
    Failed,
    /// The results are being computed.
    Pending,
    /// A value this version of the SDK does not know.
    #[serde(other)]
    Unknown,
}

/// Reads and writes an `f64` that may not be finite, as the platform does:
/// a JSON number, or the string `"nan"`, `"+inf"` (also `"inf"`) or
/// `"-inf"`. Plain `f64` fields reject the strings, and `serde_json` writes
/// non-finite values as `null`.
mod non_finite {
    use schemars::JsonSchema;
    use serde::{Deserialize, Deserializer, Serializer};

    /// A JSON number, or one of the strings for a value that is not finite.
    #[derive(Deserialize, JsonSchema)]
    #[serde(untagged)]
    pub(super) enum Schema {
        /// A finite number.
        Number(f64),
        /// `"nan"`, `"+inf"` or `"-inf"`.
        Text(String),
    }

    pub(super) fn serialize<S: Serializer>(value: &f64, s: S) -> Result<S::Ok, S::Error> {
        if value.is_nan() {
            s.serialize_str("nan")
        } else if *value == f64::INFINITY {
            s.serialize_str("+inf")
        } else if *value == f64::NEG_INFINITY {
            s.serialize_str("-inf")
        } else {
            s.serialize_f64(*value)
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        match Schema::deserialize(d)? {
            Schema::Number(value) => Ok(value),
            Schema::Text(text) => match text.as_str() {
                "nan" => Ok(f64::NAN),
                "+inf" | "inf" => Ok(f64::INFINITY),
                "-inf" => Ok(f64::NEG_INFINITY),
                _ => Err(serde::de::Error::custom(format!(
                    "invalid number {text:?}, expected a number, \"nan\", \"+inf\" or \"-inf\""
                ))),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    /// Statistics as the platform sends them, with values that are not
    /// finite.
    fn statistics_json() -> Value {
        json!({
            "instance_id": "production",
            "version_id": "v1",
            "number_of_runs_total": 2,
            "distribution_summary_statistics": {
                "min": 10.0,
                "max": "+inf",
                "count": 2,
                "mean": "inf",
                "std": "nan",
                "shifted_geometric_mean": "-inf",
                "shift_parameter": 0.0,
            },
            "distribution_percentiles": {
                "p01": 10, "p05": 10, "p10": 10, "p25": 10, "p50": 10.5,
                "p75": 11, "p90": 11, "p95": 11, "p99": 11,
            },
        })
    }

    #[test]
    fn reads_and_writes_values_that_are_not_finite() {
        let statistics: ResultStatistics = serde_json::from_value(statistics_json()).unwrap();
        let summary = &statistics.distribution_summary_statistics;
        assert_eq!(summary.min, 10.0);
        assert_eq!(summary.max, f64::INFINITY);
        assert_eq!(summary.mean, f64::INFINITY);
        assert!(summary.std.is_nan());
        assert_eq!(summary.shifted_geometric_mean, f64::NEG_INFINITY);
        assert_eq!(statistics.distribution_percentiles.p50, 10.5);
        let written = serde_json::to_value(&statistics).unwrap();
        let summary = &written["distribution_summary_statistics"];
        assert_eq!(summary["max"], "+inf");
        assert_eq!(summary["mean"], "+inf");
        assert_eq!(summary["std"], "nan");
        assert_eq!(summary["shifted_geometric_mean"], "-inf");
        assert_eq!(summary["min"], 10.0);
        let mut bad = statistics_json();
        bad["distribution_summary_statistics"]["std"] = json!("infinity");
        let err = serde_json::from_value::<ResultStatistics>(bad).unwrap_err();
        assert!(err.to_string().contains("\"infinity\""), "{err}");
    }

    #[test]
    fn writes_metrics_with_api_names() {
        let metric = Metric {
            field: "result.value".to_owned(),
            metric_type: MetricType::DirectComparison,
            params: MetricParams {
                operator: Comparison::LessThanOrEqualTo,
                tolerance: MetricTolerance {
                    tolerance_type: MetricToleranceType::Relative,
                    value: 5.0,
                },
            },
            statistic: StatisticType::Mean,
        };
        let wire = json!({
            "field": "result.value",
            "metric_type": "direct-comparison",
            "params": {
                "operator": "le",
                "tolerance": { "type": "relative", "value": 5.0 },
            },
            "statistic": "mean",
        });
        assert_eq!(serde_json::to_value(&metric).unwrap(), wire);
        assert_eq!(serde_json::from_value::<Metric>(wire).unwrap(), metric);
    }

    #[test]
    fn reads_new_metric_values_from_responses_as_unknown() {
        let metric: Metric = serde_json::from_value(json!({
            "field": "result.value",
            "metric_type": "absolute-tolerance",
            "params": {
                "operator": "approx",
                "tolerance": { "type": "percent", "value": 1 },
            },
            "statistic": "p999",
        }))
        .unwrap();
        assert_eq!(metric.metric_type, MetricType::Unknown);
        assert_eq!(metric.params.operator, Comparison::Unknown);
        assert_eq!(
            metric.params.tolerance.tolerance_type,
            MetricToleranceType::Unknown
        );
        assert_eq!(metric.statistic, StatisticType::Unknown);
    }
}
