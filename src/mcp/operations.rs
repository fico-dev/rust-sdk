//! The `fico_operation_*` tools, shared by every capability: one that
//! searches the operation catalog, and one that calls its operations for
//! each risk level, so that clients can approve reads, writes and
//! destructive calls apart.

use std::collections::BTreeSet;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::SurfaceOptions;
use super::surface::{Risk, is_admin};
use crate::tool::{Context, Tool, ToolError};
use crate::{Error, Result};

/// How many operations a search returns by default, and at most.
const DEFAULT_LIMIT: usize = 5;
const MAX_LIMIT: usize = 20;

/// Words of a search query too common to look for.
const STOP_WORDS: [&str; 7] = ["the", "and", "for", "with", "from", "that", "this"];

const SEARCH: &str = "Finds operations by keywords in their names and descriptions, best \
    matches first, and returns each one's name, description, risk level and input schema. Use \
    it when no other tool does what you need, e.g. for shadow and switchback tests, ensembles, \
    input sets, managed inputs, secrets or updates. Then call the operation with the tool of its \
    risk level: `fico_operation_read`, `fico_operation_write` or `fico_operation_destructive`.";
const SEARCH_READ_ONLY: &str = "Finds operations by keywords in their names and descriptions, \
    best matches first, and returns each one's name, description, risk level and input schema. \
    Use it when no other tool does what you need, e.g. to read shadow and switchback tests, \
    ensembles, input sets, managed inputs or secrets. Then call the operation with \
    `fico_operation_read`: this server is read-only, so it only finds operations that read.";
const READ: &str = "Calls a read-only operation by name, with its arguments, and returns its \
    result. Find the operation and its input schema with `fico_operation_search` first; its \
    risk level must be `read`. Operations that add data go through `fico_operation_write`, and \
    operations that delete or overwrite data through `fico_operation_destructive`.";
const READ_READ_ONLY: &str = "Calls a read-only operation by name, with its arguments, and \
    returns its result. Find the operation and its input schema with `fico_operation_search` \
    first; its risk level must be `read`. This server is read-only, so it calls no operation \
    that changes data.";
const WRITE: &str = "Calls an operation that only adds data, such as creating a test or an \
    input set, by name, with its arguments, and returns its result. Find the operation and its \
    input schema with `fico_operation_search` first; its risk level must be `write`. Read-only \
    operations go through `fico_operation_read`, and operations that delete or overwrite data \
    through `fico_operation_destructive`.";
const DESTRUCTIVE: &str = "Calls an operation that may delete or overwrite data, such as an \
    update, a delete or a stop, by name, with its arguments, and returns its result. Find the \
    operation and its input schema with `fico_operation_search` first; its risk level must be \
    `destructive`. Read-only operations go through `fico_operation_read`, and operations that \
    only add data through `fico_operation_write`.";

/// The operations that the `fico_operation_*` tools of a server can find
/// and call: those of the catalog that its options serve.
pub(crate) struct Catalog {
    operations: Vec<Tool>,
    remote: bool,
    admin: bool,
    read_only: bool,
    /// Operations that an advertised tool of the same name takes the place
    /// of.
    shadowed: BTreeSet<&'static str>,
}

impl Catalog {
    /// The catalog of `operations` for a server with `options`, in remote
    /// mode or in local mode, where the advertised tools named in
    /// `shadowed` take the place of the operations of the same name.
    pub(crate) fn new(
        operations: Vec<Tool>,
        options: &SurfaceOptions,
        remote: bool,
        shadowed: BTreeSet<&'static str>,
    ) -> Catalog {
        Catalog {
            operations,
            remote,
            admin: options.admin,
            read_only: options.read_only,
            shadowed,
        }
    }

