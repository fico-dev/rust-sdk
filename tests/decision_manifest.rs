//! Tests for the workspace manifest, `workspace.yaml`: `manifest::validate` and
//! `manifest::init`, and their tools. The realistic manifests are those of
//! sample Python, Go and Java workspaces, some shortened.

mod mock_server;
mod support;

use std::path::Path;

use fico_sdk::Error;
use fico_sdk::decision::local::Local;
use fico_sdk::decision::manifest::{self, ManifestInitParams, ManifestValidateParams};
use fico_sdk::decision::*;
use fico_sdk::tool::{Context, ToolError};
use serde_json::json;
use support::TempDir;

/// A temp workspace directory whose `workspace.yaml` holds `yaml`.
fn workspace(yaml: impl AsRef<[u8]>) -> TempDir {
    let dir = TempDir::new("manifest");
    std::fs::write(dir.path().join("workspace.yaml"), yaml).unwrap();
    dir
}

async fn validate_in(dir: &Path) -> fico_sdk::Result<Manifest> {
    manifest::validate(ManifestValidateParams {
        workspace_dir: Some(dir.to_owned()),
    })
    .await
}

/// Validates `yaml`, which must be a valid manifest.
async fn read(yaml: &str) -> Manifest {
    let dir = workspace(yaml);
    validate_in(dir.path()).await.unwrap()
}

/// Validates `yaml`, which must be an invalid manifest, and returns why.
async fn reason(yaml: impl AsRef<[u8]>) -> String {
    let dir = workspace(yaml);
    match validate_in(dir.path()).await {
        Err(Error::Manifest { path, reason, .. }) => {
            assert_eq!(path, dir.path().join("workspace.yaml"));
            reason
        }
        other => panic!("expected a manifest error, got {other:?}"),
    }
}

/// A Python JSON workspace's manifest, with slider options, shortened.
const PYTHON_SLIDERS: &str = r#"
# This manifest holds the information the workspace needs to run on the FICO® platform.

# Type and runtime specify the language and environment of the workspace.
type: python
runtime: ghcr.io/nextmv-io/runtime/python:3.11

# Python-specific configurations.
python:
  # All listed packages will get bundled with the workspace.
  pip-requirements: pyproject.toml # Can be a requirements.txt

# List all files/directories that should be included in the workspace. Globbing
# (e.g.: configs/*.json) is supported.
files:
  - main.py
  - visualizations.py

# Workspace configurations.
configuration:
  # Define the content format of the workspace, one of: json, multi-file.
  content:
    format: "json" # Read JSON from stdin and write JSON to stdout.
  # Define the options (parameters) of the workspace.
  options:
    strict: false
    items:
      - name: duration
        option_type: int
        default: 30
        description: Max solver runtime in seconds
        required: false
        additional_attributes:
          min: 1
          max: 300
          step: 10
        ui:
          control_type: slider
          display_name: Duration
      - name: extended_day_bonus
        option_type: float
        default: 2.0
        description: Bonus multiplier for granting extended day (scaled by student priority)
        required: false
        additional_attributes:
          min: 0.0
          max: 10.0
          step: 0.5
        ui:
          control_type: slider
          display_name: Extended Day Bonus
"#;

/// A Go multi-file workspace's manifest.
const GO_MULTI_FILE: &str = r#"
# This manifest holds the information the workspace needs to run on the FICO® platform.

# Type and runtime specify the language and environment of the workspace.
type: binary
runtime: ghcr.io/nextmv-io/runtime/default:latest

# Directives to build a Go binary from the workspace.
build:
  command: go build -o main .
  # Use Linux ARM64 as the target for the FICO® platform.
  environment:
    GOOS: linux
    GOARCH: arm64

# List all files/directories that should be included in the workspace. Globbing
# (e.g.: configs/*.json) is supported.
files:
  - main

