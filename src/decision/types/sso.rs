//! SSO configuration types.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// SSO (SAML single sign-on) configuration of the account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct SsoConfiguration {
    /// Whether users whose email is not in the SSO domain may also sign in
    /// to the account.
    #[serde(default)]
    pub allow_non_domain_users: bool,
    /// Whether SSO is enabled for the account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// URL of the identity provider's SAML metadata document, when the
    /// configuration uses one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_url: Option<String>,
    /// The identity provider's SAML metadata document (XML), when the
    /// configuration holds it instead of a URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_document: Option<String>,
    /// Other email domains that sign in through the identity provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapped_domains: Option<Vec<String>>,
}