    /// The operation named `name` and its risk level, if this server calls
    /// it, or why it doesn't.
    fn operation(&self, name: &str) -> Result<(&Tool, Risk)> {
        let invalid = |reason: String| Err(Error::Invalid { reason });
        let Some(operation) = self.operations.iter().find(|tool| tool.name() == name) else {
            return invalid(format!(
                "there is no operation `{name}`; find operations with `fico_operation_search`"
            ));
        };
        let risk = Risk::of(operation, self.remote);
        if self.shadowed.contains(name) {
            invalid(format!("call the `{name}` tool directly instead"))
        } else if self.remote && operation.is_local_only() {
            invalid(format!(
                "`{name}` reads or writes local files, so it is only available in local mode"
            ))
        } else if !self.admin && is_admin(name) {
            invalid(format!(
                "`{name}` administers a whole account, which this server does not serve"
            ))
        } else if self.read_only && risk != Risk::Read {
            invalid(format!(
                "`{name}` changes data, and this server is read-only"
            ))
        } else {
            Ok((operation, risk))
        }
    }

    /// The operations this server calls, with the score of their match for
    /// `query`, best first, leaving out those that don't match.
    fn search(&self, query: &str, capability: Option<Capability>) -> Result<Vec<&Tool>> {
        let mut terms: Vec<String> = words(query)
            .filter(|word| word.len() >= 3 && !STOP_WORDS.contains(&word.as_str()))
            .collect();
        terms.sort_unstable();
        terms.dedup();
        if terms.is_empty() {
            return Err(Error::Invalid {
                reason: "`query` needs at least one keyword of three letters or more, \
                         e.g. `shadow test`"
                    .to_owned(),
            });
        }
        let prefix = capability.map(Capability::prefix);
        let mut found: Vec<(usize, &Tool)> = self
            .operations
            .iter()
            .filter(|tool| prefix.is_none_or(|prefix| tool.name().starts_with(prefix)))
            .filter(|tool| self.operation(tool.name()).is_ok())
            .map(|tool| (score(&terms, tool), tool))
            .filter(|(score, _)| *score > 0)
            .collect();
        found.sort_by(|(a, x), (b, y)| b.cmp(a).then_with(|| x.name().cmp(y.name())));
        Ok(found.into_iter().map(|(_, tool)| tool).collect())
    }
}

/// The lowercase words of `text`: its runs of letters and digits.
fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
}

/// Whether the query term `term` matches the word `word`: one starts with
/// the other, e.g. `run` and `runs`, or `env` and `environment`.
fn matches(term: &str, word: &str) -> bool {
    word.starts_with(term) || (term.starts_with(word) && word.len() >= 3)
}

/// How well `tool` matches `terms`: 3 for every term in its name, and 1 for
/// every other term in its description.
fn score(terms: &[String], tool: &Tool) -> usize {
    let name: Vec<String> = words(tool.name()).collect();
    let description: Vec<String> = words(tool.description()).collect();
    terms
        .iter()
        .map(|term| {
            if name.iter().any(|word| matches(term, word)) {
                3
            } else if description.iter().any(|word| matches(term, word)) {
                1
            } else {
                0
            }
        })
        .sum()
}

/// A capability whose operations a search can be limited to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Capability {
    Decision,
}

impl Capability {
    /// The prefix of the names of the capability's operations.
    fn prefix(self) -> &'static str {
        match self {
            Capability::Decision => "decision_",
        }
    }
}

/// Arguments of `fico_operation_search`.
#[derive(Debug, Deserialize, JsonSchema)]
struct SearchArgs {
    /// Keywords to look for in the names and descriptions of operations,
    /// e.g. `shadow test start`.
    query: String,
    /// Only operations of this capability.
    capability: Option<Capability>,
    /// Most operations to return, from 1 to 20. Defaults to 5.
    limit: Option<usize>,
}

/// Result of `fico_operation_search`.
#[derive(Debug, Serialize)]
struct Found<'a> {
    /// The best matches, best first.
    operations: Vec<Operation<'a>>,
    /// How many operations match, including those beyond the limit.
    total: usize,
}

/// An operation that a search found.
#[derive(Debug, Serialize)]
struct Operation<'a> {
    name: &'static str,
    description: &'static str,
    /// `read`, `write` or `destructive`: the tool that calls it is
    /// `fico_operation_<risk>`.
    risk: &'static str,
    input_schema: &'a Map<String, Value>,
}

