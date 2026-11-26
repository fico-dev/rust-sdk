//! Workspace manifest (`workspace.yaml`) types.
//!
//! Users write manifests, so these are input-side types, and their enums
//! read strictly. Fields use the YAML keys. A field with a default takes
//! it when the file leaves the field out, and some also read `null` as it.
//! The other fields are `Option`s.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::OrUnknown;
use super::account::AccountMemberRole;
use super::run::ContentFormat;

/// A workspace manifest, `workspace.yaml`: how to build, push and run a workspace.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Manifest {
    /// Language or kind of the workspace. Defaults to `python`.
    #[serde(rename = "type", default)]
    pub manifest_type: ManifestType,
    /// Runtime image the workspace runs in. Defaults to the Python 3.11 runtime.
    #[serde(default)]
    pub runtime: ManifestRuntime,
    /// Python settings, for Python workspaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<ManifestPython>,
    /// Files and directories of the workspace to bundle, relative to the
    /// workspace's directory.
    /// Globs such as `configs/*.json` are supported, a trailing `/` stands
    /// for everything in a directory, and a leading `!` leaves out the files
    /// a pattern matches. At least one is required.
    pub files: Vec<String>,
    /// Configuration of the workspace: its options and content format.
    #[serde(default, deserialize_with = "or_default")]
    pub configuration: ManifestConfiguration,
    /// How to build the workspace before it is pushed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<ManifestBuild>,
    /// Command to run before the workspace is pushed, after the build command. It
    /// runs through the shell (`bash -c` on Linux and macOS, `cmd /c` on
    /// Windows) and must exit with status 0 for the push to go on. Also read
    /// from the key `pre_push`.
    #[serde(
        rename = "pre-push",
        alias = "pre_push",
        default,
        deserialize_with = "folded",
        skip_serializing_if = "Option::is_none"
    )]
    pub pre_push: Option<String>,
    /// How to run the workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ManifestExecution>,
}

/// Language or kind of a workspace.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ManifestType {
    /// A Python workspace.
    #[default]
    Python,
    /// A Go workspace.
    Go,
    /// A Java workspace.
    Java,
    /// A prebuilt binary.
    Binary,
}

/// Runtime image a workspace runs in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum ManifestRuntime {
    /// The default runtime, for binaries.
    #[serde(rename = "ghcr.io/nextmv-io/runtime/default:latest")]
    Default,
    /// Python 3.11.
    #[default]
    #[serde(rename = "ghcr.io/nextmv-io/runtime/python:3.11")]
    Python,
    /// Java.
    #[serde(rename = "ghcr.io/nextmv-io/runtime/java:latest")]
    Java,
    /// Python with Pyomo.
    #[serde(rename = "ghcr.io/nextmv-io/runtime/pyomo:latest")]
    Pyomo,
    /// Python with NVIDIA cuOpt.
    #[serde(rename = "ghcr.io/nextmv-io/runtime/cuopt:latest")]
    Cuopt,
    /// Python with GAMSPy.
    #[serde(rename = "ghcr.io/nextmv-io/runtime/gamspy:latest")]
    Gamspy,
}

/// CPU architecture to bundle Python dependencies for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ManifestPythonArch {
    /// 64-bit ARM.
    #[default]
    Arm64,
    /// 64-bit x86.
    Amd64,
}

/// How to build the workspace: a command and its environment.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ManifestBuild {
    /// Command that builds the workspace before it is pushed, before the pre-push
    /// command. It runs directly, not through a shell, and must exit with
    /// status 0 for the push to go on.
    #[serde(
        default,
        deserialize_with = "folded",
        skip_serializing_if = "Option::is_none"
    )]
    pub command: Option<String>,
    /// Environment variables to set for the build command, with their
    /// values as written, e.g. `3.10` or `017`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<BTreeMap<String, String>>,
}

/// Python settings of the workspace: requirements, architecture and version.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ManifestPython {
    /// Python packages to bundle with the workspace: the path of a
    /// `requirements.txt` or `pyproject.toml` file, or a list of packages.
    /// Also read from the key `pip_requirements`.
    #[serde(
        rename = "pip-requirements",
        alias = "pip_requirements",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub pip_requirements: Option<ManifestPipRequirements>,
    /// CPU architecture the workspace runs on. `arm64` when it is not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<ManifestPythonArch>,
    /// Python version the workspace runs with, e.g. `3.12`. `3.11` when it is not
    /// set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Python packages to bundle with a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
