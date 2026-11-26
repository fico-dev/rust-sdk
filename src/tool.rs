//! SDK functions as MCP tools.
//!
//! Every operation that MCP serves is a [`Tool`]: a name, a description, an
//! input schema derived from the operation's argument type, and the SDK call
//! itself. The tool rows live next to the methods they call, so a changed
//! signature breaks its row at compile time, and new fields reach the schema
//! on their own. [`decision::tools`](crate::decision::tools) returns the
//! `decision_*` tools.
//!
//! # Examples
//!
//! ```no_run
//! # async fn run() -> fico_sdk::Result<()> {
//! use fico_sdk::Client;
//! use fico_sdk::decision::platform::Platform;
//! use fico_sdk::tool::Context;
//! use serde_json::json;
//!
//! let platform = Platform::new(Client::builder("https://api.example.com").build()?);
//! let cx = Context::new().with_platform(platform);
//! let tool = fico_sdk::decision::tools()
//!     .into_iter()
//!     .find(|tool| tool.name() == "decision_platform_workspace_get")
//!     .unwrap();
//! let workspace = tool.call(cx, json!({ "workspace_id": "routing" })).await;
//! # Ok(())
//! # }
//! ```

use std::any::Any;
use std::cell::Cell;
use std::error::Error as StdError;
use std::fmt;
use std::future::{Future, ready};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};

use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::task::{JoinError, JoinHandle};

use crate::decision::local::Local;
use crate::decision::platform::Platform;
use crate::{Error, Result};

/// A boxed future that can move between threads, as returned by
/// [`Tool::call`].
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a tool runs: the arguments go in as JSON and the result comes out as
/// JSON.
type Handler = dyn Fn(Context, Value) -> BoxFuture<'static, Result<Value, ToolError>> + Send + Sync;

/// An SDK function exposed as an MCP tool.
///
/// Cloning a `Tool` is cheap.
#[derive(Clone)]
pub struct Tool {
    name: &'static str,
    title: Option<&'static str>,
    description: &'static str,
    input_schema: Arc<Map<String, Value>>,
    local_only: bool,
    read_only: bool,
    read_only_in_remote_mode: bool,
    destructive: bool,
    handler: Arc<Handler>,
}

