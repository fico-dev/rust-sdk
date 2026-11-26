//! SSO configuration of the current account, on `v1/enterprise/sso`.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::Platform;
use crate::decision::SsoConfiguration;
use crate::decision::path::api_path;
use crate::tool::{NoParams, Tool};
use crate::{Error, Result};

/// Tool arguments that address a domain of the SSO configuration.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct SsoDomainRef {
    /// The domain, e.g. `example.com`.
    pub(crate) domain: String,
}

/// Parameters for [`Platform::create_sso`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SsoCreateParams {
    /// Whether users whose email is not in the SSO domain may also sign in
    /// to the account. Defaults to `false`.
    pub allow_non_domain_users: bool,
    /// Whether to enable SSO right away. Defaults to `false`.
    pub enabled: bool,
    /// URL of the identity provider's SAML metadata document. Set this or
    /// `metadata_document`, not both.
    pub metadata_url: Option<String>,
    /// The identity provider's SAML metadata document, as XML content, not a
    /// path. Set this or `metadata_url`, not both.
    pub metadata_document: Option<String>,
}

/// Parameters for [`Platform::update_sso`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SsoUpdateParams {
    /// URL of the identity provider's new SAML metadata document. Set this
    /// or `metadata_document`, not both. Unset keeps the current source.
    pub metadata_url: Option<String>,
    /// The identity provider's new SAML metadata document, as XML content,
    /// not a path. Set this or `metadata_url`, not both. Unset keeps the
    /// current source.
    pub metadata_document: Option<String>,
    /// Whether SSO is enabled for the account. Unset keeps it as it is.
    pub enabled: Option<bool>,
    /// Whether users whose email is not in the SSO domain may also sign in
    /// to the account. Unset keeps it as it is.
    pub allow_non_domain_users: Option<bool>,
}

/// The API keys of the two metadata sources, of which a configuration
/// holds exactly one.
const METADATA_KEYS: [&str; 2] = ["metadata_url", "metadata_document"];

/// The one metadata source that `url` and `document` set, as its API key and
/// value. Empty strings count as unset.
fn metadata_source(
    url: Option<String>,
    document: Option<String>,
) -> Result<(&'static str, String)> {
    let url = url.filter(|url| !url.is_empty());
    let document = document.filter(|document| !document.is_empty());
    match (url, document) {
        (Some(url), None) => Ok((METADATA_KEYS[0], url)),
        (None, Some(document)) => Ok((METADATA_KEYS[1], document)),
        (None, None) => Err(Error::Invalid {
            reason: "set `metadata_url` or `metadata_document`".to_owned(),
        }),
        (Some(_), Some(_)) => Err(Error::Invalid {
            reason: "set only one of `metadata_url` and `metadata_document`".to_owned(),
        }),
    }
}