/// Arguments of the `fico_operation_*` tools that call an operation.
#[derive(Debug, Deserialize, JsonSchema)]
struct CallArgs {
    /// Name of the operation, as `fico_operation_search` returns it, e.g.
    /// `decision_platform_run_get`.
    name: String,
    /// Arguments of the operation, as its input schema describes them.
    #[serde(default)]
    arguments: Map<String, Value>,
}

/// The `fico_operation_*` tools over `catalog`.
pub(crate) fn tools(catalog: Arc<Catalog>) -> Vec<Tool> {
    let read_only = catalog.read_only;
    let search = {
        let catalog = Arc::clone(&catalog);
        Tool::new(
            "fico_operation_search",
            if read_only { SEARCH_READ_ONLY } else { SEARCH },
            move |_cx, a: SearchArgs| {
                let catalog = Arc::clone(&catalog);
                async move {
                    let found = catalog.search(&a.query, a.capability)?;
                    let limit = a.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
                    let operations = found.iter().take(limit).map(|tool| Operation {
                        name: tool.name(),
                        description: tool.description(),
                        risk: Risk::of(tool, catalog.remote).as_str(),
                        input_schema: tool.input_schema(),
                    });
                    let found = Found {
                        operations: operations.collect(),
                        total: found.len(),
                    };
                    serde_json::to_value(found).map_err(|e| Error::Encode {
                        reason: "search result is not serializable".to_owned(),
                        source: Some(Box::new(e)),
                    })
                }
            },
        )
        .with_title("Search operations")
        .read_only()
    };
    let read = executor(
        &catalog,
        Risk::Read,
        "fico_operation_read",
        if read_only { READ_READ_ONLY } else { READ },
    )
    .with_title("Call a read-only operation")
    .read_only();
    let write = executor(&catalog, Risk::Write, "fico_operation_write", WRITE)
        .with_title("Call a write operation");
    let destructive = executor(
        &catalog,
        Risk::Destructive,
        "fico_operation_destructive",
        DESTRUCTIVE,
    )
    .with_title("Call a destructive operation")
    .destructive();
    vec![search, read, write, destructive]
}

/// The tool named `name` that calls the operations of `catalog` whose risk
/// level is `risk`.
fn executor(
    catalog: &Arc<Catalog>,
    risk: Risk,
    name: &'static str,
    description: &'static str,
) -> Tool {
    let catalog = Arc::clone(catalog);
    Tool::new(name, description, move |cx, a: CallArgs| {
        let catalog = Arc::clone(&catalog);
        async move { execute(&catalog, risk, cx, a).await }
    })
}

