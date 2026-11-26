//! Integration types: the account's links to compute and data providers.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::OrUnknown;

/// An integration: a provider, such as Databricks, that runs or feeds the
/// account's workspaces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Integration {
    /// ID of the integration (API: `id`).
    #[serde(rename = "id")]
    pub integration_id: String,
    /// Name of the integration.
    pub name: String,
    /// Description of the integration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether every workspace of the account may use the integration (API:
    /// `global`).
    #[serde(rename = "global", default)]
    pub is_global: bool,
    /// IDs of the workspaces that may use the integration, when it is not
    /// global (API: `application_ids`).
    #[serde(
        rename = "application_ids",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub workspace_ids: Option<Vec<String>>,
    /// Kind of integration (API: `type`).
    #[serde(rename = "type", deserialize_with = "super::or_unknown")]
    pub integration_type: IntegrationType,
    /// Types of binary the integration runs (API: `exec_types`).
    #[serde(rename = "exec_types", default, deserialize_with = "super::or_unknown")]
    pub executable_types: Vec<ExecutableType>,
    /// The provider of the integration.
    #[serde(deserialize_with = "super::or_unknown")]
    pub provider: IntegrationProvider,
    /// The provider's configuration, such as a Databricks job and its
    /// credentials, in clear text. Only a get has it, not a list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_config: Option<Value>,
    /// When the integration was created, in RFC 3339.
    pub created_at: String,
    /// When the integration was last updated, in RFC 3339.
    pub updated_at: String,
}

impl Integration {
    /// The integration without its `provider_config`, which holds the
    /// provider's credentials.
    pub fn without_provider_config(mut self) -> Integration {
        self.provider_config = None;
        self
    }
}

/// Kind of integration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum IntegrationType {
    /// A provider that runs the workspace's runs.
    #[default]
    Compute,
    /// A provider of data.
    Data,
    /// A value this version of the SDK does not know, read from a response
    /// field. Requests should not use it: tool arguments reject it, and tool
    /// schemas leave it out.
    #[serde(skip_deserializing)]
    #[schemars(skip)]
    Unknown,
}

impl OrUnknown for IntegrationType {
    const UNKNOWN: Self = IntegrationType::Unknown;
}

/// The provider of an integration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum IntegrationProvider {
    /// Databricks.
    #[default]
    Dbx,
    /// AWS Batch.
    AwsBatch,
    /// A value this version of the SDK does not know, read from a response
    /// field. Requests should not use it: tool arguments reject it, and tool
    /// schemas leave it out.
    #[serde(skip_deserializing)]
    #[schemars(skip)]
    Unknown,
}

impl OrUnknown for IntegrationProvider {
    const UNKNOWN: Self = IntegrationProvider::Unknown;
}

/// A type of binary that an integration runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ExecutableType {
    /// A Python workspace.
    #[default]
    Python,
    /// A Java workspace.
    Java,
    /// A Go workspace.
    Go,
    /// A compiled binary.
    Binary,
    /// A workspace of a FICO SDK.
    Sdk,
    /// A value this version of the SDK does not know, read from a response
    /// field. Requests should not use it: tool arguments reject it, and tool
    /// schemas leave it out.
    #[serde(skip_deserializing)]
    #[schemars(skip)]
    Unknown,
}

impl OrUnknown for ExecutableType {
    const UNKNOWN: Self = ExecutableType::Unknown;
}

/// The health of an integration, as its provider last reported it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct IntegrationHealth {
    /// The integration's health.
    pub status: IntegrationHealthStatus,
    /// What the provider said about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// When the health was last checked, in RFC 3339.
    pub last_updated: String,
}

/// The health of an integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum IntegrationHealthStatus {
    /// The provider works.
    Healthy,
    /// The provider does not work.
    Unhealthy,
    /// The provider has no health check.
    Unavailable,
    /// A value this version of the SDK does not know.
    #[serde(other)]
    Unknown,
}
