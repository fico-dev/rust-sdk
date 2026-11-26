//! Accounts (organizations in the API), on `v1/organization`.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::Platform;
use crate::decision::Account;
use crate::decision::path::api_path;
use crate::tool::{NoParams, Tool};
use crate::{Error, Result};

/// Tool arguments that address an account, plus the operation's parameters.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct AccountRef<P> {
    /// ID of the account.
    pub(crate) account_id: String,
    #[serde(flatten)]
    pub(crate) params: P,
}

/// Parameters for [`Platform::create_account`].
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct AccountCreateParams {
    /// Name of the account.
    pub name: String,
    /// Emails of the account's administrators. Needs at least one.
    pub admins: Vec<String>,
}

/// Parameters for [`Platform::update_account`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct AccountUpdateParams {
    /// New name of the account. Must be set, since it is the only field an
    /// update changes.
    pub name: Option<String>,
}

impl Platform {
    /// Creates an account.
    ///
    /// The account is created under the current one, which needs an enabled
    /// SSO configuration.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `admins` is empty or has a blank email.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn create_account(&self, params: AccountCreateParams) -> Result<Account> {
        if params.admins.is_empty() {
            return Err(Error::Invalid {
                reason: "`admins` needs at least one email".to_owned(),
            });
        }
        if params.admins.iter().any(|admin| admin.trim().is_empty()) {
            return Err(Error::Invalid {
                reason: "`admins` has a blank email".to_owned(),
            });
        }
        let body = json!({ "name": params.name, "admins": params.admins });
        let path = api_path(&["v1", "organization"])?;
        self.client.post(path).json(&body).send_json().await
    }

    /// Gets an account, including its members.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Status`] with status 404 if the account does not exist,
    /// and the [errors of every method](Platform#errors).
    pub async fn get_account(&self, account_id: &str) -> Result<Account> {
        let path = api_path(&["v1", "organization", account_id])?;
        self.client.get(path).send_json().await
    }

    /// Updates an account.
    ///
    /// Every field but the name keeps its value, members included, and the
    /// whole account is returned.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `name` is unset or empty, since there is
    /// nothing to update.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn update_account(
        &self,
        account_id: &str,
        params: AccountUpdateParams,
    ) -> Result<Account> {
        let Some(name) = params.name.filter(|name| !name.is_empty()) else {
            return Err(Error::Invalid {
                reason: "nothing to update: set `name`".to_owned(),
            });
        };
        let path = api_path(&["v1", "organization", account_id])?;
        // The whole account goes back, members included, so fields this SDK
        // does not model keep their values too.
        let mut account: Map<String, Value> = self.client.get(&path).send_json().await?;
        account.insert("name".to_owned(), name.into());
        let response = self.client.put(path).json(&account).send().await?;
        // The platform answers with the changed fields only (the name), so
        // they go over the account that was sent.
        let changed: Map<String, Value> = response.json()?;
        account.extend(changed);
        serde_json::from_value(Value::Object(account)).map_err(|error| response.decode_error(error))
    }

    /// Deletes an account.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn delete_account(&self, account_id: &str) -> Result<()> {
        let path = api_path(&["v1", "organization", account_id])?;
        self.client.delete(path).send().await?;
        Ok(())
    }
}

pub(crate) fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "decision_platform_account_create",
            "Creates an account.",
            |cx, a: AccountCreateParams| async move {
                cx.platform()?.create_account(a).await
            },
        ),
        Tool::new(
            "decision_platform_account_get",
            "Gets an account, including its members.",
            |cx, a: AccountRef<NoParams>| async move {
                cx.platform()?.get_account(&a.account_id).await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_account_update",
            "Updates an account.",
            |cx, a: AccountRef<AccountUpdateParams>| async move {
                cx.platform()?.update_account(&a.account_id, a.params).await
            },
        )
        .destructive(),
        Tool::new(
            "decision_platform_account_delete",
            "Deletes an account.",
            |cx, a: AccountRef<NoParams>| async move {
                cx.platform()?.delete_account(&a.account_id).await
            },
        )
        .destructive(),
    ]
}