impl Tool {
    /// Creates a tool that deserializes its arguments into `A` and calls `f`.
    ///
    /// The input schema is derived from `A`, so field doc comments become
    /// the parameter descriptions. `description` should be the first line of
    /// the rustdoc of the method `f` calls, unless the tool changes what the
    /// method returns, and then it should say how.
    ///
    /// `A` must be a struct, and the types in it may only
    /// `#[serde(flatten)]` structs and maps. A flattened enum puts a `oneOf`
    /// next to the object's `properties`: [`call`](Self::call) would reject
    /// the enum's fields as unknown, and with `additionalProperties: false`
    /// the schema would accept no input.
    pub fn new<A, T, F, Fut>(name: &'static str, description: &'static str, f: F) -> Tool
    where
        A: DeserializeOwned + JsonSchema + 'static,
        T: Serialize + 'static,
        F: Fn(Context, A) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        let input_schema = input_schema::<A>();
        let f = Arc::new(f);
        let handler = move |cx: Context, args: Value| -> BoxFuture<'static, _> {
            let f = Arc::clone(&f);
            Box::pin(async move {
                // The whole call runs in its own task, so that a panic in a
                // tool becomes an error instead of ending the MCP session.
                let task = AbortOnDrop(tokio::spawn(async move {
                    let args: A = read_arguments(args)
                        .map_err(|e| ToolError::InvalidArguments(e.to_string()))?;
                    let output = f(cx, args).await.map_err(ToolError::Failed)?;
                    serde_json::to_value(output).map_err(|e| {
                        ToolError::Failed(Error::Encode {
                            reason: "tool result is not serializable".to_owned(),
                            source: Some(Box::new(e)),
                        })
                    })
                }));
                match task.await {
                    Ok(result) => result,
                    Err(error) => Err(ToolError::Panicked(panic_message(error))),
                }
            })
        };
        Tool {
            name,
            title: None,
            description,
            input_schema: Arc::new(input_schema),
            local_only: false,
            read_only: false,
            read_only_in_remote_mode: false,
            destructive: false,
            handler: Arc::new(handler),
        }
    }

    /// Marks the tool as local-only: it reads or writes files, or runs
    /// commands, so remote (HTTP) mode does not serve it.
    pub fn local_only(mut self) -> Tool {
        self.local_only = true;
        self
    }

    /// Marks the tool as read-only: it does not modify its environment (the
    /// MCP `readOnlyHint` annotation).
    pub fn read_only(mut self) -> Tool {
        self.read_only = true;
        self
    }

    /// Marks the tool as read-only in remote (HTTP) mode: it only writes
    /// files through [`Local`], e.g. into an `output_dir`, and remote mode
    /// has no `Local`, so there it only reads. Remote mode advertises it as
    /// read-only, while local mode uses its other flags.
    pub fn read_only_in_remote_mode(mut self) -> Tool {
        self.read_only_in_remote_mode = true;
        self
    }

    /// Marks the tool as destructive: it may delete or overwrite data (the
    /// MCP `destructiveHint` annotation). A tool that is neither read-only
    /// nor destructive only adds data, so MCP advertises it with
    /// `destructiveHint: false`.
    pub fn destructive(mut self) -> Tool {
        self.destructive = true;
        self
    }

    /// Sets the tool's human-readable title, e.g. `Get a run`, which MCP
    /// clients show instead of its name.
    pub fn with_title(mut self, title: &'static str) -> Tool {
        self.title = Some(title);
        self
    }

    /// Name of the tool, e.g. `decision_platform_run_get`.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Human-readable title of the tool, if it sets one. See
    /// [`with_title`](Self::with_title).
    pub fn title(&self) -> Option<&'static str> {
        self.title
    }

    /// One-sentence description of the tool.
    pub fn description(&self) -> &'static str {
        self.description
    }

    /// JSON Schema of the tool's arguments. It is always an object schema
    /// that sets `additionalProperties: false`, and so does every object
    /// schema in it that lists `properties`, unless its type takes other
    /// fields too, e.g. with a flattened map.
    pub fn input_schema(&self) -> &Map<String, Value> {
        &self.input_schema
    }

    /// Whether remote (HTTP) mode leaves the tool out. See
    /// [`local_only`](Self::local_only).
    pub fn is_local_only(&self) -> bool {
        self.local_only
    }

    /// Whether the tool only reads. See [`read_only`](Self::read_only).
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Whether the tool only reads in remote (HTTP) mode: it is read-only,
    /// or only writes through [`Local`]. See
    /// [`read_only_in_remote_mode`](Self::read_only_in_remote_mode).
    pub fn is_read_only_in_remote_mode(&self) -> bool {
        self.read_only || self.read_only_in_remote_mode
    }

    /// Whether the tool may destroy data. See
    /// [`destructive`](Self::destructive).
    pub fn is_destructive(&self) -> bool {
        self.destructive
    }

    /// Calls the tool with JSON arguments and returns its JSON result.
    ///
    /// `null` arguments are read as `{}`. The call runs in a new tokio task,
    /// so it must be awaited inside a tokio runtime. Dropping the returned
    /// future cancels the call.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] if `args`, at any depth, has
    /// a field the schema does not list or does not deserialize,
    /// [`ToolError::Failed`] if the SDK call fails, and
    /// [`ToolError::Panicked`] if it panics.
    pub fn call(&self, cx: Context, args: Value) -> BoxFuture<'static, Result<Value, ToolError>> {
        let args = match args {
            Value::Null => Value::Object(Map::new()),
            args => args,
        };
        // `#[serde(flatten)]` rules out `deny_unknown_fields`, and types
        // that responses share must accept new fields, so unknown fields are
        // rejected here, against the schema.
        if let Some(message) = unknown_field(&self.input_schema, &args, "") {
            return Box::pin(ready(Err(ToolError::InvalidArguments(message))));
        }
        (self.handler)(cx, args)
    }
}

