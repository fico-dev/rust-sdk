//! The tools a server advertises for its [`SurfaceOptions`], and how MCP
//! describes them: their titles, their hints and the server's
//! instructions.

use std::collections::BTreeSet;
use std::sync::Arc;

use rmcp::model::ToolAnnotations;

use super::operations::{self, Catalog};
use super::{Mode, SurfaceOptions};
use crate::decision;
use crate::tool::Tool;

/// What the server tells clients about the curated tools of the default
/// surface, in workflow order: each sentence goes in only when the surface
/// has every tool it names.
const WORKFLOW: [(&[&str], &str); 6] = [
    (
        &["decision_workspace_get"],
        "To work on an existing workspace, start with `decision_workspace_get`.",
    ),
    (
        &["decision_manifest_validate"],
        "Check a workspace's `workspace.yaml` with `decision_manifest_validate`.",
    ),
    (
        &["decision_local_run_create"],
        "Run a workspace on this machine with `decision_local_run_create`.",
    ),
    (
        &["decision_run_create"],
        "Run an environment's build with `decision_run_create`.",
    ),
    (
        &["decision_acceptance_create", "decision_scenario_create"],
        "Test a candidate with `decision_acceptance_create` or `decision_scenario_create`.",
    ),
    (
        &["decision_guide"],
        "Call `decision_guide` before you create, change or push a workspace.",
    ),
];

/// Name prefixes of the operations that administer a whole account, which
/// only a server with `admin` on serves. Integrations are account-wide, and
/// their configurations hold credentials.
const ADMIN_PREFIXES: [&str; 3] = [
    "decision_platform_account_",
    "decision_platform_sso_",
    "decision_platform_integration_",
];

/// What a tool may do to its environment, in a given mode. Every tool has
/// exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Risk {
    /// It only reads.
    Read,
    /// It only adds data.
    Write,
    /// It may delete or overwrite data.
    Destructive,
}

impl Risk {
    /// The risk of `tool` in remote mode or in local mode. In remote mode,
    /// the tools that only write through `Local`, which it doesn't have,
    /// only read.
    pub(crate) fn of(tool: &Tool, remote: bool) -> Risk {
        let read_only = match remote {
            true => tool.is_read_only_in_remote_mode(),
            false => tool.is_read_only(),
        };
        match (read_only, tool.is_destructive()) {
            (true, _) => Risk::Read,
            (false, true) => Risk::Destructive,
            (false, false) => Risk::Write,
        }
    }

    /// The risk level's name: `read`, `write` or `destructive`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Risk::Read => "read",
            Risk::Write => "write",
            Risk::Destructive => "destructive",
        }
    }
}

/// Whether the operation named `name` administers a whole account: the
/// account, SSO and integration operations.
pub(crate) fn is_admin(name: &str) -> bool {
    ADMIN_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

/// The tools a server advertises, and what it tells clients about them.
pub(crate) struct Surface {
    pub(crate) tools: Vec<Tool>,
    pub(crate) instructions: String,
}

impl Surface {
    /// The surface that `options` select, in remote mode or in local mode.
    pub(crate) fn new(options: &SurfaceOptions, remote: bool) -> Surface {
        let served = |tool: &Tool| {
            !(remote && tool.is_local_only())
                && !(options.read_only && Risk::of(tool, remote) != Risk::Read)
        };
        let tools: Vec<Tool> = match options.mode {
            Mode::Granular => decision::tools()
                .into_iter()
                .filter(|tool| served(tool) && (options.admin || !is_admin(tool.name())))
                .collect(),
            Mode::Default | Mode::Minimal => {
                let curated: Vec<Tool> = match options.mode {
                    Mode::Default => super::decision::tools()
                        .into_iter()
                        .filter(served)
                        .collect(),
                    _ => Vec::new(),
                };
                // An operation named like an advertised tool is left to that tool.
                let shadowed: BTreeSet<&'static str> = curated.iter().map(Tool::name).collect();
                let catalog = Catalog::new(decision::tools(), options, remote, shadowed);
                let operations = operations::tools(Arc::new(catalog));
                curated
                    .into_iter()
                    .chain(operations.into_iter().filter(served))
                    .collect()
            }
        };
        let instructions = instructions(&tools, options, remote);
        Surface {
            tools,
            instructions,
        }
    }
}

/// The MCP definition of `tool`, in remote mode or in local mode.
pub(crate) fn definition(tool: &Tool, remote: bool) -> rmcp::model::Tool {
    let title = title(tool);
    // `destructiveHint` defaults to true, so every tool that is not
    // destructive says so.
    let annotations = match Risk::of(tool, remote) {
        Risk::Read => ToolAnnotations::with_title(&title)
            .read_only(true)
            .destructive(false)
            .idempotent(true),
        Risk::Write => ToolAnnotations::with_title(&title)
            .read_only(false)
            .destructive(false),
        Risk::Destructive => ToolAnnotations::with_title(&title)
            .read_only(false)
            .destructive(true),
    };
    let schema = Arc::new(tool.input_schema().clone());
    rmcp::model::Tool::new(tool.name(), tool.description(), schema)
        .with_title(title)
        .with_annotations(annotations)
}

/// The title of `tool`: its own, or one made from its name without the
/// capability, e.g. `Platform run get` for `decision_platform_run_get`.
fn title(tool: &Tool) -> String {
    if let Some(title) = tool.title() {
        return title.to_owned();
    }
    let name = tool.name();
    let operation = name
        .split_once('_')
        .map_or(name, |(_, operation)| operation);
    let words: Vec<&str> = operation
        .split('_')
        .map(|word| match word {
            "sso" => "SSO",
            word => word,
        })
        .collect();
    let title = words.join(" ");
    let mut chars = title.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => title,
    }
}