#[non_exhaustive]
pub enum ManifestPipRequirements {
    /// Path of a `requirements.txt` or `pyproject.toml` file, relative to
    /// the workspace's directory.
    File(String),
    /// Package specifiers, e.g. `ortools==9.12.4544`.
    Packages(Vec<String>),
}

/// Console control that edits an option.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ControlType {
    /// A text input.
    #[default]
    Input,
    /// A drop-down list.
    Select,
    /// A list that allows several values.
    Multiselect,
    /// A slider.
    Slider,
    /// A toggle.
    Toggle,
    /// A value this version of the SDK does not know, read from a build's
    /// options. A manifest cannot use it, and tool schemas leave it out.
    #[serde(skip_deserializing)]
    #[schemars(skip)]
    Unknown,
}

impl OrUnknown for ControlType {
    const UNKNOWN: Self = ControlType::Unknown;
}

/// Type of an option's value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ManifestOptionType {
    /// A string.
    #[default]
    String,
    /// A boolean.
    Bool,
    /// An integer.
    Int,
    /// A floating-point number.
    Float,
    /// A value this version of the SDK does not know, read from a build's
    /// options. A manifest cannot use it, and tool schemas leave it out.
    #[serde(skip_deserializing)]
    #[schemars(skip)]
    Unknown,
}

impl OrUnknown for ManifestOptionType {
    const UNKNOWN: Self = ManifestOptionType::Unknown;
}

/// How the console shows an option.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ManifestOptionUi {
    /// Console control that edits the option. Which controls fit depends on
    /// the option's type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_type: Option<ControlType>,
    /// Account roles that the console hides the option from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden_from: Option<Vec<AccountMemberRole>>,
    /// Name that the console shows for the option.
    #[serde(
        default,
        deserialize_with = "folded",
        skip_serializing_if = "Option::is_none"
    )]
    pub display_name: Option<String>,
}

/// An option that the workspace accepts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ManifestOption {
    /// Name of the option.
    pub name: String,
    /// Type of the option's value. Also read from the key `type`.
    #[serde(alias = "type")]
    pub option_type: ManifestOptionType,
    /// Default value of the option.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// Description of the option.
    #[serde(default, deserialize_with = "folded_or_empty")]
    pub description: String,
    /// Whether runs must set the option.
    #[serde(default)]
    pub required: bool,
    /// Whether only local runs use the option, so it is never sent to the
    /// platform. A required option cannot be local only.
    #[serde(default)]
    pub local_only: bool,
    /// Attributes that constrain the option's value in the console, by
    /// option type and control: `min_length` and `max_length` for a string
    /// input; `min`, `max` and `step` for a number input or slider (required
    /// for a slider); `values` for a select or multiselect (required).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_attributes: Option<BTreeMap<String, Value>>,
    /// How the console shows the option.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<ManifestOptionUi>,
}

/// How strictly the workspace's options are validated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ManifestValidation {
    /// Which option rules a run must follow to be created: `none` (the
    /// default) checks no rule, and `all` checks every rule.
    pub enforce: String,
}

impl Default for ManifestValidation {
    fn default() -> ManifestValidation {
        ManifestValidation {
            enforce: "none".to_owned(),
        }
    }
}

/// Options that the workspace accepts, and how they are validated.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ManifestOptions {
    /// Whether runs may only set the listed options.
    #[serde(default, deserialize_with = "or_default")]
    pub strict: bool,
    /// How strictly the options are validated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation: Option<ManifestValidation>,
    /// The options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<ManifestOption>>,
    /// How runs pass the options to the workspace as command-line arguments: for
    /// each option, every entry becomes an argument, with `{{name}}` and
    /// `{{value}}` replaced by the option's name and value. For example,
    /// `["--{{name}}={{value}}"]` passes `--duration=30`. When it is not
    /// set, local runs pass `-duration 30`, and the platform
    /// `-duration=30`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Vec<String>>,
}

