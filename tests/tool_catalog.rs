//! Checks the `decision_*` tool catalog: the tool names, their flags, their
//! schemas and descriptions, and that no tool reaches the platform or the
//! local machine without the context for it.

mod mock_server;
mod support;

use std::collections::BTreeSet;

use fico_sdk::Error;
use fico_sdk::tool::{Context, Tool, ToolError};
use mock_server::MockServer;
use serde_json::{Map, Value, json};

/// The names of the 119 tools the catalog has, by area.
const TOOLS: [&str; 119] = [
    // Top level.
    "decision_manifest_init",
    "decision_manifest_validate",
    // Platform.
    "decision_platform_workspace_create",
    "decision_platform_workspace_get",
    "decision_platform_workspace_exists",
    "decision_platform_workspace_list",
    "decision_platform_workspace_update",
    "decision_platform_workspace_delete",
    "decision_platform_workspace_push",
    "decision_platform_build_create",
    "decision_platform_build_get",
    "decision_platform_build_exists",
    "decision_platform_build_list",
    "decision_platform_build_update",
    "decision_platform_build_delete",
    "decision_platform_environment_create",
    "decision_platform_environment_get",
    "decision_platform_environment_exists",
    "decision_platform_environment_list",
    "decision_platform_environment_update",
    "decision_platform_environment_delete",
    "decision_platform_run_get",
    "decision_platform_run_information_get",
    "decision_platform_run_input_get",
    "decision_platform_run_logs_get",
    "decision_platform_run_list",
    "decision_platform_run_update",
    "decision_platform_run_cancel",
    "decision_platform_run_delete",
    "decision_platform_run_create",
    "decision_platform_run_clone",
    "decision_platform_run_compare",
    "decision_platform_run_track",
    "decision_platform_run_asset_list",
    "decision_platform_run_asset_get",
    "decision_platform_run_asset_delete",
    "decision_platform_integration_create",
    "decision_platform_integration_get",
    "decision_platform_integration_exists",
    "decision_platform_integration_list",
    "decision_platform_integration_update",
    "decision_platform_integration_delete",
    "decision_platform_integration_health_get",
    "decision_platform_upload_create",
    "decision_platform_data_upload",
    "decision_platform_acceptance_create",
    "decision_platform_acceptance_get",
    "decision_platform_acceptance_list",
    "decision_platform_acceptance_update",
    "decision_platform_acceptance_delete",
    "decision_platform_scenario_create",
    "decision_platform_scenario_get",
    "decision_platform_scenario_list",
    "decision_platform_scenario_metadata_get",
    "decision_platform_scenario_update",
    "decision_platform_scenario_delete",
    "decision_platform_shadow_create",
    "decision_platform_shadow_get",
    "decision_platform_shadow_list",
    "decision_platform_shadow_delete",
    "decision_platform_shadow_metadata_get",
    "decision_platform_shadow_start",
    "decision_platform_shadow_stop",
    "decision_platform_shadow_update",
    "decision_platform_switchback_create",
    "decision_platform_switchback_get",
    "decision_platform_switchback_list",
    "decision_platform_switchback_delete",
    "decision_platform_switchback_metadata_get",
    "decision_platform_switchback_start",
    "decision_platform_switchback_stop",
    "decision_platform_switchback_update",
    "decision_platform_ensemble_create",
    "decision_platform_ensemble_get",
    "decision_platform_ensemble_list",
    "decision_platform_ensemble_update",
    "decision_platform_ensemble_delete",
    "decision_platform_input_set_create",
    "decision_platform_input_set_get",
    "decision_platform_input_set_list",
    "decision_platform_input_set_update",
    "decision_platform_input_set_delete",
    "decision_platform_managed_input_create",
    "decision_platform_managed_input_get",
    "decision_platform_managed_input_list",
    "decision_platform_managed_input_update",
    "decision_platform_managed_input_delete",
    "decision_platform_secrets_create",
    "decision_platform_secrets_get",
    "decision_platform_secrets_list",
    "decision_platform_secrets_update",
    "decision_platform_secrets_delete",
    "decision_platform_account_create",
    "decision_platform_account_get",
    "decision_platform_account_update",
    "decision_platform_account_delete",
    "decision_platform_sso_create",
    "decision_platform_sso_get",
    "decision_platform_sso_enable",
    "decision_platform_sso_disable",
    "decision_platform_sso_update",
    "decision_platform_sso_delete",
    "decision_platform_sso_domain_delete",
    // Local.
    "decision_local_run_create",
    "decision_local_run_get",
    "decision_local_run_information_get",
    "decision_local_run_input_get",
    "decision_local_run_logs_get",
    "decision_local_run_list",
    "decision_local_run_clone",
    "decision_local_run_compare",
    "decision_local_run_visuals_get",
    "decision_local_workspace_delete",
    "decision_local_workspace_get",
    "decision_local_workspace_list",
    "decision_local_workspace_register",
    "decision_local_workspace_registered",
    "decision_local_workspace_update",
    "decision_local_workspace_sync",
];