# Workspace configurations.
configuration:
  # Define the content format of the workspace, one of: json, multi-file.
  content:
    format: multi-file # Read and write files from disk.
    # Configuration of the multi-file format.
    multi-file:
      input:
        path: inputs # Directory where the workspace will read input files from.
      output:
        solutions: outputs/solutions # Directory where the workspace will write solution files to.
        metrics: outputs/metrics.json # Path where the workspace will write metrics to.
  # Define the options (parameters) of the workspace.
  options:
    items:
      - name: details
        option_type: bool
        default: true
        description: Print details to logs. Default true.
        required: false
        ui:
          display_name: Details
"#;

/// A Java JSON workspace's manifest.
const JAVA: &str = r#"
# This manifest holds the information the workspace needs to run on the FICO® platform.

# Type and runtime specify the language and environment of the workspace.
type: java
runtime: ghcr.io/nextmv-io/runtime/java:latest

# Directives to compile the workspace.
pre-push: mvn package

# List all files/directories that should be included in the workspace. Globbing
# (e.g.: configs/*.json) is supported.
files:
  - main.jar

# Workspace configurations.
configuration:
  # Define the content format of the workspace, one of: json, multi-file.
  content:
    format: "json" # Read JSON from stdin and write JSON to stdout.
"#;

/// A Python cuOpt workspace's manifest, with Python settings, shortened.
const CUOPT: &str = r#"
# This manifest holds the information the workspace needs to run on the FICO® platform.
type: python
runtime: ghcr.io/nextmv-io/runtime/cuopt:latest

python:
  # All listed packages will get bundled with the workspace.
  pip-requirements: pyproject.toml # Can be a requirements.txt
  arch: amd64 # The FICO® platform runs cuOpt models on amd64 instances.
  version: 3.12 # The cuOpt image uses Python 3.12.

files:
  - main.py
  - visual.py
"#;

/// A Go workspace's manifest, with option validation and roles, shortened.
const GO_VALIDATION_AND_ROLES: &str = r#"
# This manifest holds the information the workspace needs to run on the FICO® platform.
type: go
runtime: ghcr.io/nextmv-io/runtime/default:latest

build:
  command: go build -o main .
  environment:
    GOOS: linux
    GOARCH: arm64

files:
  - main

configuration:
  options:
    strict: false
    validation:
      enforce: all
    items:
      - name: check.verbosity
        option_type: string
        description: "{off, low, medium, high} verbosity of the check"
        additional_attributes:
          values:
            - "off"
            - "low"
            - "medium"
            - "high"
        ui:
          control_type: select
          display_name: Check Verbosity
      - name: format.disable.progression
        option_type: bool
        description: Disable the progression series
        ui:
          control_type: toggle
          display_name: Disable Progression
          hidden_from:
            - viewer
            - operator
      - name: runner.input.path
        option_type: string
        description: The input file path
        local_only: true

  # Specify the format the workspace reads and writes.
  content:
    format: "json" # Read JSON from stdin and write JSON to stdout.
"#;

#[tokio::test]
async fn reads_a_python_workspace_with_sliders() {
    let manifest = read(PYTHON_SLIDERS).await;
    assert_eq!(manifest.manifest_type, ManifestType::Python);
    assert_eq!(manifest.runtime, ManifestRuntime::Python);
    assert_eq!(manifest.files, ["main.py", "visualizations.py"]);
    let python = manifest.python.unwrap();
    assert_eq!(
        python.pip_requirements,
        Some(ManifestPipRequirements::File("pyproject.toml".to_owned()))
    );
    assert_eq!(manifest.configuration.content.format, ContentFormat::Json);
    assert_eq!(manifest.configuration.content.multi_file, None);
    let options = manifest.configuration.options.unwrap();
    assert!(!options.strict);
    let items = options.items.unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].name, "duration");
    assert_eq!(items[0].option_type, ManifestOptionType::Int);
    assert_eq!(items[0].default, Some(json!(30)));
    let attributes = items[0].additional_attributes.as_ref().unwrap();
    assert_eq!(attributes["max"], json!(300));
    let ui = items[1].ui.as_ref().unwrap();
    assert_eq!(ui.control_type, Some(ControlType::Slider));
    assert_eq!(ui.display_name.as_deref(), Some("Extended Day Bonus"));
    assert_eq!(items[1].default, Some(json!(2.0)));
}