/// Where a multi-file workspace reads its input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ManifestContentMultiFileInput {
    /// Directory the workspace reads its input files from, relative to the workspace.
    /// Defaults to `inputs/`.
    pub path: String,
}

impl Default for ManifestContentMultiFileInput {
    fn default() -> ManifestContentMultiFileInput {
        ManifestContentMultiFileInput {
            path: "inputs/".to_owned(),
        }
    }
}

/// Where a multi-file workspace writes its metrics, assets and solutions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ManifestContentMultiFileOutput {
    /// File the workspace writes its metrics to, relative to the workspace. Defaults to
    /// `outputs/metrics.json`.
    pub metrics: String,
    /// File the workspace writes its assets to, relative to the workspace. Defaults to
    /// `outputs/assets.json`.
    pub assets: String,
    /// Directory the workspace writes its solution files to, relative to the workspace.
    /// Defaults to `outputs/solutions/`.
    pub solutions: String,
}

impl Default for ManifestContentMultiFileOutput {
    fn default() -> ManifestContentMultiFileOutput {
        ManifestContentMultiFileOutput {
            metrics: "outputs/metrics.json".to_owned(),
            assets: "outputs/assets.json".to_owned(),
            solutions: "outputs/solutions/".to_owned(),
        }
    }
}

/// Input and output locations of a multi-file workspace.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ManifestContentMultiFile {
    /// Where the workspace reads its input.
    #[serde(default, deserialize_with = "or_default")]
    pub input: ManifestContentMultiFileInput,
    /// Where the workspace writes its output.
    #[serde(default, deserialize_with = "or_default")]
    pub output: ManifestContentMultiFileOutput,
}

/// Format of the workspace's input and output content.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ManifestContent {
    /// Format of the workspace's input and output: `json` reads JSON from stdin
    /// and writes it to stdout, and `multi-file` reads and writes files.
    /// Defaults to `json`.
    #[serde(default)]
    pub format: ContentFormat,
    /// Where a `multi-file` workspace reads and writes its files. Only for the
    /// `multi-file` format, which uses the default locations when it is not
    /// set. Also read from the key `multi_file`.
    #[serde(
        rename = "multi-file",
        alias = "multi_file",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub multi_file: Option<ManifestContentMultiFile>,
}

/// Configuration of the workspace: its options and content format.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ManifestConfiguration {
    /// Options that the workspace accepts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<ManifestOptions>,
    /// Format of the workspace's input and output.
    #[serde(default, deserialize_with = "or_default")]
    pub content: ManifestContent,
}

/// How to run the workspace: its entry point and working directory.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ManifestExecution {
    /// Entry point of the workspace, relative to the workspace, e.g. `./solve.py`. When it
    /// is not set, it is `main.py`, `main` or `main.jar`, by the workspace's type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    /// Working directory of the workspace, e.g. `./src/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// Reads a field that defaults to `T::default()`, and `null` as it.
fn or_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// Reads an optional text field, [folded](fold).
fn folded<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(Option::<String>::deserialize(d)?.map(|text| fold(&text)))
}

/// Reads a text field that defaults to empty, [folded](fold), and `null`
/// as empty.
fn folded_or_empty<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(folded(d)?.unwrap_or_default())
}

/// `text` with each run of whitespace that holds a newline replaced by one
/// space, then trimmed: how the manifest reads text that YAML block
/// scalars (`|` and `>`) spread over several lines.
fn fold(text: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(char::is_whitespace) {
        folded.push_str(&rest[..start]);
        let run = &rest[start..];
        let end = run.find(|c: char| !c.is_whitespace()).unwrap_or(run.len());
        let space = &run[..end];
        folded.push_str(if space.contains('\n') { " " } else { space });
        rest = &run[end..];
    }
    folded.push_str(rest);
    folded.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::fold;

    #[test]
    fn folds_newlines_into_one_space_and_trims() {
        assert_eq!(fold("echo a\necho b\n"), "echo a echo b");
        assert_eq!(fold("a  \n \n  b"), "a b");
        assert_eq!(fold("a  b\tc"), "a  b\tc");
        assert_eq!(fold("  padded  "), "padded");
        assert_eq!(fold(""), "");
    }
}