/// Local-only tools: every `decision_local_*` tool, the tools that write
/// files, push (it runs the manifest's commands) and data upload (it reads
/// files and sends them to a URL the caller picks).
fn is_local_only(name: &str) -> bool {
    name.starts_with("decision_local_")
        || matches!(
            name,
            "decision_manifest_init"
                | "decision_manifest_validate"
                | "decision_platform_workspace_push"
                | "decision_platform_data_upload"
        )
}

/// Read-only tools: get, list, exists, information, input, logs, metadata,
/// registered and visuals, plus compare and `manifest_validate`, which only
/// read. The exceptions write files where the caller says, and MCP clients
/// may auto-approve read-only tools: the run and run input gets download
/// into `output_dir`, and the run asset get into `output_file`. Local reads that finalize a run or render its visuals
/// stay read-only: they only write the run's own derived files, and doing it
/// again changes nothing.
fn is_read_only(name: &str) -> bool {
    let reads = ["_get", "_list", "_exists", "_registered", "_compare"]
        .iter()
        .any(|verb| name.ends_with(verb));
    let writes = matches!(
        name,
        "decision_platform_run_get"
            | "decision_platform_run_input_get"
            | "decision_platform_run_asset_get"
            | "decision_local_run_get"
            | "decision_local_run_input_get"
    );
    (reads && !writes) || name == "decision_manifest_validate"
}

/// Destructive tools, in MCP's sense of `destructiveHint` (every other tool
/// only adds data): delete, cancel, stop, disable and `sso_domain_delete`,
/// plus the tools that overwrite data: every update, push (it replaces the
/// binary), `manifest_init` (it overwrites `workspace.yaml`), `sso_enable`
/// (it changes how every member of the account signs in) and the run, run
/// input and run asset gets (they overwrite files in `output_dir` or
/// `output_file`).
fn is_destructive(name: &str) -> bool {
    ["_delete", "_cancel", "_stop", "_disable", "_update"]
        .iter()
        .any(|verb| name.ends_with(verb))
        || matches!(
            name,
            "decision_platform_workspace_push"
                | "decision_manifest_init"
                | "decision_platform_sso_enable"
                | "decision_platform_run_get"
                | "decision_platform_run_input_get"
                | "decision_platform_run_asset_get"
                | "decision_local_run_get"
                | "decision_local_run_input_get"
        )
}

/// Tools that only read in remote mode, which has no `Local`: the read-only
/// tools, plus the platform run and run input gets, which only write into
/// `output_dir`, and so only in local mode.
fn is_read_only_in_remote_mode(name: &str) -> bool {
    is_read_only(name)
        || matches!(
            name,
            "decision_platform_run_get"
                | "decision_platform_run_input_get"
                | "decision_platform_run_asset_get"
        )
}

fn tools() -> Vec<Tool> {
    fico_sdk::decision::tools()
}

fn names_where(keep: impl Fn(&Tool) -> bool) -> BTreeSet<&'static str> {
    tools()
        .into_iter()
        .filter(|tool| keep(tool))
        .map(|tool| tool.name())
        .collect()
}

fn listed_where(keep: impl Fn(&str) -> bool) -> BTreeSet<&'static str> {
    TOOLS.into_iter().filter(|name| keep(name)).collect()
}

