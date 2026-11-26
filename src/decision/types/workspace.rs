//! Platform workspace types.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::build::BuildExecutable;

/// A workspace on the platform (an application in the API).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Workspace {
    /// ID of the workspace (API: `id`).
    #[serde(rename = "id")]
    pub workspace_id: String,
    /// Name of the workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Description of the workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Kind of workspace.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub workspace_type: Option<WorkspaceType>,
    /// ID of the environment that runs use by default (API:
    /// `default_instance`).
    #[serde(
        rename = "default_instance",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub default_environment_id: Option<String>,
    /// ID of the environment that experiments use by default (API:
    /// `default_experiment_instance`).
    #[serde(
        rename = "default_experiment_instance",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub default_experiment_environment: Option<String>,
    /// ID of the marketplace subscription, for a subscription workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_id: Option<String>,
    /// Whether the workspace is locked.
    #[serde(default)]
    pub locked: bool,
    /// The binary that the workspace's last push uploaded, which `latest`
    /// runs. Only a push changes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_executable: Option<BuildExecutable>,
    /// The workspace's settings, such as the links the console shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<WorkspaceSettings>,
    /// Where the deletion of the workspace is, while the platform deletes
    /// it: `App deletion in progress` or `App deletion failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletion_status: Option<String>,
    /// When the workspace was created, in RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// When the workspace was last updated, in RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// A workspace's settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct WorkspaceSettings {
    /// Links that the console shows on the workspace's page.
    pub links: Vec<WorkspaceLink>,
}

/// A link that the console shows on a workspace's page.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct WorkspaceLink {
    /// Text of the link, 1 to 64 characters.
    pub label: String,
    /// Where the link goes: a URL of at most 2,048 characters.
    pub url: String,
    /// Description of the link, at most 256 characters.
    pub description: String,
    /// Tags of the link: at most 10, each 1 to 64 characters.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// Kind of workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum WorkspaceType {
    /// A custom workspace.
    Custom,
    /// A workspace subscribed to one in the marketplace.
    Subscription,
    /// A pipeline workspace.
    Pipeline,
    /// A value this version of the SDK does not know.
    #[serde(other)]
    Unknown,
}