#[tokio::test]
async fn reads_a_go_multi_file_workspace() {
    let manifest = read(GO_MULTI_FILE).await;
    assert_eq!(manifest.manifest_type, ManifestType::Binary);
    assert_eq!(manifest.runtime, ManifestRuntime::Default);
    let build = manifest.build.unwrap();
    assert_eq!(build.command.as_deref(), Some("go build -o main ."));
    let environment = build.environment.unwrap();
    assert_eq!(environment["GOOS"], "linux");
    assert_eq!(environment["GOARCH"], "arm64");
    let content = manifest.configuration.content;
    assert_eq!(content.format, ContentFormat::MultiFile);
    let multi_file = content.multi_file.unwrap();
    assert_eq!(multi_file.input.path, "inputs");
    assert_eq!(multi_file.output.solutions, "outputs/solutions");
    assert_eq!(multi_file.output.metrics, "outputs/metrics.json");
    // The file leaves the assets out, so they get the default.
    assert_eq!(multi_file.output.assets, "outputs/assets.json");
    let options = manifest.configuration.options.unwrap();
    let details = &options.items.unwrap()[0];
    assert_eq!(details.option_type, ManifestOptionType::Bool);
    assert_eq!(details.default, Some(json!(true)));
}

#[tokio::test]
async fn reads_a_java_workspace() {
    let manifest = read(JAVA).await;
    assert_eq!(manifest.manifest_type, ManifestType::Java);
    assert_eq!(manifest.runtime, ManifestRuntime::Java);
    assert_eq!(manifest.pre_push.as_deref(), Some("mvn package"));
    assert_eq!(manifest.files, ["main.jar"]);
    assert_eq!(manifest.configuration.options, None);
}

#[tokio::test]
async fn reads_python_settings() {
    let manifest = read(CUOPT).await;
    assert_eq!(manifest.runtime, ManifestRuntime::Cuopt);
    let python = manifest.python.unwrap();
    assert_eq!(python.arch, Some(ManifestPythonArch::Amd64));
    assert_eq!(python.version.as_deref(), Some("3.12"));
    // A version is read as text: `3.10` stays `3.10`, not the number `3.1`.
    let manifest = read("files: [main.py]\npython:\n  version: 3.10\n").await;
    assert_eq!(manifest.python.unwrap().version.as_deref(), Some("3.10"));
    let manifest =
        read("files: [main.py]\npython:\n  pip-requirements: [ortools==9.12.4544, pyyaml]\n").await;
    assert_eq!(
        manifest.python.unwrap().pip_requirements,
        Some(ManifestPipRequirements::Packages(vec![
            "ortools==9.12.4544".to_owned(),
            "pyyaml".to_owned()
        ]))
    );
}

#[tokio::test]
async fn reads_option_validation_and_roles() {
    let manifest = read(GO_VALIDATION_AND_ROLES).await;
    assert_eq!(manifest.manifest_type, ManifestType::Go);
    let options = manifest.configuration.options.unwrap();
    assert_eq!(options.validation.unwrap().enforce, "all");
    let items = options.items.unwrap();
    let values = &items[0].additional_attributes.as_ref().unwrap()["values"];
    assert_eq!(values, &json!(["off", "low", "medium", "high"]));
    let ui = items[1].ui.as_ref().unwrap();
    assert_eq!(
        ui.hidden_from,
        Some(vec![AccountMemberRole::Viewer, AccountMemberRole::Operator])
    );
    assert!(items[2].local_only && !items[2].required);
    assert_eq!(items[2].ui, None);
}