impl fmt::Debug for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tool")
            .field("name", &self.name)
            .field("title", &self.title)
            .field("local_only", &self.local_only)
            .field("read_only", &self.read_only)
            .field("read_only_in_remote_mode", &self.read_only_in_remote_mode)
            .field("destructive", &self.destructive)
            .finish_non_exhaustive()
    }
}

/// Why a [`Tool::call`] failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum ToolError {
    /// The arguments do not match the tool's input schema. Holds the
    /// message, which names the offending field.
    InvalidArguments(String),
    /// The SDK call failed.
    Failed(Error),
    /// The SDK call panicked. Holds the panic message.
    Panicked(String),
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolError::InvalidArguments(message) => write!(f, "invalid arguments: {message}"),
            ToolError::Failed(error) => fmt::Display::fmt(error, f),
            ToolError::Panicked(message) => write!(f, "the tool panicked: {message}"),
        }
    }
}

impl StdError for ToolError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            // `Failed` is transparent: it shows the error's own message.
            ToolError::Failed(error) => error.source(),
            _ => None,
        }
    }
}

/// What a tool call may use: the platform, the local machine, or both.
///
/// Remote (HTTP) mode builds a context without [`Local`], so tools that
/// take filesystem paths fail there instead of touching the server's files.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Context {
    platform: Option<Platform>,
    local: Option<Local>,
}

impl Context {
    /// Creates an empty context, with neither the platform nor local mode.
    pub fn new() -> Context {
        Context::default()
    }

    /// Lets tool calls use `platform`.
    pub fn with_platform(mut self, platform: Platform) -> Context {
        self.platform = Some(platform);
        self
    }

    /// Lets tool calls use `local`: the local registry, local runs and the
    /// filesystem.
    pub fn with_local(mut self, local: Local) -> Context {
        self.local = Some(local);
        self
    }

    /// The same context without local mode. Remote mode passes every context
    /// through this, so that no tool reads or writes the server's files.
    pub(crate) fn without_local(mut self) -> Context {
        self.local = None;
        self
    }

    /// The platform.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if the context has no platform.
    pub fn platform(&self) -> Result<&Platform> {
        self.platform.as_ref().ok_or_else(|| Error::Invalid {
            reason: "this tool needs the platform, which is not configured".to_owned(),
        })
    }

    /// The local machine.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if the context has no local mode, e.g. in
    /// remote mode.
    pub fn local(&self) -> Result<&Local> {
        self.local.as_ref().ok_or_else(|| Error::Invalid {
            reason: "this tool reads or writes local files, which is only available in local mode"
                .to_owned(),
        })
    }
}

/// Tool arguments that address a workspace, plus the operation's
/// parameters.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct InWorkspace<P> {
    /// ID of the workspace.
    pub(crate) workspace_id: String,
    #[serde(flatten)]
    pub(crate) params: P,
}

/// Parameters of a tool whose operation takes no parameters.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct NoParams {}

thread_local! {
    /// Whether this thread is deserializing tool arguments.
    static READING_ARGUMENTS: Cell<bool> = const { Cell::new(false) };
}

/// Deserializes tool arguments into `A`, with [`reading_arguments`] set.
fn read_arguments<A: DeserializeOwned>(args: Value) -> serde_json::Result<A> {
    /// Restores the previous value, even if deserialization panics.
    struct Restore(bool);

    impl Drop for Restore {
        fn drop(&mut self) {
            READING_ARGUMENTS.set(self.0);
        }
    }

    let _restore = Restore(READING_ARGUMENTS.replace(true));
    serde_json::from_value(args)
}

/// Whether this thread is deserializing tool arguments. Fields that read
/// responses leniently check it, so that tool arguments stay strict at every
/// depth: e.g. `or_unknown` rejects enum values this SDK does not know here,
/// instead of reading them as `Unknown`.
pub(crate) fn reading_arguments() -> bool {
    READING_ARGUMENTS.get()
}