/// Calls the operation of `catalog` that `args` names, if its risk level is
/// `risk`.
async fn execute(catalog: &Catalog, risk: Risk, cx: Context, args: CallArgs) -> Result<Value> {
    let (operation, actual) = catalog.operation(&args.name)?;
    if actual != risk {
        let actual = actual.as_str();
        return Err(Error::Invalid {
            reason: format!(
                "`{}` is a {actual} operation: call it with `fico_operation_{actual}`",
                args.name
            ),
        });
    }
    match operation.call(cx, Value::Object(args.arguments)).await {
        Ok(value) => Ok(value),
        Err(ToolError::Failed(error)) => Err(error),
        Err(ToolError::InvalidArguments(message)) => Err(Error::Invalid {
            reason: format!("arguments of `{}`: {message}", args.name),
        }),
        // Panics again, so that the call fails as this tool's own panic.
        Err(ToolError::Panicked(message)) => std::panic::resume_unwind(Box::new(message)),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::mcp::Mode;

    fn catalog(options: &SurfaceOptions, remote: bool, shadowed: &[&'static str]) -> Arc<Catalog> {
        let shadowed = shadowed.iter().copied().collect();
        Arc::new(Catalog::new(
            crate::decision::tools(),
            options,
            remote,
            shadowed,
        ))
    }

    fn tool<'a>(tools: &'a [Tool], name: &str) -> &'a Tool {
        tools.iter().find(|tool| tool.name() == name).unwrap()
    }

    async fn search(catalog: &Arc<Catalog>, args: Value) -> Result<Value, ToolError> {
        let tools = tools(Arc::clone(catalog));
        tool(&tools, "fico_operation_search")
            .call(Context::new(), args)
            .await
    }

    fn found(result: &Value) -> Vec<&str> {
        let operations = result["operations"].as_array().unwrap();
        operations
            .iter()
            .map(|operation| operation["name"].as_str().unwrap())
            .collect()
    }

    #[tokio::test]
    async fn finds_operations_by_keywords_with_their_schema() {
        let local = catalog(&SurfaceOptions::default(), false, &[]);
        let result = search(&local, json!({ "query": "start a shadow test" }))
            .await
            .unwrap();
        assert_eq!(found(&result)[0], "decision_platform_shadow_start");
        let first = &result["operations"][0];
        assert_eq!(first["risk"], "write");
        assert_eq!(first["description"], "Starts a shadow test.");
        let properties = &first["input_schema"]["properties"];
        assert!(properties["workspace_id"].is_object(), "{first}");
        let result = search(&local, json!({ "query": "environments", "limit": 2 }))
            .await
            .unwrap();
        assert_eq!(found(&result).len(), 2);
        assert!(result["total"].as_u64().unwrap() > 2);
        for operation in found(&result) {
            assert!(operation.contains("environment"), "{operation}");
        }
        let err = search(&local, json!({ "query": "a to" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("at least one keyword"), "{err}");
    }

    #[tokio::test]
    async fn finds_only_what_this_server_calls() {
        let options = SurfaceOptions::default();
        let query = json!({ "query": "local run account sso workspace", "limit": 20 });
        let remote = search(&catalog(&options, true, &[]), query.clone())
            .await
            .unwrap();
        let names = found(&remote);
        assert!(!names.is_empty());
        assert!(!names.iter().any(|name| name.starts_with("decision_local_")));
        assert!(!names.iter().any(|name| is_admin(name)));
        let admin = catalog(&options.clone().admin(true), false, &[]);
        let names = search(&admin, query.clone()).await.unwrap();
        assert!(found(&names).iter().any(|name| is_admin(name)));
        let read_only = catalog(&options.clone().read_only(true), false, &[]);
        let result = search(&read_only, query.clone()).await.unwrap();
        for operation in result["operations"].as_array().unwrap() {
            assert_eq!(operation["risk"], "read", "{operation}");
        }
        let shadowed = catalog(&options, false, &["decision_local_run_list"]);
        let result = search(&shadowed, json!({ "query": "local runs", "limit": 20 }))
            .await
            .unwrap();
        assert!(!found(&result).contains(&"decision_local_run_list"));
        assert!(found(&result).contains(&"decision_local_run_get"));
        let query = json!({ "query": "run", "capability": "decision", "limit": 20 });
        let result = search(&catalog(&options, false, &[]), query).await.unwrap();
        assert!(
            found(&result)
                .iter()
                .all(|name| name.starts_with("decision_"))
        );
    }

    /// Arguments of the test operations.
    #[derive(Debug, Deserialize, JsonSchema)]
    struct TestArgs {
        /// A required field.
        id: String,
    }

    /// A test operation named `name` that returns its name and argument,
    /// or panics for the ID `panic`.
    fn operation(name: &'static str) -> Tool {
        Tool::new(name, "Tests.", move |_cx, a: TestArgs| async move {
            if a.id == "panic" {
                panic!("boom");
            }
            Ok(json!({ "operation": name, "id": a.id }))
        })
    }

    /// A catalog of test operations: one of each risk level, one that only
    /// reads in remote mode, a local-only one, an admin one and one that a
    /// curated tool shadows.
    fn test_catalog(options: &SurfaceOptions, remote: bool) -> Vec<Tool> {
        let operations = vec![
            operation("test_read").read_only(),
            operation("test_write"),
            operation("test_destructive").destructive(),
            operation("test_output_dir_get")
                .destructive()
                .read_only_in_remote_mode(),
            operation("test_local").local_only().read_only(),
            operation("decision_platform_account_get").read_only(),
            operation("decision_manifest_validate").read_only(),
        ];
        let shadowed = ["decision_manifest_validate"].into_iter().collect();
        tools(Arc::new(Catalog::new(
            operations, options, remote, shadowed,
        )))
    }

    async fn execute(tools: &[Tool], executor: &str, name: &str) -> Result<Value, ToolError> {
        let args = json!({ "name": name, "arguments": { "id": "a" } });
        tool(tools, executor).call(Context::new(), args).await
    }

    fn rejection(result: Result<Value, ToolError>) -> String {
        match result {
            Err(ToolError::Failed(Error::Invalid { reason })) => reason,
            other => panic!("not rejected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn calls_operations_of_its_risk_level_only() {
        let tools = test_catalog(&SurfaceOptions::default(), false);
        let executors = [
            ("fico_operation_read", "test_read"),
            ("fico_operation_write", "test_write"),
            ("fico_operation_destructive", "test_destructive"),
        ];
        for (executor, name) in executors {
            let result = execute(&tools, executor, name).await.unwrap();
            assert_eq!(result, json!({ "operation": name, "id": "a" }));
            for (other, _) in executors.iter().filter(|(other, _)| *other != executor) {
                let reason = rejection(execute(&tools, other, name).await);
                assert!(
                    reason.contains(&format!("call it with `{executor}`")),
                    "{reason}"
                );
            }
        }
    }

    #[tokio::test]
    async fn reads_in_remote_mode_what_only_writes_through_local() {
        let options = SurfaceOptions::default();
        let local = test_catalog(&options, false);
        let name = "test_output_dir_get";
        execute(&local, "fico_operation_destructive", name)
            .await
            .unwrap();
        rejection(execute(&local, "fico_operation_read", name).await);
        let remote = test_catalog(&options, true);
        execute(&remote, "fico_operation_read", name).await.unwrap();
        rejection(execute(&remote, "fico_operation_destructive", name).await);
    }

    #[tokio::test]
    async fn rejects_operations_this_server_does_not_call() {
        let options = SurfaceOptions::default().mode(Mode::Minimal);
        let local = test_catalog(&options, false);
        let remote = test_catalog(&options, true);
        let read = "fico_operation_read";
        let cases = [
            (
                &local,
                read,
                "test_nothing",
                "find operations with `fico_operation_search`",
            ),
            (&remote, read, "test_local", "only available in local mode"),
            (
                &local,
                read,
                "decision_platform_account_get",
                "administers a whole account",
            ),
            (
                &local,
                read,
                "decision_manifest_validate",
                "call the `decision_manifest_validate` tool",
            ),
        ];
        for (tools, executor, name, expected) in cases {
            let reason = rejection(execute(tools, executor, name).await);
            assert!(reason.contains(expected), "{name}: {reason}");
        }
        execute(&local, read, "test_local").await.unwrap();
        let admin = test_catalog(&options.clone().admin(true), false);
        execute(&admin, read, "decision_platform_account_get")
            .await
            .unwrap();
        let read_only = test_catalog(&options.read_only(true), false);
        let reason = rejection(execute(&read_only, "fico_operation_write", "test_write").await);
        assert!(reason.contains("this server is read-only"), "{reason}");
    }

    #[tokio::test]
    async fn names_the_bad_field_of_the_operation() {
        let tools = test_catalog(&SurfaceOptions::default(), false);
        let cases = [
            (json!({}), "missing field `id`"),
            (json!({ "id": "a", "nmae": "b" }), "unknown field `nmae`"),
        ];
        for (arguments, expected) in cases {
            let args = json!({ "name": "test_read", "arguments": arguments });
            let result = tool(&tools, "fico_operation_read")
                .call(Context::new(), args)
                .await;
            let reason = rejection(result);
            assert!(reason.starts_with("arguments of `test_read`: "), "{reason}");
            assert!(reason.contains(expected), "{reason}");
        }
    }

    #[tokio::test]
    async fn fails_as_a_panic_when_the_operation_panics() {
        let tools = test_catalog(&SurfaceOptions::default(), false);
        let args = json!({ "name": "test_read", "arguments": { "id": "panic" } });
        let result = tool(&tools, "fico_operation_read")
            .call(Context::new(), args)
            .await;
        assert!(
            matches!(&result, Err(ToolError::Panicked(message)) if message == "boom"),
            "{result:?}"
        );
    }
}