/// The smallest value a schema accepts: objects with only their required
/// properties, the first enum value or variant, and `"x"` for strings.
fn minimal(schema: &Value) -> Value {
    if let Some(value) = schema.get("const") {
        return value.clone();
    }
    if let Some(first) = schema["enum"].as_array().and_then(|values| values.first()) {
        return first.clone();
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(first) = schema[key].as_array().and_then(|variants| variants.first()) {
            return minimal(first);
        }
    }
    let kind = match &schema["type"] {
        Value::String(kind) => kind.as_str(),
        Value::Array(kinds) => kinds
            .iter()
            .filter_map(Value::as_str)
            .find(|kind| *kind != "null")
            .unwrap_or("null"),
        _ => "object",
    };
    match kind {
        "string" => json!("x"),
        "integer" | "number" => json!(1),
        "boolean" => json!(false),
        "array" => json!([]),
        "null" => Value::Null,
        _ => {
            let mut object = Map::new();
            let required = schema["required"].as_array().into_iter().flatten();
            for name in required.filter_map(Value::as_str) {
                object.insert(name.to_owned(), minimal(&schema["properties"][name]));
            }
            Value::Object(object)
        }
    }
}

fn minimal_args(tool: &Tool) -> Value {
    minimal(&Value::Object(tool.input_schema().clone()))
}

fn is_invalid(result: &Result<Value, ToolError>) -> bool {
    matches!(result, Err(ToolError::Failed(Error::Invalid { .. })))
}

#[test]
fn has_exactly_the_listed_tools() {
    let names: Vec<&str> = tools().iter().map(Tool::name).collect();
    let unique: BTreeSet<&str> = names.iter().copied().collect();
    assert_eq!(unique.len(), names.len(), "duplicate tool names");
    let listed: BTreeSet<&str> = TOOLS.into_iter().collect();
    assert_eq!(listed.len(), TOOLS.len(), "duplicate listed names");
    assert_eq!(unique, listed);
}

#[test]
fn sets_the_listed_flags() {
    assert_eq!(
        names_where(Tool::is_local_only),
        listed_where(is_local_only)
    );
    assert_eq!(names_where(Tool::is_read_only), listed_where(is_read_only));
    assert_eq!(
        names_where(Tool::is_read_only_in_remote_mode),
        listed_where(is_read_only_in_remote_mode)
    );
    assert_eq!(
        names_where(Tool::is_destructive),
        listed_where(is_destructive)
    );
}

#[test]
fn describes_every_tool_in_one_sentence() {
    for tool in tools() {
        let description = tool.description();
        let name = tool.name();
        assert!(
            description.starts_with(char::is_uppercase) && description.ends_with('.'),
            "{name}: {description}"
        );
        assert!(!description.contains('\n'), "{name}: {description}");
        assert!(!description.contains("[`"), "{name}: {description}");
    }
}

#[test]
fn has_closed_object_schemas_with_documented_properties() {
    for tool in tools() {
        let name = tool.name();
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object", "{name}");
        assert_eq!(schema["additionalProperties"], false, "{name}");
        // A root `oneOf` (from a flattened enum) or `$ref` would hide fields
        // from `Tool::call`'s unknown-field check and from
        // `additionalProperties`.
        for key in ["$schema", "title", "oneOf", "anyOf", "allOf", "$ref"] {
            assert!(!schema.contains_key(key), "{name}: {key}");
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        for (property, value) in properties.into_iter().flatten() {
            let description = value["description"].as_str().unwrap_or_default();
            assert!(
                !description.is_empty(),
                "{name}.{property} has no description"
            );
        }
        // Nested objects are closed too, and none has a composition keyword
        // next to its `properties` (from a flattened enum), which would hide
        // fields from the unknown-field check.
        let mut objects = Vec::new();
        object_schemas(schema, &mut objects);
        for object in objects {
            assert!(object.contains_key("additionalProperties"), "{name}");
            for key in ["oneOf", "anyOf", "allOf", "$ref"] {
                assert!(!object.contains_key(key), "{name}: {key}");
            }
        }
        // Inlined types bring their own rustdoc, e.g. in array `items` and
        // in `anyOf` branches, so every level is checked.
        let mut texts = Vec::new();
        descriptions(&Value::Object(schema.clone()), &mut texts);
        for text in texts {
            assert!(!text.contains("[`"), "{name}: intra-doc link in {text:?}");
        }
    }
}

/// Every object schema in `schema` that lists `properties`, at any depth.
fn object_schemas<'a>(schema: &'a Map<String, Value>, objects: &mut Vec<&'a Map<String, Value>>) {
    if schema.contains_key("properties") {
        objects.push(schema);
    }
    for (keyword, value) in schema {
        let subschemas: Vec<&Value> = match (keyword.as_str(), value) {
            ("properties", Value::Object(properties)) => properties.values().collect(),
            ("items" | "additionalProperties", value) => vec![value],
            ("anyOf" | "oneOf" | "allOf", Value::Array(branches)) => branches.iter().collect(),
            _ => Vec::new(),
        };
        for subschema in subschemas.into_iter().filter_map(Value::as_object) {
            object_schemas(subschema, objects);
        }
    }
}