/// Generates the input schema of `A`: inlined, closed to unknown fields at
/// every depth, and without `$schema`, `title` and the root `description`,
/// which documents the Rust type rather than the tool.
fn input_schema<A: JsonSchema>() -> Map<String, Value> {
    let schema = SchemaSettings::draft2020_12()
        .with(|settings| settings.inline_subschemas = true)
        .into_generator()
        .into_root_schema_for::<A>();
    let mut schema = match Value::from(schema) {
        Value::Object(schema) => schema,
        _ => Map::new(),
    };
    schema.remove("$schema");
    schema.remove("title");
    schema.remove("description");
    schema.insert("additionalProperties".to_owned(), Value::Bool(false));
    close(&mut schema);
    schema
}

/// Sets `additionalProperties: false` on every object schema in `schema`
/// that lists `properties` and leaves it unset. A type that takes other
/// fields, e.g. with a flattened map, sets it already.
fn close(schema: &mut Map<String, Value>) {
    if schema.contains_key("properties") {
        schema
            .entry("additionalProperties")
            .or_insert(Value::Bool(false));
    }
    for (keyword, value) in schema.iter_mut() {
        let subschemas: Vec<&mut Value> = match (keyword.as_str(), value) {
            ("properties", Value::Object(properties)) => properties.values_mut().collect(),
            ("items" | "additionalProperties", value) => vec![value],
            ("anyOf" | "oneOf", Value::Array(branches)) => branches.iter_mut().collect(),
            _ => Vec::new(),
        };
        for subschema in subschemas {
            if let Value::Object(subschema) = subschema {
                close(subschema);
            }
        }
    }
}

/// The message for the first field of `value`, at any depth, that its
/// object schema in `schema` does not list. `path` is where `value` is in
/// the arguments, e.g. `secrets[0]`, and empty at the root.
///
/// An object or array that several `anyOf` or `oneOf` branches admit, as
/// with an untagged enum, is not checked.
fn unknown_field(schema: &Map<String, Value>, value: &Value, path: &str) -> Option<String> {
    // E.g. an `Option` of a tagged enum: `anyOf` the `oneOf` of its variants,
    // or null.
    let mut schema = schema;
    while !schema.contains_key("properties")
        && let Some(branch) = branch(schema, value)
    {
        schema = branch;
    }
    match value {
        Value::Object(fields) => {
            let properties = schema.get("properties").and_then(Value::as_object);
            let additional = schema.get("additionalProperties");
            for (key, field) in fields {
                let path = match path {
                    "" => key.clone(),
                    _ => format!("{path}.{key}"),
                };
                let field_schema = match (properties.and_then(|p| p.get(key)), additional) {
                    (Some(field_schema), _) => field_schema,
                    (None, Some(Value::Bool(false))) => {
                        let mut expected: Vec<&str> = properties
                            .map(|p| p.keys().map(String::as_str).collect())
                            .unwrap_or_default();
                        expected.sort_unstable();
                        return Some(match expected.as_slice() {
                            [] => format!("unknown field `{path}`, there are no fields"),
                            _ => format!(
                                "unknown field `{path}`, expected one of `{}`",
                                expected.join("`, `")
                            ),
                        });
                    }
                    (None, Some(field_schema)) => field_schema,
                    (None, None) => continue,
                };
                if let Some(field_schema) = field_schema.as_object()
                    && let Some(message) = unknown_field(field_schema, field, &path)
                {
                    return Some(message);
                }
            }
            None
        }
        Value::Array(items) => {
            let item_schema = schema.get("items")?.as_object()?;
            items
                .iter()
                .enumerate()
                .find_map(|(i, item)| unknown_field(item_schema, item, &format!("{path}[{i}]")))
        }
        _ => None,
    }
}