#[tokio::test]
async fn fills_in_the_defaults() {
    let manifest = read("files: [main.py]\n").await;
    let expected = Manifest {
        files: vec!["main.py".to_owned()],
        ..Default::default()
    };
    assert_eq!(manifest, expected);
    assert_eq!(manifest.manifest_type, ManifestType::Python);
    assert_eq!(manifest.runtime, ManifestRuntime::Python);
    assert_eq!(manifest.configuration.content.format, ContentFormat::Json);

    // A multi-file workspace that leaves its locations out gets the default ones.
    let manifest =
        read("files: [main]\nconfiguration:\n  content:\n    format: multi-file\n").await;
    let multi_file = manifest.configuration.content.multi_file.unwrap();
    assert_eq!(multi_file, ManifestContentMultiFile::default());
    assert_eq!(multi_file.input.path, "inputs/");
    assert_eq!(multi_file.output.solutions, "outputs/solutions/");

    // `null` reads as the default in these fields.
    let yaml = "files: [main.py]\nconfiguration:\n  options:\n    strict: null\n    \
                items:\n      - name: a\n        type: string\n        description: null\n  \
                content: null\n";
    let manifest = read(yaml).await;
    let options = manifest.configuration.options.unwrap();
    assert!(!options.strict);
    assert_eq!(options.items.unwrap()[0].description, "");
    assert_eq!(manifest.configuration.content, ManifestContent::default());
    let manifest = read("files: [main.py]\nconfiguration: ~\n").await;
    assert_eq!(manifest.configuration, ManifestConfiguration::default());
}

/// The emitted JSON uses each field's main key, e.g. `pre-push` for the
/// file's `pre_push` and `option_type` for its `type`, and fills in the
/// defaults.
#[tokio::test]
async fn emits_the_main_keys_and_the_defaults() {
    let yaml = r#"
files: [main.py]
pre_push: make
python:
  pip_requirements: requirements.txt
configuration:
  content:
    format: multi-file
    multi_file:
      input: {path: data}
  options:
    validation: {}
    items:
      - name: duration
        type: int
        ui: {control_type: input}
    format: ["--{{name}}={{value}}"]
"#;
    let manifest = read(yaml).await;
    let expected = json!({
        "type": "python",
        "runtime": "ghcr.io/nextmv-io/runtime/python:3.11",
        "python": { "pip-requirements": "requirements.txt" },
        "files": ["main.py"],
        "configuration": {
            "options": {
                "strict": false,
                "validation": { "enforce": "none" },
                "items": [{
                    "name": "duration",
                    "option_type": "int",
                    "description": "",
                    "required": false,
                    "local_only": false,
                    "ui": { "control_type": "input" },
                }],
                "format": ["--{{name}}={{value}}"],
            },
            "content": {
                "format": "multi-file",
                "multi-file": {
                    "input": { "path": "data" },
                    "output": {
                        "metrics": "outputs/metrics.json",
                        "assets": "outputs/assets.json",
                        "solutions": "outputs/solutions/",
                    },
                },
            },
        },
        "pre-push": "make",
    });
    assert_eq!(serde_json::to_value(&manifest).unwrap(), expected);
    // What it emits reads back the same.
    let back: Manifest = serde_json::from_value(expected).unwrap();
    assert_eq!(back, manifest);
}

#[tokio::test]
async fn folds_text_over_several_lines() {
    let yaml = r#"
files: [main.py]
pre-push: >
  make
  build
build:
  command: |
    go build
    -o main .
configuration:
  options:
    items:
      - name: a
        option_type: string
        description: |
          A long
          description.
        ui:
          display_name: "  A  "
"#;
    let manifest = read(yaml).await;
    assert_eq!(manifest.pre_push.as_deref(), Some("make build"));
    let command = manifest.build.unwrap().command;
    assert_eq!(command.as_deref(), Some("go build -o main ."));
    let option = &manifest.configuration.options.unwrap().items.unwrap()[0];
    assert_eq!(option.description, "A long description.");
    let display_name = option.ui.as_ref().unwrap().display_name.as_deref();
    assert_eq!(display_name, Some("A"));
}

