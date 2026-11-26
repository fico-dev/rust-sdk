//! Secrets collections, on `v1/applications/{workspace_id}/secrets`.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::Platform;
use crate::decision::id::safe_id;
use crate::decision::path::api_path;
use crate::decision::{Secret, SecretType, SecretsCollection, SecretsCollectionSummary};
use crate::tool::{InWorkspace, NoParams, Tool};
use crate::{Error, Result};

/// Tool arguments that address a secrets collection, plus the operation's
/// parameters.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct SecretsRef<P> {
    /// ID of the workspace.
    pub(crate) workspace_id: String,
    /// ID of the secrets collection.
    pub(crate) collection_id: String,
    #[serde(flatten)]
    pub(crate) params: P,
}

/// Parameters for [`Platform::create_secrets`].
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct SecretsCreateParams {
    /// ID of the secrets collection. Generated when not set.
    pub collection_id: Option<String>,
    /// Name of the secrets collection. Defaults to its ID.
    pub name: Option<String>,
    /// Description of the secrets collection.
    pub description: Option<String>,
    /// The secrets of the collection. At least one.
    pub secrets: Vec<Secret>,
}

/// Parameters for [`Platform::update_secrets`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SecretsUpdateParams {
    /// New name of the secrets collection.
    pub name: Option<String>,
    /// New description of the secrets collection.
    pub description: Option<String>,
    /// New secrets of the collection. They replace all of its secrets, so
    /// list every secret to keep, unless `keep_existing` is set. At least
    /// one.
    pub secrets: Option<Vec<Secret>>,
    /// Whether to keep the collection's secrets that `secrets` leaves out.
    /// By default they are deleted.
    pub keep_existing: bool,
}

impl Platform {
    /// Creates a secrets collection.
    ///
    /// An unset or empty `collection_id` is generated, and an unset or empty
    /// `name` defaults to the ID.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `secrets` is empty, or a secret has no
    /// value or an `Unknown` type.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn create_secrets(
        &self,
        workspace_id: &str,
        params: SecretsCreateParams,
    ) -> Result<SecretsCollectionSummary> {
        check_secrets(&params.secrets)?;
        let id = match params.collection_id.filter(|id| !id.is_empty()) {
            Some(id) => id,
            None => safe_id("secrets"),
        };
        let name = match params.name.filter(|name| !name.is_empty()) {
            Some(name) => name,
            None => id.clone(),
        };
        let mut body = json!({ "id": id, "name": name, "secrets": params.secrets });
        if let Some(description) = params.description {
            body["description"] = description.into();
        }
        let path = api_path(&["v1", "applications", workspace_id, "secrets"])?;
        self.client.post(path).json(&body).send_json().await
    }

    /// Gets a secrets collection, including its secrets.
    ///
    /// The platform masks the value of each secret, e.g. `***************`,
    /// and never returns it. [`SecretsCollection::without_values`] removes
    /// the masked values.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Status`] with status 404 if the secrets collection does
    /// not exist, and the [errors of every method](Platform#errors).
    pub async fn get_secrets(
        &self,
        workspace_id: &str,
        collection_id: &str,
    ) -> Result<SecretsCollection> {
        let path = api_path(&["v1", "applications", workspace_id, "secrets", collection_id])?;
        self.client.get(path).send_json().await
    }

    /// Lists the secrets collections of a workspace, without their secrets.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn list_secrets(&self, workspace_id: &str) -> Result<Vec<SecretsCollectionSummary>> {
        let path = api_path(&["v1", "applications", workspace_id, "secrets"])?;
        self.client.get(path).paginate().collect().await
    }

    /// Updates a secrets collection.
    ///
    /// Fields that `params` leaves unset keep their values. `secrets`
    /// replaces every secret of the collection.
    ///
    /// It sends only what changes, with `PATCH`, because the platform masks
    /// the values of the secrets it returns and rejects them when they are
    /// sent back. A name or description alone is one request. With
    /// `secrets`, it first gets the collection, for the type and location of
    /// each secret, never its value: a secret with the type and location of
    /// an existing one updates it, a new one is created, and the existing
    /// ones that `secrets` leaves out are deleted, or kept with
    /// `keep_existing`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`], before any request, if `params` sets no
    /// field, or if `secrets` is empty, a secret has no value or an
    /// `Unknown` type, or two secrets have the same type and location.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn update_secrets(
        &self,
        workspace_id: &str,
        collection_id: &str,
        params: SecretsUpdateParams,
    ) -> Result<SecretsCollectionSummary> {
        if params.name.is_none() && params.description.is_none() && params.secrets.is_none() {
            return Err(Error::Invalid {
                reason: "set at least one of `name`, `description` and `secrets`".to_owned(),
            });
        }
        if let Some(secrets) = &params.secrets {
            check_secrets(secrets)?;
            check_unique_keys(secrets)?;
        }
        let path = api_path(&["v1", "applications", workspace_id, "secrets", collection_id])?;
        let mut body = Map::new();
        if let Some(name) = params.name {
            body.insert("name".to_owned(), name.into());
        }
        if let Some(description) = params.description {
            body.insert("description".to_owned(), description.into());
        }
        if let Some(secrets) = params.secrets {
            // Only the keys of the existing secrets are read: their values,
            // which are masked, never go back.
            let collection: Map<String, Value> = self.client.get(&path).send_json().await?;
            let existing = existing_keys(&collection);
            let mut operations = Vec::with_capacity(secrets.len());
            let mut kept = BTreeSet::new();
            for secret in &secrets {
                let key = (wire_type(secret), secret.location.clone());
                let op = if existing.contains(&key) {
                    "update"
                } else {
                    "create"
                };
                let mut operation = json!({ "op": op });
                if let (Value::Object(operation), Value::Object(secret)) =
                    (&mut operation, json!(secret))
                {
                    operation.extend(secret);
                }
                operations.push(operation);
                kept.insert(key);
            }
            if !params.keep_existing {
                for (secret_type, location) in existing.difference(&kept) {
                    operations
                        .push(json!({ "op": "delete", "type": secret_type, "location": location }));
                }
            }
            body.insert("operations".to_owned(), operations.into());
        }
        self.client.patch(path).json(&body).send_json().await
    }

    /// Deletes a secrets collection.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn delete_secrets(&self, workspace_id: &str, collection_id: &str) -> Result<()> {
        let path = api_path(&["v1", "applications", workspace_id, "secrets", collection_id])?;
        self.client.delete(path).send().await?;
        Ok(())
    }
}

