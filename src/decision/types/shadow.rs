//! Shadow test types.
//!
//! Fields use the API's names, except for the decision nouns (`shadow_id`,
//! `workspace_id`, `environment_id`). On the wire they keep the API's
//! names.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::experiment::ExperimentStatus;
use super::run::Run;

/// A shadow test, including its runs.
///
/// A shadow test runs candidate environments next to baseline environments,
/// on the same inputs, to compare their results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ShadowTest {
    /// ID of the shadow test (API: `id`).
    #[serde(rename = "id", default, skip_serializing_if = "Option::is_none")]
    pub shadow_id: Option<String>,
    /// Name of the shadow test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Description of the shadow test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// ID of the workspace the shadow test belongs to (API: `app_id`).
    #[serde(rename = "app_id", default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// When the shadow test was created, in RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// When the shadow test was last updated, in RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Status of the shadow test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ExperimentStatus>,
    /// When the shadow test completed, in RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    /// The baseline environments of the shadow test, each with the
    /// candidate environments compared with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparisons: Option<Vec<TestComparison>>,
    /// When the shadow test starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_events: Option<StartEvents>,
    /// When the shadow test ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub termination_events: Option<TerminationEvents>,
    /// Distributions of the runs' metrics, grouped by environment and
    /// build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grouped_distributional_summaries: Option<Vec<Map<String, Value>>>,
    /// Runs of the shadow test. Only
    /// [`Platform::get_shadow`](crate::decision::platform::Platform::get_shadow)
    /// fills them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs: Option<Vec<Run>>,
}

/// Metadata of a shadow test, without its runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ShadowTestMetadata {
    /// ID of the shadow test (API: `id`).
    #[serde(rename = "id", default, skip_serializing_if = "Option::is_none")]
    pub shadow_id: Option<String>,
    /// Name of the shadow test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Description of the shadow test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// ID of the workspace the shadow test belongs to (API: `app_id`).
    #[serde(rename = "app_id", default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// When the shadow test was created, in RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// When the shadow test was last updated, in RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Status of the shadow test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ExperimentStatus>,
}

/// A baseline environment and the candidate environments a shadow test compares
/// with it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TestComparison {
    /// ID of the baseline environment (API: `baseline_instance_id`).
    #[serde(rename = "baseline_instance_id")]
    pub baseline_environment_id: String,
    /// IDs of the candidate environments that run next to the baseline
    /// environment (API: `candidate_instance_ids`).
    #[serde(rename = "candidate_instance_ids")]
    pub candidate_environment_ids: Vec<String>,
}

/// When a shadow test starts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StartEvents {
    /// When the shadow test starts, in RFC 3339, e.g. `2026-10-13T14:00:00Z`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
}

/// When a shadow test ends: after a number of runs, or at a time.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminationEvents {
    /// Most runs of the baseline environments before the shadow test ends,
    /// from 1 to 300.
    #[schemars(range(min = 1, max = 300))]
    pub maximum_runs: u32,
    /// When the shadow test ends, in RFC 3339, e.g. `2026-10-20T14:00:00Z`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
}

/// How to stop a shadow or switchback test.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum StopIntent {
    /// Mark the test as completed.
    #[default]
    Complete,
    /// Mark the test as canceled.
    Cancel,
}