#[tokio::test]
async fn keeps_build_environment_values_as_written() {
    let yaml =
        "files: [main]\nbuild:\n  environment: {PY: 3.10, O: 017, E: 1e3, DEBUG: yes, N: 3}\n";
    let environment = read(yaml).await.build.unwrap().environment.unwrap();
    let values: Vec<(&str, &str)> = environment
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let expected = [
        ("DEBUG", "yes"),
        ("E", "1e3"),
        ("N", "3"),
        ("O", "017"),
        ("PY", "3.10"),
    ];
    assert_eq!(values, expected);
    let reason = reason("files: [main]\nbuild:\n  environment: {PATHS: [a, b]}\n").await;
    assert!(reason.contains("line 3"), "{reason}");
}

#[tokio::test]
async fn keeps_the_last_duplicate_key() {
    let manifest = read("files: [a]\ntype: java\nfiles: [b]\n").await;
    assert_eq!(manifest.files, ["b"]);
    assert_eq!(manifest.manifest_type, ManifestType::Java);
}

#[tokio::test]
async fn ignores_unknown_keys() {
    let manifest = read("files: [main.py]\nmarketplace: {listed: true}\n").await;
    assert_eq!(manifest.files, ["main.py"]);
}

/// Each manifest, with the start of the reason it is invalid.
const INVALID: &[(&str, &str)] = &[
    ("", "unexpected end of input"),
    (
        "files: [main.py\ntype: python\n",
        "illegal placement of ':' indicator at line 2",
    ),
    ("type: python\n", "missing field `files`"),
    ("files: main.py\n", "expected sequence start at line 1"),
    ("files: []\n", "`files` must list at least one file"),
    ("files: [a]\ntype: rust\n", "unknown variant `rust`"),
    ("files: [a]\nruntime: ubuntu\n", "unknown variant `ubuntu`"),
    // The `csv-archive` and `text` formats are not supported.
    (
        "files: [a]\nconfiguration:\n  content:\n    format: csv-archive\n",
        "unknown variant `csv-archive`",
    ),
    (
        "files: [a]\nconfiguration:\n  content:\n    format: text\n",
        "unknown variant `text`",
    ),
    // A `null` format is rejected, not read as the default.
    (
        "files: [a]\nconfiguration:\n  content:\n    format: null\n",
        "",
    ),
    (
        "files: [a]\nconfiguration:\n  content:\n    multi-file: {}\n",
        "`multi-file` is set, but the content format is `json`, not `multi-file`: remove \
         `multi-file` or change the format",
    ),
    (
        "files: [a]\nconfiguration:\n  options:\n    validation: {enforce: some}\n",
        "`enforce` is `some`, but it must be `none` or `all`",
    ),
    (
        "files: [a]\nconfiguration:\n  options:\n    items:\n      - name: x\n        \
         option_type: int\n        required: true\n        local_only: true\n",
        "option `x` cannot be both required and local only",
    ),
    (
        "files: [a]\nconfiguration:\n  options:\n    items:\n      - name: x\n        \
         option_type: bool\n        ui: {control_type: slider}\n",
        "option `x` has option type `bool` with control type `slider`, which does not fit: \
         use control type `toggle`",
    ),
    (
        "files: [a]\nconfiguration:\n  options:\n    items:\n      - name: x\n        \
         option_type: string\n        ui: {control_type: toggle}\n",
        "option `x` has option type `string` with control type `toggle`, which does not \
         fit: use control type `input`, `multiselect`, `select`",
    ),
    (
        "files: [a]\nconfiguration:\n  options:\n    items:\n      - name: x\n        \
         option_type: int\n        ui: {control_type: slider}\n",
        "option `x` does not set the additional attributes `max`, `min`, `step`, which \
         option type `int` with control type `slider` requires",
    ),
    (
        "files: [a]\nconfiguration:\n  options:\n    items:\n      - name: x\n        \
         option_type: float\n        additional_attributes: {min: 0}\n        \
         ui: {control_type: slider}\n",
        "option `x` does not set the additional attributes `max`, `step`, which option \
         type `float` with control type `slider` requires",
    ),
    (
        "files: [a]\nconfiguration:\n  options:\n    items:\n      - name: x\n        \
         option_type: string\n        additional_attributes: {values: [a], min: 0}\n        \
         ui: {control_type: multiselect}\n",
        "option `x` sets the additional attributes `min`, which option type `string` with \
         control type `multiselect` does not take: it takes `values`",
    ),
    (
        "files: [a]\nconfiguration:\n  options:\n    items:\n      - name: x\n        \
         option_type: bool\n        additional_attributes: {values: [true]}\n        \
         ui: {control_type: toggle}\n",
        "option `x` sets the additional attributes `values`, which option type `bool` with \
         control type `toggle` does not take: it takes none",
    ),
    (
        "files: [a]\nconfiguration:\n  options:\n    items:\n      - name: x\n        \
         option_type: string\n        ui: {hidden_from: [owner]}\n",
        "unknown variant `owner`",
    ),
    (
        "files: [a]\nconfiguration:\n  options:\n    items:\n      - name: x\n",
        "missing field `option_type`",
    ),
    (
        "files: [a]\npre-push: a\npre_push: b\n",
        "duplicate field `pre-push`",
    ),
];

