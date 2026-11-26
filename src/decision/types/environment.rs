//! Environment types.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::run::{Format, RunQueuing};

/// An environment: a build and the configuration to run it with (an instance in
/// the API).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Environment {
    /// ID of the environment (API: `id`).
    #[serde(rename = "id")]
    pub environment_id: String,
    /// ID of the workspace the environment belongs to (API: `application_id`).
    #[serde(rename = "application_id")]
    pub workspace_id: String,
    /// ID of the build the environment runs (API: `version_id`).
    #[serde(rename = "version_id")]
    pub build_id: String,
    /// Name of the environment.
    pub name: String,
    /// Description of the environment.
    pub description: String,
    /// Configuration the environment runs its build with.
    pub configuration: EnvironmentConfiguration,
    /// Whether the environment is locked. The platform refuses to update or
    /// delete a locked environment until it is unlocked.
    pub locked: bool,
    /// When the environment was created, in RFC 3339.
    pub created_at: String,
    /// When the environment was last updated, in RFC 3339.
    pub updated_at: String,
}

/// Configuration an environment runs its build with.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EnvironmentConfiguration {
    /// Execution class to run on, i.e. the machine size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_class: Option<String>,
    /// Format of the runs' input. Only its input format is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Format>,
    /// Options that every run of the environment uses, by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<BTreeMap<String, String>>,
    /// ID of the secrets collection to give the environment's runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets_collection_id: Option<String>,
    /// Queuing settings of the environment's runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queuing: Option<RunQueuing>,
    /// ID of the integration to run through. Needs the `integration`
    /// execution class, or none, which then becomes `integration`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration_id: Option<String>,
}
