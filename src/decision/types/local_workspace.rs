//! Local registry types.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::ser::Error as _;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};

use super::run::ContentFormat;

/// A workspace in the local registry: its directory, with an ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct LocalWorkspace {
    /// ID of the workspace in the local registry.
    pub workspace_id: String,
    /// Directory of the workspace, the one that holds its `workspace.yaml`:
    /// an absolute path, with symlinks resolved. On Windows, it has the
    /// `\\?\` prefix that resolving symlinks gives, as in
    /// `\\?\C:\workspaces\routing`.
    pub src: PathBuf,
    /// Format of the workspace's input and output, from its manifest.
    #[serde(deserialize_with = "super::or_unknown")]
    pub content_format: ContentFormat,
    /// When the workspace was registered, as an RFC 3339 timestamp in UTC.
    pub created_at: String,
    /// When the workspace's entry last changed, as an RFC 3339 timestamp in
    /// UTC.
    pub updated_at: String,
    /// Description of the workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Identifies a workspace in the local registry: by its ID, by its source
/// directory, or by both.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LocalWorkspaceRef {
    /// ID of the workspace in the local registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// Source directory of the workspace: the directory that holds its
    /// `workspace.yaml`. A relative path is resolved against the current
    /// directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub src: Option<PathBuf>,
}

/// The local registry file: every registered workspace.
///
/// Writing it back keeps what this version of the SDK does not know: the
/// other keys of the file and of its entries, and content formats that read
/// as `Unknown`. So a registry that a newer version wrote loses nothing when
/// this version changes it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Registry {
    /// The registered workspaces, in the order they were registered.
    #[serde(default)]
    pub(crate) workspaces: Vec<RegistryEntry>,
    /// The other keys of the file, as written.
    #[serde(flatten)]
    other: Map<String, Value>,
}

/// An entry of the registry file: a registered workspace, and the JSON
/// object it was read from.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "Map<String, Value>")]
pub(crate) struct RegistryEntry {
    /// The workspace, as this version reads it. Changes go here.
    pub(crate) workspace: LocalWorkspace,
    /// The object the entry was read from, empty for a new entry. Writing
    /// the entry back overwrites its keys with the workspace's fields, so
    /// the keys this version does not know keep their values.
    raw: Map<String, Value>,
}

impl RegistryEntry {
    /// A new entry for `workspace`.
    pub(crate) fn new(workspace: LocalWorkspace) -> RegistryEntry {
        RegistryEntry {
            workspace,
            raw: Map::new(),
        }
    }
}

impl TryFrom<Map<String, Value>> for RegistryEntry {
    type Error = serde_json::Error;

    fn try_from(raw: Map<String, Value>) -> Result<RegistryEntry, serde_json::Error> {
        let workspace = serde_json::from_value(Value::Object(raw.clone()))?;
        Ok(RegistryEntry { workspace, raw })
    }
}

impl Serialize for RegistryEntry {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let Value::Object(fields) =
            serde_json::to_value(&self.workspace).map_err(S::Error::custom)?
        else {
            return Err(S::Error::custom("a local workspace must be a JSON object"));
        };
        let mut entry = self.raw.clone();
        for (key, value) in fields {
            // A content format this version does not know reads as
            // `Unknown`, so it keeps the value it was read with.
            let unknown = self.workspace.content_format == ContentFormat::Unknown;
            if key == "content_format" && unknown && entry.contains_key(&key) {
                continue;
            }
            entry.insert(key, value);
        }
        entry.serialize(serializer)
    }
}