#[tokio::test]
async fn rejects_invalid_manifests() {
    for (yaml, expected) in INVALID {
        let reason = reason(yaml).await;
        assert!(reason.starts_with(expected), "{yaml:?}: {reason}");
        assert!(!reason.contains('\n'), "{yaml:?}: {reason}");
        assert!(!reason.contains("(defined at"), "{yaml:?}: {reason}");
    }
    let reason = reason(b"files: [\xff]\n").await;
    assert!(reason.starts_with("the file is not UTF-8"), "{reason}");
}

#[tokio::test]
async fn reports_nested_errors_at_their_location() {
    let yaml = "files: [a]\nconfiguration:\n  options:\n    items:\n      - name: x\n";
    assert_eq!(
        reason(yaml).await,
        "missing field `option_type` at line 5, column 9"
    );
    // An error in an alias is reported where its anchor is.
    let yaml = "base: &base\n  name: x\nfiles: [a]\nconfiguration:\n  options:\n    \
                items:\n      - *base\n";
    assert_eq!(
        reason(yaml).await,
        "missing field `option_type` at line 2, column 3"
    );
}

#[tokio::test]
async fn accepts_options_without_a_control() {
    // Without a control, no additional attribute is checked.
    let yaml = "files: [a]\nconfiguration:\n  options:\n    items:\n      - name: x\n        \
                option_type: int\n        additional_attributes: {anything: 1}\n";
    let manifest = read(yaml).await;
    assert!(manifest.configuration.options.is_some());
}

#[tokio::test]
async fn reports_a_missing_manifest_as_an_io_error() {
    let dir = TempDir::new("manifest");
    let err = validate_in(dir.path()).await.unwrap_err();
    match err {
        Error::Io { path, source, .. } => {
            assert_eq!(path, dir.path().join("workspace.yaml"));
            assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
        }
        other => panic!("expected an I/O error, got {other:?}"),
    }
}

#[tokio::test]
async fn shows_the_file_and_the_reason() {
    let dir = workspace("files: []\n");
    let err = validate_in(dir.path()).await.unwrap_err();
    let path = dir.path().join("workspace.yaml");
    assert_eq!(
        err.to_string(),
        format!(
            "invalid manifest `{}`: `files` must list at least one file",
            path.display()
        )
    );
}

fn local_context(home: &TempDir) -> Context {
    Context::new().with_local(Local::with_home(home.path().join(".fico")))
}

#[tokio::test]
async fn validates_through_the_tool() {
    let dir = workspace(JAVA);
    let args = json!({ "workspace_dir": dir.path() });
    let result = support::call_tool(local_context(&dir), "decision_manifest_validate", args);
    let manifest = result.await.unwrap();
    assert_eq!(manifest["type"], "java");
    assert_eq!(manifest["pre-push"], "mvn package");
    assert_eq!(
        manifest["configuration"]["content"],
        json!({ "format": "json" })
    );

    let dir = workspace("files: []\n");
    let args = json!({ "workspace_dir": dir.path() });
    let err = support::call_tool(local_context(&dir), "decision_manifest_validate", args)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("`files` must list"), "{err}");
}