impl Platform {
    /// Creates the SSO configuration of the account.
    ///
    /// The SAML metadata document is passed as content: the SDK never reads it
    /// from a path. The SSO domain is the domain of the caller's email.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] unless exactly one of `metadata_url` and
    /// `metadata_document` is set and not empty.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn create_sso(&self, params: SsoCreateParams) -> Result<()> {
        let (key, value) = metadata_source(params.metadata_url, params.metadata_document)?;
        let mut body = json!({
            "allow_non_domain_users": params.allow_non_domain_users,
            "enabled": params.enabled,
        });
        body[key] = value.into();
        let path = api_path(&["v1", "enterprise", "sso"])?;
        self.client.post(path).json(&body).send().await?;
        Ok(())
    }

    /// Gets the SSO configuration of the account.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn get_sso(&self) -> Result<SsoConfiguration> {
        let path = api_path(&["v1", "enterprise", "sso"])?;
        self.client.get(path).send_json().await
    }

    /// Enables SSO for the account.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn enable_sso(&self) -> Result<()> {
        let path = api_path(&["v1", "enterprise", "sso", "enable"])?;
        self.client.put(path).send().await?;
        Ok(())
    }

    /// Disables SSO for the account.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn disable_sso(&self) -> Result<()> {
        let path = api_path(&["v1", "enterprise", "sso", "disable"])?;
        self.client.put(path).send().await?;
        Ok(())
    }

    /// Updates the SSO configuration of the account.
    ///
    /// Only the fields that `params` sets change. A new metadata source
    /// replaces the old one, so a configuration can switch between a URL and
    /// a document.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`], before any request, if `params` sets no
    /// field, or sets both `metadata_url` and `metadata_document`. Empty
    /// strings count as unset.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn update_sso(&self, params: SsoUpdateParams) -> Result<()> {
        let SsoUpdateParams {
            metadata_url,
            metadata_document,
            enabled,
            allow_non_domain_users,
        } = params;
        let url = metadata_url.filter(|url| !url.is_empty());
        let document = metadata_document.filter(|document| !document.is_empty());
        let source = match (url.is_some(), document.is_some()) {
            (false, false) => None,
            _ => Some(metadata_source(url, document)?),
        };
        if source.is_none() && enabled.is_none() && allow_non_domain_users.is_none() {
            return Err(Error::Invalid {
                reason: "nothing to update: set `metadata_url`, `metadata_document`, `enabled` \
                         or `allow_non_domain_users`"
                    .to_owned(),
            });
        }
        let path = api_path(&["v1", "enterprise", "sso"])?;
        // The whole configuration goes back, so fields this SDK does not
        // model keep their values too, and so does the current metadata
        // source when no new one is set.
        let mut config: Map<String, Value> = self.client.get(&path).send_json().await?;
        if let Some((key, value)) = source {
            // The platform rejects a configuration with both sources, so the
            // old one goes.
            for old in METADATA_KEYS {
                config.remove(old);
            }
            config.insert(key.to_owned(), value.into());
        }
        if let Some(enabled) = enabled {
            config.insert("enabled".to_owned(), enabled.into());
        }
        if let Some(allow) = allow_non_domain_users {
            config.insert("allow_non_domain_users".to_owned(), allow.into());
        }
        self.client.put(path).json(&config).send().await?;
        Ok(())
    }

    /// Deletes the SSO configuration of the account.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn delete_sso(&self) -> Result<()> {
        let path = api_path(&["v1", "enterprise", "sso"])?;
        self.client.delete(path).send().await?;
        Ok(())
    }

    /// Removes a domain from the SSO configuration of the account.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn delete_sso_domain(&self, domain: &str) -> Result<()> {
        let path = api_path(&["v1", "enterprise", "sso", "domains", domain])?;
        self.client.delete(path).send().await?;
        Ok(())
    }
}

pub(crate) fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "decision_platform_sso_create",
            "Creates the SSO configuration of the account.",
            |cx, a: SsoCreateParams| async move { cx.platform()?.create_sso(a).await },
        ),
        Tool::new(
            "decision_platform_sso_get",
            "Gets the SSO configuration of the account.",
            |cx, _: NoParams| async move { cx.platform()?.get_sso().await },
        )
        .read_only(),
        Tool::new(
            "decision_platform_sso_enable",
            "Enables SSO for the account.",
            |cx, _: NoParams| async move { cx.platform()?.enable_sso().await },
        )
        .destructive(),
        Tool::new(
            "decision_platform_sso_disable",
            "Disables SSO for the account.",
            |cx, _: NoParams| async move { cx.platform()?.disable_sso().await },
        )
        .destructive(),
        Tool::new(
            "decision_platform_sso_update",
            "Updates the SSO configuration of the account.",
            |cx, a: SsoUpdateParams| async move { cx.platform()?.update_sso(a).await },
        )
        .destructive(),
        Tool::new(
            "decision_platform_sso_delete",
            "Deletes the SSO configuration of the account.",
            |cx, _: NoParams| async move { cx.platform()?.delete_sso().await },
        )
        .destructive(),
        Tool::new(
            "decision_platform_sso_domain_delete",
            "Removes a domain from the SSO configuration of the account.",
            |cx, a: SsoDomainRef| async move { cx.platform()?.delete_sso_domain(&a.domain).await },
        )
        .destructive(),
    ]
}
