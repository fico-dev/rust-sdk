//! Switchback test types.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::experiment::ExperimentStatus;
use super::run::Run;

/// A switchback test, including its runs.
///
/// Every field is optional, because responses leave some out: only the
/// metadata carries `app_id`, an update returns just the ID, name,
/// description and timestamps, and only
/// [`Platform::get_switchback`](crate::decision::platform::Platform::get_switchback)
/// fills `runs`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct SwitchbackTest {
    /// ID of the switchback test (API: `id`).
    #[serde(rename = "id", default, skip_serializing_if = "Option::is_none")]
    pub switchback_id: Option<String>,
    /// Name of the switchback test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Description of the switchback test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// ID of the workspace the switchback test belongs to (API: `app_id`).
    #[serde(rename = "app_id", default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// When the switchback test was created, in RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// When the switchback test was last updated, in RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Status of the switchback test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ExperimentStatus>,
    /// When the switchback test started, in RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// When the switchback test completed, in RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    /// The environments the switchback test alternates between.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparison: Option<TestComparisonSingle>,
    /// Schedule of the switchback test: which environment runs in each unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<SwitchbackPlan>,
    /// Runs of the switchback test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs: Option<Vec<Run>>,
}

/// Metadata of a switchback test, without its runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct SwitchbackTestMetadata {
    /// ID of the switchback test (API: `id`).
    #[serde(rename = "id", default, skip_serializing_if = "Option::is_none")]
    pub switchback_id: Option<String>,
    /// Name of the switchback test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Description of the switchback test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// ID of the workspace the switchback test belongs to (API: `app_id`).
    #[serde(rename = "app_id", default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// When the switchback test was created, in RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// When the switchback test was last updated, in RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Status of the switchback test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ExperimentStatus>,
}

/// A baseline environment and the candidate environment a switchback test
/// alternates with it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TestComparisonSingle {
    /// ID of the baseline environment (API: `baseline_instance_id`).
    #[serde(rename = "baseline_instance_id")]
    pub baseline_environment_id: String,
    /// ID of the candidate environment (API: `candidate_instance_id`).
    #[serde(rename = "candidate_instance_id")]
    pub candidate_environment_id: String,
}

/// A time unit of a switchback plan, and the environment it runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct SwitchbackPlanUnit {
    /// How long the unit lasts, in minutes.
    pub duration_minutes: f64,
    /// ID of the environment that runs during the unit (API: `instance_id`).
    #[serde(rename = "instance_id")]
    pub environment_id: String,
    /// Position of the unit in the plan, from 0.
    pub index: u32,
}

/// Schedule of a switchback test.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct SwitchbackPlan {
    /// When the plan's first unit begins, in RFC 3339. Starting a test
    /// without one sets it to when the test starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    /// The units of the plan, in order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<Vec<SwitchbackPlanUnit>>,
}