const TYPES: [ManifestType; 4] = [
    ManifestType::Python,
    ManifestType::Go,
    ManifestType::Java,
    ManifestType::Binary,
];

const FORMATS: [ContentFormat; 2] = [ContentFormat::Json, ContentFormat::MultiFile];

#[test]
fn defaults_to_a_python_json_sample_with_options() {
    let params = ManifestInitParams::default();
    assert_eq!(params.workspace_dir, None);
    assert_eq!(params.manifest_type, ManifestType::Python);
    assert_eq!(params.content_format, ContentFormat::Json);
    assert!(params.with_options);
}

#[tokio::test]
async fn writes_a_valid_sample_for_every_type_and_format() {
    for manifest_type in TYPES {
        for content_format in FORMATS {
            for with_options in [false, true] {
                let dir = TempDir::new("manifest");
                let params = ManifestInitParams {
                    workspace_dir: Some(dir.path().to_owned()),
                    manifest_type,
                    content_format,
                    with_options,
                    overwrite: false,
                };
                let case = format!("{params:?}");
                let path = manifest::init(params).await.unwrap();
                assert_eq!(path, dir.path().join("workspace.yaml"), "{case}");
                let text = std::fs::read_to_string(&path).unwrap();
                assert!(!text.contains("Nextmv"), "{case}");
                let manifest = validate_in(dir.path()).await.unwrap();
                // The Go sample builds a binary, so its type is `binary`.
                let expected_type = match manifest_type {
                    ManifestType::Go => ManifestType::Binary,
                    other => other,
                };
                assert_eq!(manifest.manifest_type, expected_type, "{case}");
                let content = &manifest.configuration.content;
                assert_eq!(content.format, content_format, "{case}");
                let options = manifest.configuration.options.as_ref();
                let items = options.and_then(|options| options.items.as_ref());
                assert_eq!(items.map(Vec::len), with_options.then_some(4), "{case}");
            }
        }
    }
}

#[tokio::test]
async fn writes_the_python_json_sample_text() {
    let dir = TempDir::new("manifest");
    let params = ManifestInitParams {
        workspace_dir: Some(dir.path().to_owned()),
        with_options: false,
        ..Default::default()
    };
    let path = manifest::init(params).await.unwrap();
    let expected = "\
# This manifest holds the information the workspace needs to run on the FICO® platform.

# Type and runtime specify the language and environment of the workspace.
type: python
runtime: ghcr.io/nextmv-io/runtime/python:3.11

# Python-specific configurations.
python:
  # All listed packages will get bundled with the workspace.
  pip-requirements: pyproject.toml # Can be a requirements.txt

# List all files/directories that should be included in the workspace. Globbing
# (e.g.: configs/*.json) is supported.
files:
  - main.py

# Workspace configurations.
configuration:
  # Define the content format of the workspace, one of: json, multi-file.
  content:
    format: json # Read JSON from stdin and write JSON to stdout.
";
    assert_eq!(std::fs::read_to_string(path).unwrap(), expected);
}

#[tokio::test]
async fn creates_the_directory_and_replaces_the_manifest_with_overwrite() {
    let dir = TempDir::new("manifest");
    let workspace_dir = dir.path().join("workspaces").join("routing");
    let params = ManifestInitParams {
        workspace_dir: Some(workspace_dir.clone()),
        manifest_type: ManifestType::Java,
        overwrite: true,
        ..Default::default()
    };
    let path = manifest::init(params.clone()).await.unwrap();
    assert_eq!(path, workspace_dir.join("workspace.yaml"));
    std::fs::write(&path, "not: a manifest\n").unwrap();
    manifest::init(params).await.unwrap();
    let manifest = validate_in(&workspace_dir).await.unwrap();
    assert_eq!(manifest.manifest_type, ManifestType::Java);
}