/// The only `anyOf` or `oneOf` branch of `schema` that admits the object or
/// array `value`: its type matches, and so do its `required` fields and
/// `const` properties, which tell enum variants apart.
fn branch<'a>(schema: &'a Map<String, Value>, value: &Value) -> Option<&'a Map<String, Value>> {
    let branches = schema.get("anyOf").or_else(|| schema.get("oneOf"))?;
    let mut admitting = branches
        .as_array()?
        .iter()
        .filter_map(Value::as_object)
        .filter(|branch| admits(branch, value));
    let first = admitting.next()?;
    admitting.next().is_none().then_some(first)
}

/// Whether `schema` may describe `value`, if it is an object or an array.
/// See [`branch`].
fn admits(schema: &Map<String, Value>, value: &Value) -> bool {
    let kind = match value {
        Value::Object(_) => "object",
        Value::Array(_) => "array",
        _ => return false,
    };
    let type_matches = match schema.get("type") {
        Some(Value::String(t)) => t == kind,
        Some(Value::Array(types)) => types.iter().any(|t| t == kind),
        _ => true,
    };
    let Some(fields) = value.as_object() else {
        return type_matches;
    };
    let required = schema.get("required").and_then(Value::as_array);
    let has_required = required
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .all(|name| fields.contains_key(name));
    let properties = schema.get("properties").and_then(Value::as_object);
    let consts_match = properties.into_iter().flatten().all(|(name, property)| {
        property
            .get("const")
            .is_none_or(|expected| fields.get(name) == Some(expected))
    });
    type_matches && has_required && consts_match
}

/// The message of a panicked task.
fn panic_message(error: JoinError) -> String {
    if !error.is_panic() {
        return "the tool's task was cancelled".to_owned();
    }
    let payload: Box<dyn Any + Send> = error.into_panic();
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => match payload.downcast::<&'static str>() {
            Ok(message) => (*message).to_owned(),
            Err(_) => "unknown panic payload".to_owned(),
        },
    }
}

/// Aborts the task when dropped, so cancelling a call cancels its task.
pub(crate) struct AbortOnDrop<T>(pub(crate) JoinHandle<T>);

