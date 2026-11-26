//! The SSO lookup of an email domain, which a login goes through.

use std::time::Duration;

use serde::Deserialize;

use super::oidc;
use crate::{Client, Error, Result, RetryPolicy};

/// How long the lookup may take.
const SSO_TIMEOUT: Duration = Duration::from_secs(10);

/// The SSO provider of the email domain `domain` on `endpoint`: the
/// identity provider a login goes through, or `None` when the domain has no
/// SSO enabled. `GET v1/enterprise/sso/domain?domain=<domain>`, with no
/// token.
///
/// [`login`](super::login) calls it for a session's
/// [`sso_domain`](super::Session::sso_domain), and falls back to the
/// standard login when it fails.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if `endpoint` is `http` but not on this
/// machine, or is not a URL, and the transport's errors.
pub async fn sso_provider(endpoint: &str, domain: &str) -> Result<Option<String>> {
    #[derive(Deserialize)]
    struct Lookup {
        #[serde(default)]
        enabled: bool,
        domain_identifier: Option<String>,
    }
    oidc::checked_url(endpoint, "the endpoint").map_err(|_| Error::Invalid {
        reason: format!("endpoint `{endpoint}` is not `https`, so it is not asked for SSO"),
    })?;
    let client = Client::builder(endpoint)
        .retry(RetryPolicy::default().max_retries(2))
        .build()?;
    let lookup: Lookup = client
        .get("/v1/enterprise/sso/domain")
        .query(&[("domain", domain.trim())])
        .no_auth()
        .timeout(SSO_TIMEOUT)
        .send_json()
        .await?;
    Ok(lookup
        .domain_identifier
        .filter(|provider| lookup.enabled && !provider.is_empty()))
}