#[tokio::test]
async fn keeps_an_existing_manifest_without_overwrite() {
    let dir = workspace("files: [main.py]\n");
    let params = ManifestInitParams {
        workspace_dir: Some(dir.path().to_owned()),
        manifest_type: ManifestType::Java,
        ..Default::default()
    };
    let err = manifest::init(params).await.unwrap_err();
    let file = dir.path().join("workspace.yaml");
    assert!(
        matches!(&err, Error::Io { path, source, .. }
            if *path == file && source.kind() == std::io::ErrorKind::AlreadyExists),
        "{err:?}"
    );
    assert_eq!(std::fs::read_to_string(file).unwrap(), "files: [main.py]\n");
}

#[tokio::test]
async fn rejects_the_unknown_content_format() {
    let dir = TempDir::new("manifest");
    let params = ManifestInitParams {
        workspace_dir: Some(dir.path().to_owned()),
        content_format: ContentFormat::Unknown,
        ..Default::default()
    };
    let err = manifest::init(params).await.unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert!(!dir.path().join("workspace.yaml").exists());
}

#[tokio::test]
async fn reports_a_directory_it_cannot_create() {
    let dir = workspace("files: [main.py]\n");
    let file = dir.path().join("workspace.yaml");
    let params = ManifestInitParams {
        workspace_dir: Some(file.clone()),
        ..Default::default()
    };
    let err = manifest::init(params).await.unwrap_err();
    assert!(
        matches!(&err, Error::Io { path, .. } if *path == file),
        "{err}"
    );
}

#[tokio::test]
async fn writes_a_sample_through_the_tool() {
    let dir = TempDir::new("manifest");
    let args = json!({
        "workspace_dir": dir.path(),
        "manifest_type": "go",
        "content_format": "multi-file",
        "with_options": false,
    });
    let result = support::call_tool(local_context(&dir), "decision_manifest_init", args);
    let path = dir.path().join("workspace.yaml");
    assert_eq!(result.await.unwrap(), json!(path));
    let manifest = validate_in(dir.path()).await.unwrap();
    assert_eq!(
        manifest.configuration.content.format,
        ContentFormat::MultiFile
    );
    assert_eq!(manifest.configuration.options, None);

    let args = json!({ "workspace_dir": dir.path(), "content_format": "csv-archive" });
    let err = support::call_tool(local_context(&dir), "decision_manifest_init", args)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArguments(_)), "{err}");
}

#[tokio::test]
async fn writes_a_manifest_that_reads_back() {
    let dir = workspace("files: [old.py]\n");
    let mut manifest = Manifest {
        manifest_type: ManifestType::Go,
        files: vec!["main".to_owned(), "configs/*.json".to_owned()],
        ..Default::default()
    };
    manifest.configuration.content.format = ContentFormat::MultiFile;
    let path = manifest::write(dir.path(), &manifest).await.unwrap();
    assert_eq!(path, dir.path().join("workspace.yaml"));
    // Reading fills in the default multi-file locations.
    manifest.configuration.content.multi_file = Some(Default::default());
    assert_eq!(validate_in(dir.path()).await.unwrap(), manifest);
    // The file goes in through a rename: no temporary file is left.
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["workspace.yaml"]);
}

#[tokio::test]
async fn writes_no_invalid_manifest() {
    let dir = workspace("files: [main.py]\n");
    for manifest in [
        Manifest::default(),
        Manifest {
            files: vec!["main.py".to_owned()],
            configuration: ManifestConfiguration {
                content: ManifestContent {
                    format: ContentFormat::Unknown,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
        // Serialized as `unknown`, which reading rejects.
        Manifest {
            files: vec!["main.py".to_owned()],
            configuration: ManifestConfiguration {
                options: Some(ManifestOptions {
                    items: Some(vec![ManifestOption {
                        name: "duration".to_owned(),
                        ui: Some(ManifestOptionUi {
                            hidden_from: Some(vec![AccountMemberRole::Unknown]),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        },
    ] {
        let err = manifest::write(dir.path(), &manifest).await.unwrap_err();
        assert!(matches!(err, Error::Manifest { .. }), "{err}");
    }
    let yaml = std::fs::read_to_string(dir.path().join("workspace.yaml")).unwrap();
    assert_eq!(yaml, "files: [main.py]\n");
}
