//! Input set and managed input types.
//!
//! Fields use the API's names, except for the decision nouns
//! (`input_set_id`, `managed_input_id`, `workspace_id`). On the wire they
//! keep the API's names.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::run::Format;

/// An input set: a named group of inputs for experiments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct InputSet {
    /// ID of the input set (API: `id`).
    #[serde(rename = "id")]
    pub input_set_id: String,
    /// ID of the workspace the input set belongs to (API: `app_id`).
    #[serde(rename = "app_id")]
    pub workspace_id: String,
    /// Name of the input set.
    pub name: String,
    /// Description of the input set.
    pub description: String,
    /// IDs of the runs whose inputs the input set holds. Empty for an input
    /// set of managed inputs.
    pub input_ids: Vec<String>,
    /// Managed inputs the input set holds.
    pub inputs: Vec<ManagedInput>,
    /// When the input set was created, in RFC 3339.
    pub created_at: String,
    /// When the input set was last updated, in RFC 3339.
    pub updated_at: String,
}

/// A managed input: an input stored on the platform for reuse.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ManagedInput {
    /// ID of the managed input (API: `id`).
    #[serde(rename = "id")]
    pub managed_input_id: String,
    /// Name of the managed input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Description of the managed input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// ID of the run whose input the managed input was created from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// ID of the upload the managed input was created from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_id: Option<String>,
    /// Format of the managed input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Format>,
    /// When the managed input was created, in RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// When the managed input was last updated, in RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}
