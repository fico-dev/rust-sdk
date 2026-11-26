//! Ensemble definition types.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::OrUnknown;

/// An ensemble definition: groups of runs, and the rules that pick the best
/// run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct EnsembleDefinition {
    /// ID of the ensemble definition (API: `id`).
    #[serde(rename = "id")]
    pub ensemble_id: String,
    /// ID of the workspace the ensemble definition belongs to (API:
    /// `application_id`).
    #[serde(rename = "application_id")]
    pub workspace_id: String,
    /// Name of the ensemble definition.
    #[serde(default)]
    pub name: String,
    /// Description of the ensemble definition.
    #[serde(default)]
    pub description: String,
    /// Groups of child runs that each ensemble run starts.
    pub run_groups: Vec<RunGroup>,
    /// Rules that pick the result of an ensemble run from its child runs.
    pub rules: Vec<EvaluationRule>,
    /// When the ensemble definition was created, in RFC 3339.
    pub created_at: String,
    /// When the ensemble definition was last updated, in RFC 3339.
    pub updated_at: String,
}

/// A group of runs of one environment in an ensemble.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunGroup {
    /// ID of the run group (API: `id`).
    #[serde(rename = "id")]
    pub group_id: String,
    /// ID of the environment that the run group's runs use (API:
    /// `instance_id`).
    #[serde(rename = "instance_id")]
    pub environment_id: String,
    /// Options that the run group's runs use, as string key-value pairs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<BTreeMap<String, String>>,
    /// How many times the run group's run is repeated after the first one,
    /// with the same environment and options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repetitions: Option<u32>,
}

/// Whether an evaluation rule prefers higher or lower values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum RuleObjective {
    /// Prefer higher values.
    #[default]
    Maximize,
    /// Prefer lower values.
    Minimize,
    /// A value this version of the SDK does not know, read from a response
    /// field. Requests should not use it: tool arguments reject it, and tool
    /// schemas leave it out.
    #[serde(skip_deserializing)]
    #[schemars(skip)]
    Unknown,
}

impl OrUnknown for RuleObjective {
    const UNKNOWN: Self = RuleObjective::Unknown;
}

/// How an evaluation rule's tolerance is measured.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum RuleToleranceType {
    /// An absolute difference from the best value.
    #[default]
    Absolute,
    /// A difference relative to the best value, where `1` is 100%.
    Relative,
    /// A value this version of the SDK does not know, read from a response
    /// field. Requests should not use it: tool arguments reject it, and tool
    /// schemas leave it out.
    #[serde(skip_deserializing)]
    #[schemars(skip)]
    Unknown,
}

impl OrUnknown for RuleToleranceType {
    const UNKNOWN: Self = RuleToleranceType::Unknown;
}

/// How close to the best value a run must be to count as tied.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RuleTolerance {
    /// Largest difference from the best value that still counts as tied.
    pub value: f64,
    /// How the difference is measured (API: `type`).
    #[serde(rename = "type", deserialize_with = "super::or_unknown")]
    pub tolerance_type: RuleToleranceType,
}

/// A rule that an ensemble uses to pick the best run.
///
/// Rules apply in `index` order: the runs tied with the best run under one
/// rule compete under the next. If several are still tied after the last
/// rule, the platform picks one at random.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EvaluationRule {
    /// ID of the rule (API: `id`).
    #[serde(rename = "id")]
    pub rule_id: String,
    /// Path to the value that the rule compares, as a JSONPath into each
    /// run's metrics, such as `$.result.value` (API: `statistics_path`).
    #[serde(rename = "statistics_path")]
    pub metrics_path: String,
    /// Whether the rule prefers higher or lower values.
    #[serde(deserialize_with = "super::or_unknown")]
    pub objective: RuleObjective,
    /// How close to the best value a run must be to count as tied.
    pub tolerance: RuleTolerance,
    /// Position of the rule, from 0. Lower indices apply first.
    pub index: u32,
}
