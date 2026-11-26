//! Auth: API keys and PKCE logins, the tokens of PKCE sessions, teams and
//! the SSO lookup.
//!
//! * **API keys** need nothing here: [`Resolved::client_builder`] sends
//!   them as a fixed bearer token.
//! * **PKCE** profiles share a login per session ([`sessions`]). [`login`]
//!   returns the URL for the caller to open in a browser and waits for the
//!   identity provider's callback on a loopback port; the tokens are stored
//!   in `<home>/auth/<session>/tokens.json`. A [`Client`] built by
//!   [`Resolved::client_builder`] asks for them before each request and
//!   refreshes them 30 seconds before they expire, so building a client
//!   makes no request. [`logout`] deletes them.
//! * [`list_teams`] and [`resolve_team`] find the team a PKCE profile acts
//!   for, and [`sso_provider`] the SSO provider of an email domain.
//! * [`login_profiles`] and [`logout_profiles`] log in and out the sessions
//!   of a profile, or of every PKCE profile, in one call each. Creating a
//!   profile with its login is [`config::create_profile`].
//! * [`ProfileProvider`] serves the MCP server from a profile.
//!
//! Nothing here prints: callers open the browser and word every message.
//! No error and no `Debug` output shows a token, a code, the PKCE verifier
//! or an API key.
//!
//! [`Resolved::client_builder`]: crate::config::Resolved::client_builder
//! [`Client`]: crate::Client
//! [`config::create_profile`]: crate::config::create_profile

mod base64;
mod oidc;
mod pkce;
mod profiles;
mod provider;
mod sso;
mod store;
mod teams;
mod token;

use std::path::Path;

use serde::Serialize;

pub use self::pkce::{
    BROWSER_TIMEOUT, DEFAULT_CALLBACK_PORT, LoggedIn, LoginOptions, PendingLogin, SCOPES, login,
};
pub(crate) use self::profiles::login_session;
pub use self::profiles::{SessionLogin, SessionLogout, login_profiles, logout_profiles};
pub use self::provider::ProfileProvider;
pub use self::sso::sso_provider;
pub use self::store::logout;
pub use self::teams::{Team, list_teams, resolve_team};
pub(crate) use self::token::SessionTokens;
use crate::config::{Config, Credential, EndpointOidc, Env, Profile};
use crate::{Error, ProfileProblem, Result};

/// A PKCE session and the profiles that use it, which log in once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Session {
    /// The session's name, in lowercase, e.g. `default`.
    pub session: String,
    /// The endpoint the session's tokens are for, normalized, e.g.
    /// `https://api.example.com`.
    pub endpoint: String,
    /// The PKCE profiles that use the session with this endpoint, the
    /// default profile first.
    pub profiles: Vec<String>,
    /// The OIDC settings of the endpoint. `None` when it has none, so the
    /// session can't log in.
    #[serde(skip)]
    pub oidc: Option<EndpointOidc>,
    /// The email domain whose SSO provider the login goes through: the
    /// first one that the session's profiles set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sso_domain: Option<String>,
}

/// The PKCE sessions of `config`, each with the profiles that use it, so
/// that each session logs in once.
///
/// With `profile`, only that profile's session, with just that profile.
/// Without one, the sessions of every PKCE profile: the default profile's
/// first, then the others by profile name. Profiles are grouped by session
/// and endpoint; a session that two endpoints share is an error, so their
/// tokens never overwrite each other.
///
/// A session whose endpoint has no OIDC settings is returned with
/// [`Session::oidc`] unset, so the caller can warn about its profiles and
/// still log them out.
///
/// # Errors
///
/// Returns [`Error::NotFound`] if `profile` does not exist,
/// [`Error::Profile`] with [`ProfileProblem::NotPkce`] if it is not a PKCE
/// profile, and [`Error::Invalid`] if a profile breaks the
/// rules of [`Config::set_profile`], a profile's name is blank or has
/// leading or trailing whitespace, which only a hand-edited `config.yaml`
/// can have, or a session is used with two endpoints.
pub fn sessions(config: &Config, profile: Option<&str>) -> Result<Vec<Session>> {
    let names: Vec<&str> = match profile.map(str::trim) {
        Some(name) => {
            let stored = config.profiles.get(name).ok_or_else(|| Error::NotFound {
                kind: "profile",
                id: name.to_owned(),
            })?;
            if !matches!(stored, Profile::Pkce { .. }) {
                return Err(Error::Profile {
                    profile: name.to_owned(),
                    problem: ProfileProblem::NotPkce {
                        auth_type: "api_key",
                    },
                });
            }
            vec![name]
        }
        None => {
            let default = config.default_profile.as_deref();
            let mut names: Vec<&str> = config
                .profiles
                .iter()
                .filter(|(_, profile)| matches!(profile, Profile::Pkce { .. }))
                .map(|(name, _)| name.as_str())
                .collect();
            // Stable, so the others keep their name order.
            names.sort_by_key(|name| Some(*name) != default);
            names
        }
    };

    let mut sessions: Vec<Session> = Vec::new();
    for name in names {
        // `resolve` trims a name and reads a blank one as unset, so it would
        // resolve another profile, or none, for a stored name that is blank
        // or untrimmed. Any other stored name resolves to itself.
        let misnamed = || Error::Invalid {
            reason: format!(
                "profile `{name}` has a blank or untrimmed name: rename it in `config.yaml`"
            ),
        };
        if name.trim() != name || name.is_empty() {
            return Err(misnamed());
        }
        let resolved = config
            .resolve(Some(name), &Env::default())?
            .ok_or_else(misnamed)?;
        let Credential::Session(session) = resolved.credential else {
            continue;
        };
        let sso_domain = match &config.profiles[name] {
            Profile::Pkce { sso_domain, .. } => sso_domain.clone(),
            _ => None,
        };
        match sessions.iter_mut().find(|other| other.session == session) {
            Some(other) if other.endpoint != resolved.endpoint => {
                return Err(Error::Invalid {
                    reason: format!(
                        "session `{session}` is used with two endpoints, `{}` and `{}`: give \
                         profile `{name}` another session",
                        other.endpoint, resolved.endpoint
                    ),
                });
            }
            Some(other) => {
                other.profiles.push(name.to_owned());
                other.sso_domain = other.sso_domain.take().or(sso_domain);
            }
            None => sessions.push(Session {
                session,
                endpoint: resolved.endpoint,
                profiles: vec![name.to_owned()],
                oidc: resolved.oidc,
                sso_domain,
            }),
        }
    }
    Ok(sessions)
}

/// The bearer token of `session` in the FICO home `home`, refreshed first
/// when it expires within 30 seconds, without a login. `configuration
/// create` calls it to reuse a session's tokens, and logs in when it
/// returns [`Error::NotLoggedIn`].
///
/// # Errors
///
/// Returns [`Error::NotLoggedIn`] if the session has no usable tokens for
/// its endpoint (an `invalid_grant` deletes them), [`Error::Invalid`] if the
/// endpoint is `http` but not on this machine, and [`Error::Login`],
/// [`Error::Io`] or the transport's errors if a refresh fails.
pub async fn ensure_token(home: &Path, session: &Session) -> Result<String> {
    SessionTokens::new(
        home,
        &session.session,
        &session.endpoint,
        session.oidc.clone(),
    )
    .bearer()
    .await
}
