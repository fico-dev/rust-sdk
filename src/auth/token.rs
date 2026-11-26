//! The tokens of a PKCE session in use: the bearer, its expiry and the
//! refresh, which [`SessionTokens`] does for a [`Client`](crate::Client)
//! before each request.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;

use super::base64;
use super::oidc::{self, Endpoints};
use super::store::{self, Tokens};
use crate::config::{EndpointOidc, normalize_endpoint};
use crate::http::TokenSource;
use crate::time::{now_unix, parse_unix};
use crate::tool::BoxFuture;
use crate::{Error, Result};

/// Tokens are refreshed when they expire within this many seconds.
const REFRESH_MARGIN: i64 = 30;

/// The token that a session sends as its bearer: the ID token. The
/// platform decides which one it reads.
pub(crate) fn bearer(tokens: &Tokens) -> Option<&str> {
    tokens.id_token.as_deref()
}

/// Whether `tokens` expire within [`REFRESH_MARGIN`], so they are
/// refreshed before use. An unreadable `expires_at` counts as expired.
fn needs_refresh(tokens: &Tokens) -> bool {
    expiry(tokens).saturating_sub(now_unix()) < REFRESH_MARGIN
}

/// When the bearer of `tokens` stops being usable, in Unix seconds: the
/// earlier of the access token's expiry and the ID token's `exp`.
/// `i64::MIN` when `expires_at` is unreadable.
fn expiry(tokens: &Tokens) -> i64 {
    let access = parse_unix(&tokens.expires_at).unwrap_or(i64::MIN);
    match bearer(tokens).and_then(id_token_exp) {
        Some(exp) => access.min(exp),
        None => access,
    }
}

/// The `exp` claim of the JWT `token`, if it has one. Its other claims are
/// checked where the token is issued and used ([`oidc::check_id_token`]).
fn id_token_exp(token: &str) -> Option<i64> {
    let payload = base64::decode_url(token.split('.').nth(1)?)?;
    let claims: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    Some(claims.get("exp")?.as_f64()? as i64)
}

/// The token source of a PKCE session: it reads the session's tokens from
/// the FICO home, refreshes them when they expire within 30 seconds, and
/// gives the bearer to the [`Client`](crate::Client) before each request.
///
/// Clones share one cache and one lock, so a refresh happens once. Another
/// process may refresh the same tokens: the file is read again before a
/// refresh, and the last one to save wins.
#[derive(Clone)]
pub(crate) struct SessionTokens {
    shared: Arc<Shared>,
}

struct Shared {
    home: PathBuf,
    session: String,
    endpoint: String,
    oidc: Option<EndpointOidc>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    tokens: Option<Tokens>,
    endpoints: Option<Endpoints>,
    /// The bearer of stored tokens that can't be refreshed, so later
    /// requests fail without asking the identity provider again. A new
    /// login stores another bearer, which clears it.
    dead: Option<String>,
}

impl SessionTokens {
    /// The token source of `session`, whose tokens are for `endpoint`, in
    /// the FICO home `home`. `oidc` are the endpoint's OIDC settings, which
    /// a refresh needs. Does no I/O.
    pub(crate) fn new(
        home: &Path,
        session: &str,
        endpoint: &str,
        oidc: Option<EndpointOidc>,
    ) -> SessionTokens {
        let endpoint = normalize_endpoint(endpoint).unwrap_or_else(|_| endpoint.to_owned());
        SessionTokens {
            shared: Arc::new(Shared {
                home: home.to_owned(),
                session: session.trim().to_ascii_lowercase(),
                endpoint,
                oidc,
                state: Mutex::new(State::default()),
            }),
        }
    }

    /// The session's bearer token, refreshed first when it expires within
    /// 30 seconds.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotLoggedIn`] if the session has no tokens, has
    /// tokens for another endpoint or without a bearer, or can't be
    /// refreshed because it has no refresh token or the identity provider
    /// rejected it (`invalid_grant`, which deletes the tokens). Returns
    /// [`Error::Invalid`] if the endpoint is `http` but not on this
    /// machine, or a refresh is needed and the endpoint has no OIDC
    /// settings, [`Error::Login`] if the tokens were issued for another
    /// client or the refreshed ones fail their checks, and [`Error::Io`] if
    /// the token file can't be read or written. A bearer that expired, also
    /// after a refresh that kept it, is [`Error::NotLoggedIn`].
    pub(crate) async fn bearer(&self) -> Result<String> {
        let shared = &self.shared;
        // Tokens go only over `https`, or `http` on this machine.
        oidc::checked_url(&shared.endpoint, "the endpoint of a PKCE session").map_err(|_| {
            Error::Invalid {
                reason: format!(
                    "endpoint `{}` is not `https`, so session `{}` can't send its tokens there",
                    shared.endpoint, shared.session
                ),
            }
        })?;
        let mut state = shared.state.lock().await;
        if let Some(tokens) = &state.tokens
            && !needs_refresh(tokens)
        {
            return self.checked_bearer(tokens);
        }
        // Another client or process may have logged in or refreshed.
        let tokens = self.load().await?;
        if state.dead.is_some() && state.dead.as_deref() == bearer(&tokens) {
            return Err(self.not_logged_in());
        }
        state.dead = None;
        let tokens = match needs_refresh(&tokens) {
            true => self.refresh(&mut state, tokens).await?,
            false => tokens,
        };
        let bearer = self.checked_bearer(&tokens)?;
        state.tokens = Some(tokens);
        Ok(bearer)
    }