/// What the server tells clients about `tools`: what they work on, and how
/// to use them. It only names tools that the surface has.
fn instructions(tools: &[Tool], options: &SurfaceOptions, remote: bool) -> String {
    let has = |name: &str| tools.iter().any(|tool| tool.name() == name);
    let mut sentences: Vec<String> = Vec::new();
    sentences.push(match remote {
        true => "These tools work with decision workspaces, such as optimization models, on the \
                 FICO® platform."
            .to_owned(),
        false => "These tools work with decision workspaces, such as optimization models, on the \
                  FICO® platform and on this machine."
            .to_owned(),
    });
    sentences.push(
        "A workspace holds its builds and environments, and a run executes the build of an \
         environment on an input."
            .to_owned(),
    );
    if options.mode == Mode::Granular && has("decision_platform_workspace_get") {
        sentences.push(
            "Every operation is its own tool, named after what it works on and what it does, \
             e.g. `decision_platform_workspace_get`."
                .to_owned(),
        );
    }
    for (names, sentence) in WORKFLOW {
        if names.iter().all(|name| has(name)) {
            sentences.push(sentence.to_owned());
        }
    }
    if has("fico_operation_search") {
        let executors: Vec<String> = ["read", "write", "destructive"]
            .iter()
            .map(|risk| format!("fico_operation_{risk}"))
            .filter(|name| has(name))
            .map(|name| format!("`{name}`"))
            .collect();
        let executors = match executors.as_slice() {
            [one] => format!("with {one}"),
            [rest @ .., last] => format!(
                "with the tool of its risk level: {} or {last}",
                rest.join(", ")
            ),
            [] => String::new(),
        };
        let start = match options.mode {
            Mode::Default => "For anything else, find",
            _ => "Find",
        };
        sentences.push(format!(
            "{start} operations by keyword with `fico_operation_search`, then call one \
             {executors}."
        ));
    }
    if options.read_only {
        sentences.push(
            "This server is read-only: its tools only read, and cannot start, change or delete \
             anything."
                .to_owned(),
        );
    }
    sentences.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &'static str) -> Tool {
        crate::decision::tools()
            .into_iter()
            .find(|tool| tool.name() == name)
            .unwrap()
    }

    #[test]
    fn makes_titles_from_names() {
        let cases = [
            ("decision_platform_run_get", "Platform run get"),
            ("decision_manifest_init", "Manifest init"),
            (
                "decision_platform_sso_domain_delete",
                "Platform SSO domain delete",
            ),
            (
                "decision_platform_input_set_list",
                "Platform input set list",
            ),
        ];
        for (name, expected) in cases {
            assert_eq!(title(&tool(name)), expected);
        }
        let titled = tool("decision_manifest_init").with_title("Write a manifest");
        assert_eq!(title(&titled), "Write a manifest");
    }

    #[test]
    fn reads_only_in_remote_mode_what_only_writes_through_local() {
        let run_get = tool("decision_platform_run_get");
        assert_eq!(Risk::of(&run_get, false), Risk::Destructive);
        assert_eq!(Risk::of(&run_get, true), Risk::Read);
        let create = tool("decision_platform_run_create");
        assert_eq!(Risk::of(&create, false), Risk::Write);
        assert_eq!(Risk::of(&create, true), Risk::Write);
        let list = tool("decision_platform_run_list");
        assert_eq!(Risk::of(&list, false), Risk::Read);
    }

    #[test]
    fn finds_the_admin_operations() {
        let admin: Vec<&str> = crate::decision::tools()
            .iter()
            .map(Tool::name)
            .filter(|name| is_admin(name))
            .collect();
        assert_eq!(admin.len(), 18, "{admin:?}");
    }
}
