//! Build types.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::account::AccountMemberRole;
use super::manifest::{ControlType, ManifestOptionType};
use super::run::ContentFormat;

/// A build: an immutable snapshot of a workspace's binary (a version in the
/// API).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Build {
    /// ID of the build (API: `id`).
    #[serde(rename = "id")]
    pub build_id: String,
    /// ID of the workspace the build belongs to (API: `application_id`).
    #[serde(rename = "application_id")]
    pub workspace_id: String,
    /// Name of the build.
    pub name: String,
    /// Description of the build.
    pub description: String,
    /// The binary the build runs. The platform leaves it out when the
    /// build has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<BuildExecutable>,
    /// When the build was created, in RFC 3339.
    pub created_at: String,
    /// When the build was last updated, in RFC 3339.
    pub updated_at: String,
}

/// The binary a build runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct BuildExecutable {
    /// ID of the binary (API: `id`).
    #[serde(rename = "id")]
    pub executable_id: String,
    /// Email of the user who uploaded the binary.
    pub user_email: String,
    /// When the binary was uploaded, in RFC 3339.
    pub uploaded_at: String,
    /// What the binary needs to run.
    pub requirements: BuildExecutableRequirements,
}

/// What a build's binary needs to run: its type, runtime, entry point,
/// options and the format of its input and output, as push sent them from
/// the manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct BuildExecutableRequirements {
    /// Type of the binary, from the manifest's `type`, e.g. `go` or
    /// `python`.
    pub executable_type: String,
    /// Runtime the binary runs in, from the manifest's `runtime`, e.g.
    /// `ghcr.io/nextmv-io/runtime/default:latest`.
    pub runtime: String,
    /// File that runs the binary, relative to its directory, from the
    /// manifest's `execution.entrypoint`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    /// Directory the binary runs in, relative to its directory, from the
    /// manifest's `execution.cwd`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    /// Options the binary accepts, from the manifest's `configuration.options`,
    /// without the local-only ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<BuildOptions>,
    /// Format of the binary's input and output, from the manifest's
    /// `configuration.content`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io_configuration: Option<BuildIoConfiguration>,
}

/// Options that a build's binary accepts, and how they are validated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct BuildOptions {
    /// Whether runs may only set the listed options.
    #[serde(default)]
    pub strict: bool,
    /// How strictly the options are validated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation: Option<BuildOptionsValidation>,
    /// How runs pass the options to the binary as command-line arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<BuildOptionsFormat>,
    /// The options.
    #[serde(default)]
    pub items: Vec<BuildOption>,
}

/// How strictly a build's options are validated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct BuildOptionsValidation {
    /// Which option rules a run must follow to be created: `none` checks no
    /// rule, and `all` checks every rule.
    pub enforce: String,
}

/// How runs pass a build's options to its binary as command-line
/// arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct BuildOptionsFormat {
    /// For each option, every entry becomes an argument, with `{{name}}` and
    /// `{{value}}` replaced by the option's name and value.
    pub template: Vec<String>,
}

/// An option that a build's binary accepts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct BuildOption {
    /// Name of the option.
    pub name: String,
    /// Description of the option.
    #[serde(default)]
    pub description: String,
    /// Type of the option's value.
    #[serde(deserialize_with = "super::or_unknown")]
    pub option_type: ManifestOptionType,
    /// Whether runs must set the option.
    #[serde(default)]
    pub required: bool,
    /// Default value of the option.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// Attributes that constrain the option's value in the console, such
    /// as `min` and `max`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_attributes: Option<BTreeMap<String, Value>>,
    /// How the console shows the option.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<BuildOptionUi>,
}

/// How the console shows a build's option.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct BuildOptionUi {
    /// Console control that edits the option.
    #[serde(
        default,
        deserialize_with = "super::or_unknown",
        skip_serializing_if = "Option::is_none"
    )]
    pub control_type: Option<ControlType>,
    /// Account roles that the console hides the option from.
    #[serde(
        default,
        deserialize_with = "super::or_unknown",
        skip_serializing_if = "Option::is_none"
    )]
    pub hidden_from: Option<Vec<AccountMemberRole>>,
    /// Name that the console shows for the option.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// Format of a build's input and output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct BuildIoConfiguration {
    /// Format of the input and output.
    #[serde(deserialize_with = "super::or_unknown")]
    pub format: ContentFormat,
    /// Where a multi-file binary reads its input and writes its output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_file: Option<BuildMultiFile>,
}

/// Where a multi-file build reads its input and writes its output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct BuildMultiFile {
    /// Directory the binary reads its input files from, relative to its
    /// working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_path: Option<String>,
    /// Where the binary writes its output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_configuration: Option<BuildMultiFileOutput>,
}

/// Where a multi-file build writes its output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct BuildMultiFileOutput {
    /// File the binary writes its metrics to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_path: Option<String>,
    /// File the binary writes its assets to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assets_path: Option<String>,
    /// Directory the binary writes its solution files to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solutions_path: Option<String>,
}