/// Every `description` in `schema`, at any depth.
fn descriptions(schema: &Value, texts: &mut Vec<String>) {
    match schema {
        Value::Object(object) => {
            for (key, value) in object {
                match (key.as_str(), value) {
                    ("description", Value::String(text)) => texts.push(text.clone()),
                    _ => descriptions(value, texts),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| descriptions(item, texts)),
        _ => {}
    }
}

#[tokio::test]
async fn needs_a_context_for_every_tool() {
    for tool in tools() {
        let result = tool.call(Context::new(), minimal_args(&tool)).await;
        assert!(is_invalid(&result), "{}: {result:?}", tool.name());
    }
}

#[tokio::test]
async fn keeps_local_only_tools_out_of_remote_mode() {
    let server = MockServer::start(Vec::new()).await;
    let remote = Context::new().with_platform(support::platform(&server));
    for tool in tools().into_iter().filter(Tool::is_local_only) {
        let result = tool.call(remote.clone(), minimal_args(&tool)).await;
        assert!(is_invalid(&result), "{}: {result:?}", tool.name());
    }
    assert!(server.requests().is_empty());
}

/// Suffixes of the fields that hold a filesystem path, such as `output_dir`,
/// `input_dir_path` or `metadata_file`.
const PATH_SUFFIXES: [&str; 3] = ["_dir", "_path", "_file"];

/// One set of arguments for each path field of `schema`, at any depth: the
/// field's path, and the smallest value `schema` accepts with that field set
/// to `path`.
fn path_field_args(schema: &Value, path: &Value) -> Vec<(String, Value)> {
    let mut found = Vec::new();
    if let Some(items) = schema.get("items") {
        for (field, item) in path_field_args(items, path) {
            found.push((format!("[0].{field}"), json!([item])));
        }
    }
    for key in ["anyOf", "oneOf"] {
        for branch in schema[key].as_array().into_iter().flatten() {
            found.extend(path_field_args(branch, path));
        }
    }
    let properties = schema.get("properties").and_then(Value::as_object);
    for (name, property) in properties.into_iter().flatten() {
        let mut fields: Vec<(String, Value)> = path_field_args(property, path)
            .into_iter()
            .map(|(field, value)| match field.starts_with('[') {
                true => (format!("{name}{field}"), value),
                false => (format!("{name}.{field}"), value),
            })
            .collect();
        if PATH_SUFFIXES.iter().any(|suffix| name.ends_with(suffix)) {
            fields.push((name.clone(), path.clone()));
        }
        for (field, value) in fields {
            let mut args = minimal(schema);
            args[name.as_str()] = value;
            found.push((field, args));
        }
    }
    found
}

/// Remote mode must never read or write the server's files, so every tool it
/// serves fails before any request when a path field is set. The fields are
/// found by name, so new ones are checked too.
#[tokio::test]
async fn needs_local_mode_for_path_fields() {
    let server = MockServer::start(Vec::new()).await;
    let remote = Context::new().with_platform(support::platform(&server));
    let mut checked = BTreeSet::new();
    for tool in tools().into_iter().filter(|tool| !tool.is_local_only()) {
        let schema = Value::Object(tool.input_schema().clone());
        for (field, args) in path_field_args(&schema, &json!("/srv/data")) {
            let result = tool.call(remote.clone(), args).await;
            let field = format!("{}.{field}", tool.name());
            assert!(is_invalid(&result), "{field}: {result:?}");
            checked.insert(field);
        }
    }
    for field in [
        "decision_platform_run_get.output_dir",
        "decision_platform_run_input_get.output_dir",
        "decision_platform_run_create.input_dir",
        "decision_platform_run_create.input_file",
        "decision_platform_run_asset_get.output_file",
    ] {
        assert!(checked.contains(field), "{field} was not found");
    }
    assert!(server.requests().is_empty());
}
