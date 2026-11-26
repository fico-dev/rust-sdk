//! Secrets collection types.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::OrUnknown;

/// A secrets collection, without its secrets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct SecretsCollectionSummary {
    /// ID of the secrets collection (API: `id`).
    #[serde(rename = "id")]
    pub collection_id: String,
    /// ID of the workspace the secrets collection belongs to (API:
    /// `application_id`).
    #[serde(rename = "application_id")]
    pub workspace_id: String,
    /// Name of the secrets collection.
    pub name: String,
    /// Description of the secrets collection.
    pub description: String,
    /// When the secrets collection was created, in RFC 3339.
    pub created_at: String,
    /// When the secrets collection was last updated, in RFC 3339.
    pub updated_at: String,
}

/// A secrets collection, including its secrets.
///
/// On the wire it is one object: the fields of `summary` plus `secrets`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct SecretsCollection {
    /// ID, name, description and dates of the secrets collection.
    #[serde(flatten)]
    pub summary: SecretsCollectionSummary,
    /// The secrets of the collection.
    pub secrets: Vec<Secret>,
}

impl SecretsCollection {
    /// The collection with the value of each secret removed, as the
    /// `decision_platform_secrets_get` tool returns it.
    ///
    /// [`Platform::create_secrets`](crate::decision::platform::Platform::create_secrets)
    /// and
    /// [`Platform::update_secrets`](crate::decision::platform::Platform::update_secrets)
    /// reject a secret without a value, so its secrets cannot be written
    /// back by mistake.
    pub fn without_values(mut self) -> SecretsCollection {
        for secret in &mut self.secrets {
            secret.value = None;
        }
        self
    }
}

/// How a run receives a secret.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum SecretType {
    /// As an environment variable.
    #[default]
    Env,
    /// As a file.
    File,
    /// A value this version of the SDK does not know, read from a response
    /// field. Requests should not use it: tool arguments reject it, and tool
    /// schemas leave it out.
    #[serde(skip_deserializing)]
    #[schemars(skip)]
    Unknown,
}

impl OrUnknown for SecretType {
    const UNKNOWN: Self = SecretType::Unknown;
}

/// A secret, and where a run finds it.
///
/// Its `Debug` output hides the secret's value.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Secret {
    /// How a run receives the secret (API: `type`).
    #[serde(rename = "type", deserialize_with = "super::or_unknown")]
    pub secret_type: SecretType,
    /// Where a run finds the secret: the name of the environment variable,
    /// or the path of the file.
    pub location: String,
    /// Value of the secret. Creating or updating secrets needs it. The
    /// platform returns it masked, e.g. `***************`, and a collection
    /// whose values were removed leaves it out.
    // Tool schemas require it, and `#[serde(default)]` would make schemars
    // drop it from `required`. serde reads a missing `Option` as `None`
    // without it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(required)]
    pub value: Option<String>,
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = self.value.as_ref().map(|_| "<redacted>");
        f.debug_struct("Secret")
            .field("secret_type", &self.secret_type)
            .field("location", &self.location)
            .field("value", &value)
            .finish()
    }
}