impl<T> Future for AbortOnDrop<T> {
    type Output = Result<T, JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx)
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use serde_json::json;

    use super::*;

    /// Arguments of the test tool.
    #[derive(Debug, Deserialize, JsonSchema)]
    struct EchoArgs {
        /// The text to echo.
        text: String,
        /// How many times to repeat it.
        #[serde(default)]
        times: Option<u32>,
        #[serde(flatten)]
        extra: EchoExtra,
    }

    /// Flattened into [`EchoArgs`].
    #[derive(Debug, Default, Deserialize, JsonSchema)]
    #[serde(default)]
    struct EchoExtra {
        /// Whether to fail.
        fail: bool,
        /// Whether to panic.
        panic: bool,
    }

    fn echo() -> Tool {
        Tool::new("echo", "Echoes text.", |_cx, a: EchoArgs| async move {
            if a.extra.panic {
                panic!("echo panicked");
            }
            if a.extra.fail {
                return Err(Error::Invalid {
                    reason: "asked to fail".to_owned(),
                });
            }
            Ok(json!({ "text": a.text.repeat(a.times.unwrap_or(1) as usize) }))
        })
    }

    async fn call(tool: &Tool, args: Value) -> Result<Value, ToolError> {
        tool.call(Context::new(), args).await
    }

    #[test]
    fn derives_a_closed_object_schema_with_descriptions() {
        let schema = Value::Object(echo().input_schema().clone());
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], json!(["text"]));
        assert_eq!(
            schema["properties"]["text"]["description"],
            "The text to echo."
        );
        assert_eq!(
            schema["properties"]["fail"]["description"],
            "Whether to fail."
        );
        for key in ["$schema", "title", "description"] {
            assert!(schema.get(key).is_none(), "{key}");
        }
    }

    #[test]
    fn sets_the_flags() {
        let tool = echo();
        assert!(!tool.is_local_only() && !tool.is_read_only() && !tool.is_destructive());
        assert!(!tool.is_read_only_in_remote_mode());
        assert!(echo().read_only().is_read_only_in_remote_mode());
        let tool = tool.read_only_in_remote_mode();
        assert!(tool.is_read_only_in_remote_mode() && !tool.is_read_only());
        let tool = tool.local_only().read_only().destructive();
        assert!(tool.is_local_only() && tool.is_read_only() && tool.is_destructive());
        assert_eq!(tool.name(), "echo");
        assert_eq!(tool.description(), "Echoes text.");
        assert_eq!(tool.title(), None);
        assert_eq!(tool.with_title("Echo text").title(), Some("Echo text"));
    }

    #[tokio::test]
    async fn returns_the_result_as_json() {
        let result = call(&echo(), json!({ "text": "ab", "times": 2 })).await;
        assert_eq!(result.unwrap(), json!({ "text": "abab" }));
    }

    #[tokio::test]
    async fn reads_null_arguments_as_an_empty_object() {
        let tool = Tool::new("none", "Takes nothing.", |_cx, _: Nothing| async {
            Ok(true)
        });
        assert_eq!(call(&tool, Value::Null).await.unwrap(), json!(true));
        let err = call(&echo(), Value::Null).await.unwrap_err();
        assert!(
            matches!(&err, ToolError::InvalidArguments(m) if m.contains("missing field `text`")),
            "{err}"
        );
    }

    #[tokio::test]
    async fn rejects_unknown_and_invalid_fields() {
        let err = call(&echo(), json!({ "text": "a", "txet": "b" }))
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid arguments: unknown field `txet`, expected one of \
             `fail`, `panic`, `text`, `times`"
        );
        let err = call(&echo(), json!({ "text": 1 })).await.unwrap_err();
        assert!(
            matches!(&err, ToolError::InvalidArguments(m) if m.contains("invalid type")),
            "{err}"
        );
        let err = call(&echo(), json!(["a"])).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)), "{err}");
    }

    /// Arguments of a test tool with nested objects in every shape.
    #[derive(Debug, Deserialize, JsonSchema)]
    #[expect(dead_code, reason = "the tests only check field names")]
    struct NestArgs {
        /// A list of items.
        #[serde(default)]
        items: Vec<Item>,
        /// An optional item.
        item: Option<Item>,
        /// Items by name.
        #[serde(default)]
        by_name: BTreeMap<String, Item>,
        /// A tagged shape.
        shape: Option<Shape>,
        /// An item with extra fields.
        open: Option<OpenItem>,
        /// Any JSON.
        data: Option<Value>,
    }

    /// An item.
    #[derive(Debug, Deserialize, JsonSchema)]
    #[expect(dead_code, reason = "the tests only check field names")]
    struct Item {
        /// Name of the item.
        name: String,
        /// Note on the item.
        note: Option<String>,
    }

    /// A shape.
    #[derive(Debug, Deserialize, JsonSchema)]
    #[expect(dead_code, reason = "the tests only check field names")]
    #[serde(tag = "type", rename_all = "lowercase")]
    enum Shape {
        /// A circle.
        Circle {
            /// Radius of the circle.
            radius: f64,
        },
        /// A square.
        Square {
            /// Side of the square.
            side: f64,
        },
    }

    /// An item that keeps fields it does not know.
    #[derive(Debug, Deserialize, JsonSchema)]
    #[expect(dead_code, reason = "the tests only check field names")]
    struct OpenItem {
        /// Name of the item.
        name: String,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    }

    fn nest() -> Tool {
        Tool::new("nest", "Takes nested objects.", |_cx, _: NestArgs| async {
            Ok(true)
        })
    }

    #[test]
    fn closes_nested_object_schemas() {
        let schema = Value::Object(nest().input_schema().clone());
        let properties = &schema["properties"];
        assert_eq!(properties["items"]["items"]["additionalProperties"], false);
        assert_eq!(properties["item"]["additionalProperties"], false);
        let by_name = &properties["by_name"]["additionalProperties"];
        assert_eq!(by_name["additionalProperties"], false);
        for variant in properties["shape"]["anyOf"][0]["oneOf"].as_array().unwrap() {
            assert_eq!(variant["additionalProperties"], false);
        }
        assert_eq!(properties["open"]["additionalProperties"], true);
    }

    #[tokio::test]
    async fn rejects_unknown_fields_at_every_depth() {
        let ok = json!({
            "items": [{ "name": "a", "note": "b" }],
            "item": { "name": "a" },
            "by_name": { "a": { "name": "a" } },
            "shape": { "type": "circle", "radius": 1.0 },
            "open": { "name": "a", "color": "red" },
            "data": { "any": { "thing": 1 } },
        });
        assert_eq!(call(&nest(), ok).await.unwrap(), json!(true));
        let cases = [
            (
                json!({ "items": [{ "name": "a" }, { "name": "b", "nots": "c" }] }),
                "unknown field `items[1].nots`, expected one of `name`, `note`",
            ),
            (
                json!({ "item": { "name": "a", "nmae": "b" } }),
                "unknown field `item.nmae`, expected one of `name`, `note`",
            ),
            (
                json!({ "by_name": { "a": { "name": "a", "x": 1 } } }),
                "unknown field `by_name.a.x`, expected one of `name`, `note`",
            ),
            (
                json!({ "shape": { "type": "circle", "radius": 1.0, "side": 2.0 } }),
                "unknown field `shape.side`, expected one of `radius`, `type`",
            ),
        ];
        for (args, message) in cases {
            let err = call(&nest(), args).await.unwrap_err();
            assert!(matches!(err, ToolError::InvalidArguments(_)), "{err}");
            assert_eq!(err.to_string(), format!("invalid arguments: {message}"));
        }
    }

    #[tokio::test]
    async fn reports_sdk_errors_transparently() {
        let err = call(&echo(), json!({ "text": "a", "fail": true }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Failed(Error::Invalid { .. })));
        assert_eq!(err.to_string(), "invalid argument: asked to fail");
    }

    #[tokio::test]
    async fn turns_panics_into_errors() {
        let err = call(&echo(), json!({ "text": "a", "panic": true }))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Panicked(m) if m == "echo panicked"),
            "{err}"
        );
    }

    /// Arguments of a test tool that takes none.
    #[derive(Debug, Deserialize, JsonSchema)]
    struct Nothing {}

    /// Sets its flag when dropped.
    struct SetOnDrop(Arc<AtomicBool>);

    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn cancels_the_task_when_the_call_is_dropped() {
        let dropped = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&dropped);
        let tool = Tool::new("wait", "Waits.", move |_cx, _: Nothing| {
            let guard = SetOnDrop(Arc::clone(&flag));
            async move {
                let _guard = guard;
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok(())
            }
        });
        let call = tool.call(Context::new(), Value::Null);
        let timed_out = tokio::time::timeout(Duration::from_millis(50), call).await;
        assert!(timed_out.is_err());
        // The aborted task drops its future, and with it the guard.
        for _ in 0..100 {
            if dropped.load(Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the task kept running after the call was dropped");
    }

    #[test]
    fn checks_the_context() {
        let cx = Context::new();
        assert!(matches!(cx.platform(), Err(Error::Invalid { .. })));
        assert!(matches!(cx.local(), Err(Error::Invalid { .. })));
        let cx = cx.with_local(Local::with_home("/tmp/fico"));
        assert!(cx.local().is_ok());
        assert!(matches!(
            cx.without_local().local(),
            Err(Error::Invalid { .. })
        ));
    }

    #[test]
    fn moves_into_tasks() {
        // Roots, contexts and tools must move into tokio tasks, and be
        // wrappable by PyO3, so none of them may borrow.
        fn assert_task_safe<T: Clone + Send + Sync + 'static>() {}
        assert_task_safe::<Platform>();
        assert_task_safe::<Local>();
        assert_task_safe::<Context>();
        assert_task_safe::<Tool>();
    }
}