/// Checks that no two of `secrets` have the same type and location: each
/// becomes its own operation of an update.
fn check_unique_keys(secrets: &[Secret]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for secret in secrets {
        let key = (wire_type(secret), secret.location.clone());
        if !seen.insert(key) {
            return Err(Error::Invalid {
                reason: format!(
                    "`secrets` has the {} secret `{}` more than once",
                    wire_type(secret),
                    secret.location
                ),
            });
        }
    }
    Ok(())
}

/// The type and location of each secret of `collection`, as the platform
/// writes them, so that a type this SDK does not know keeps its name.
fn existing_keys(collection: &Map<String, Value>) -> BTreeSet<(String, String)> {
    let secrets = collection.get("secrets").and_then(Value::as_array);
    let key = |secret: &Value| {
        let secret_type = secret.get("type")?.as_str()?.to_owned();
        let location = secret.get("location")?.as_str()?.to_owned();
        Some((secret_type, location))
    };
    secrets.into_iter().flatten().filter_map(key).collect()
}

/// The type of `secret` as the platform writes it, e.g. `env`.
fn wire_type(secret: &Secret) -> String {
    match json!(secret.secret_type) {
        Value::String(secret_type) => secret_type,
        other => other.to_string(),
    }
}

/// Checks the secrets that a create or an update sends: at least one, and
/// each with a value.
fn check_secrets(secrets: &[Secret]) -> Result<()> {
    if secrets.is_empty() {
        return Err(Error::Invalid {
            reason: "`secrets` needs at least one secret".to_owned(),
        });
    }
    if let Some(index) = secrets.iter().position(|secret| secret.value.is_none()) {
        return Err(Error::Invalid {
            reason: format!("`secrets[{index}]` has no `value`"),
        });
    }
    if let Some(index) = secrets
        .iter()
        .position(|secret| secret.secret_type == SecretType::Unknown)
    {
        return Err(Error::Invalid {
            reason: format!("`secrets[{index}].type` must be `env` or `file`"),
        });
    }
    Ok(())
}

pub(crate) fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "decision_platform_secrets_create",
            "Creates a secrets collection.",
            |cx, a: InWorkspace<SecretsCreateParams>| async move {
                cx.platform()?
                    .create_secrets(&a.workspace_id, a.params)
                    .await
            },
        ),
        // A client may auto-approve this read-only tool, so its result never
        // carries secret values, which would land in the model's context.
        // Its description says so, which makes it the one row whose
        // description is not the first line of its method's rustdoc.
        Tool::new(
            "decision_platform_secrets_get",
            "Gets a secrets collection, including its secrets but not their values.",
            |cx, a: SecretsRef<NoParams>| async move {
                cx.platform()?
                    .get_secrets(&a.workspace_id, &a.collection_id)
                    .await
                    .map(SecretsCollection::without_values)
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_secrets_list",
            "Lists the secrets collections of a workspace, without their secrets.",
            |cx, a: InWorkspace<NoParams>| async move {
                cx.platform()?.list_secrets(&a.workspace_id).await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_secrets_update",
            "Updates a secrets collection.",
            |cx, a: SecretsRef<SecretsUpdateParams>| async move {
                cx.platform()?
                    .update_secrets(&a.workspace_id, &a.collection_id, a.params)
                    .await
            },
        )
        .destructive(),
        Tool::new(
            "decision_platform_secrets_delete",
            "Deletes a secrets collection.",
            |cx, a: SecretsRef<NoParams>| async move {
                cx.platform()?
                    .delete_secrets(&a.workspace_id, &a.collection_id)
                    .await
            },
        )
        .destructive(),
    ]
}