    /// The session's stored tokens, if they are for this endpoint.
    async fn load(&self) -> Result<Tokens> {
        let shared = &self.shared;
        match store::load(&shared.home, &shared.session).await? {
            Some(tokens) if tokens.endpoint == shared.endpoint => Ok(tokens),
            _ => Err(self.not_logged_in()),
        }
    }

    /// The bearer of `tokens`, after these checks: it exists, has not
    /// expired, and was issued for the endpoint's client. A stored bearer
    /// that is missing or has expired needs a new login
    /// ([`Error::NotLoggedIn`]); one for another client fails as the login
    /// would have ([`Error::Login`]).
    fn checked_bearer(&self, tokens: &Tokens) -> Result<String> {
        let bearer = bearer(tokens).ok_or_else(|| self.not_logged_in())?;
        if id_token_exp(bearer).is_none_or(|exp| exp < now_unix()) {
            return Err(self.not_logged_in());
        }
        if let Some(oidc) = &self.shared.oidc {
            oidc::check_id_token(bearer, &oidc.client_id)?;
        }
        Ok(bearer.to_owned())
    }

    /// Refreshes `tokens` at the identity provider's token endpoint and
    /// stores the new ones. The refresh and ID tokens are kept when the
    /// response leaves them out, as Cognito's does.
    async fn refresh(&self, state: &mut State, tokens: Tokens) -> Result<Tokens> {
        let shared = &self.shared;
        let Some(refresh_token) = tokens.refresh_token.clone() else {
            return Err(self.not_logged_in());
        };
        let oidc = shared.oidc.as_ref().ok_or_else(|| Error::Invalid {
            reason: format!(
                "endpoint `{}` has no OIDC settings, so session `{}` can't be refreshed",
                shared.endpoint, shared.session
            ),
        })?;
        let endpoints = match &state.endpoints {
            Some(endpoints) => endpoints.clone(),
            None => {
                let endpoints = oidc::discover(oidc).await?;
                state.endpoints = Some(endpoints.clone());
                endpoints
            }
        };
        let form = [
            ("grant_type", "refresh_token"),
            ("client_id", oidc.client_id.as_str()),
            ("refresh_token", refresh_token.as_str()),
        ];
        let issued =
            oidc::request_tokens(&endpoints.token, &form, &oidc.client_id, &shared.endpoint).await;
        let mut refreshed = match issued {
            Ok(refreshed) => refreshed,
            Err(error) if oidc::is_invalid_grant(&error) => {
                store::delete(&shared.home, &shared.session).await?;
                state.tokens = None;
                return Err(self.not_logged_in());
            }
            Err(error) => return Err(error),
        };
        if refreshed.refresh_token.is_none() {
            refreshed.refresh_token = Some(refresh_token);
        }
        if refreshed.id_token.is_none() {
            // The bearer is the ID token: a kept one that has expired can't
            // be refreshed this way, so the session needs a new login,
            // instead of a refresh on every request.
            let kept_exp = tokens.id_token.as_deref().and_then(id_token_exp);
            if kept_exp.is_some_and(|exp| exp.saturating_sub(now_unix()) < REFRESH_MARGIN) {
                state.dead = tokens.id_token;
                return Err(self.not_logged_in());
            }
            refreshed.id_token = tokens.id_token;
        }
        store::save(&shared.home, &shared.session, &refreshed).await?;
        Ok(refreshed)
    }

    fn not_logged_in(&self) -> Error {
        Error::NotLoggedIn {
            session: self.shared.session.clone(),
        }
    }
}

impl TokenSource for SessionTokens {
    fn token(&self) -> BoxFuture<'_, Result<String>> {
        Box::pin(self.bearer())
    }
}

impl fmt::Debug for SessionTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionTokens")
            .field("session", &self.shared.session)
            .field("endpoint", &self.shared.endpoint)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(expires_at: &str, id_token: Option<String>) -> Tokens {
        Tokens {
            endpoint: "https://api".to_owned(),
            token_type: "Bearer".to_owned(),
            access_token: "a".to_owned(),
            id_token,
            refresh_token: None,
            expires_at: expires_at.to_owned(),
        }
    }

    fn jwt(exp: f64) -> String {
        let claims = serde_json::json!({ "exp": exp }).to_string();
        format!("h.{}.s", base64::encode_url(claims.as_bytes()))
    }

    #[test]
    fn refreshes_tokens_that_expire_within_the_margin() {
        let now = now_unix();
        assert!(!needs_refresh(&tokens(
            &crate::time::unix_rfc3339(now + 600),
            None
        )));
        assert!(needs_refresh(&tokens(
            &crate::time::unix_rfc3339(now + 10),
            None
        )));
        assert!(needs_refresh(&tokens(
            &crate::time::unix_rfc3339(now - 600),
            None
        )));
        // The ID token's `exp` counts too.
        let later = crate::time::unix_rfc3339(now + 600);
        assert!(needs_refresh(&tokens(&later, Some(jwt((now + 10) as f64)))));
        assert!(!needs_refresh(&tokens(
            &later,
            Some(jwt((now + 900) as f64))
        )));
    }

    #[test]
    fn refreshes_tokens_whose_expiry_is_unreadable() {
        // Without saturating math, these overflowed (and panicked in debug).
        assert!(needs_refresh(&tokens("2026-10-20 12:00:00", None)));
        assert!(needs_refresh(&tokens("", None)));
        let later = crate::time::unix_rfc3339(now_unix() + 600);
        assert!(needs_refresh(&tokens(&later, Some(jwt(-1e300)))));
    }
}
