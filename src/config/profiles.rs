//! Whole-call profile management over `config.yaml`: get, list and delete a
//! profile, with the token cleanup that goes with it.

use std::fmt;
use std::path::Path;

use serde::Serialize;

use super::{Config, EndpointOidc, Profile, ProfileSet, email_domain, profile_not_found};
use crate::auth::{self, LoginOptions, PendingLogin, Session, Team};
use crate::{Client, Error, ProfileProblem, Result};

/// A profile and its place in the configuration, as [`get_profile`] and
/// [`list_profiles`] return it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct NamedProfile {
    /// Name of the profile.
    pub name: String,
    /// The profile. An API key is masked (see [`mask_api_key`]).
    ///
    /// [`mask_api_key`]: super::mask_api_key
    #[serde(flatten)]
    pub profile: Profile,
    /// Whether it is the default profile.
    pub default: bool,
}

/// What [`delete_profile`] did.
#[derive(Debug)]
#[non_exhaustive]
pub struct ProfileRemoved {
    /// Name of the deleted profile, trimmed.
    pub name: String,
    /// The default profile after the deletion: unchanged, the `new_default`
    /// that was given, or `None` once no profile is left.
    pub new_default: Option<String>,
    /// The PKCE session whose tokens no other profile used, if the deleted
    /// profile had one, and what happened to its tokens.
    pub unused_session: Option<UnusedSession>,
}

/// A session that no profile uses anymore, and the result of deleting its
/// tokens. Deleting them can fail after `config.yaml` is saved, so the
/// failure is data and not an error of the call.
#[derive(Debug)]
#[non_exhaustive]
pub struct UnusedSession {
    /// The session's name, in lowercase.
    pub session: String,
    /// Whether it had tokens, or why they could not be deleted.
    pub deleted: Result<bool>,
}

/// The profile `name` of the FICO home `home`, with its API key masked.
///
/// # Errors
///
/// Returns [`Error::NotFound`] if there is no such profile, and the errors
/// of [`Config::load`]. A missing `config.yaml` is an empty configuration, so
/// every name is not found.
pub async fn get_profile(home: &Path, name: &str) -> Result<NamedProfile> {
    let config = Config::load(home).await?.masked();
    let name = name.trim();
    let profile = config
        .profiles
        .get(name)
        .cloned()
        .ok_or_else(|| profile_not_found(name))?;
    Ok(NamedProfile {
        default: config.default_profile.as_deref() == Some(name),
        name: name.to_owned(),
        profile,
    })
}

/// The profiles of the FICO home `home`, with their API keys masked: the
/// default profile first, then the others by name. Empty when there is no
/// `config.yaml` or no profile.
///
/// # Errors
///
/// Returns the errors of [`Config::load`].
pub async fn list_profiles(home: &Path) -> Result<Vec<NamedProfile>> {
    let config = Config::load(home).await?.masked();
    let default = config.default_profile.clone();
    let mut profiles: Vec<NamedProfile> = config
        .profiles
        .into_iter()
        .map(|(name, profile)| NamedProfile {
            default: default.as_deref() == Some(name.as_str()),
            name,
            profile,
        })
        .collect();
    // Stable, so the others keep their name order.
    profiles.sort_by_key(|profile| !profile.default);
    Ok(profiles)
}

/// Deletes the profile `name` from the FICO home `home`, and the tokens of
/// its PKCE session when no other profile uses that session. With
/// `new_default`, that other profile becomes the default.
///
/// The checks and the save are those of [`Config::delete_profile`] and
/// [`Config::save`]. The tokens are deleted once the file is saved, so a
/// failure to delete them does not undo the deletion: it is in
/// [`UnusedSession::deleted`].
///
/// # Errors
///
/// Returns [`Error::NotFound`] if `name` or `new_default` is not a profile,
/// [`Error::Invalid`] if `new_default` is `name`, [`Error::Profile`] with
/// [`ProfileProblem::NewDefaultNeeded`](crate::ProfileProblem) if `name` is
/// the default profile and others exist but `new_default` is `None`, and the
/// errors of [`Config::load`] and [`Config::save`]. Nothing changes then.
pub async fn delete_profile(
    home: &Path,
    name: &str,
    new_default: Option<&str>,
) -> Result<ProfileRemoved> {
    let mut config = Config::load(home).await?;
    let deleted = config.delete_profile(name, new_default)?;
    config.save(home).await?;
    let unused_session = match deleted.unused_session {
        Some(session) => Some(UnusedSession {
            deleted: auth::logout(home, &session).await,
            session,
        }),
        None => None,
    };
    Ok(ProfileRemoved {
        name: name.trim().to_owned(),
        new_default: deleted.new_default,
        unused_session,
    })
}

/// What [`create_profile`] creates or updates a profile from.
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub enum ProfileParams {
    /// A profile that authenticates with an API key.
    ApiKey {
        /// The platform endpoint, e.g. `api.example.com`; see
        /// [`Config::set_profile`].
        endpoint: Option<String>,
        /// The API key.
        api_key: String,
    },
    /// A profile that logs in with the browser.
    Pkce(PkceParams),
}

// Written by hand so the API key never reaches a log, as `Profile` does.
impl fmt::Debug for ProfileParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProfileParams::ApiKey { endpoint, .. } => f
                .debug_struct("ApiKey")
                .field("endpoint", endpoint)
                .field("api_key", &"<redacted>")
                .finish(),
            ProfileParams::Pkce(params) => f.debug_tuple("Pkce").field(params).finish(),
        }
    }
}

/// The settings of a PKCE profile for [`create_profile`].
///
/// ```
/// use fico_sdk::config::PkceParams;
///
/// let params = PkceParams {
///     endpoint: Some("api.example.com".to_owned()),
///     auth_session: Some("work".to_owned()),
///     team: Some("Hare".to_owned()),
///     ..PkceParams::default()
/// };
/// # let _ = params;
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PkceParams {
    /// The platform endpoint, e.g. `api.example.com`; see
    /// [`Config::set_profile`].
    pub endpoint: Option<String>,
    /// The session whose login the profile shares; see
    /// [`Config::set_profile`].
    pub auth_session: Option<String>,
    /// The user's email, or just its domain, to find the SSO provider the
    /// login goes through. Only the domain is stored.
    pub sso_email: Option<String>,
    /// The URL of the identity provider's OIDC discovery document. Replaces
    /// the stored one of the endpoint, and is required when the endpoint has
    /// none.
    pub oidc_discovery_url: Option<String>,
    /// The OAuth2 client ID at the identity provider. Replaces the stored
    /// one of the endpoint, and is required when the endpoint has none.
    pub client_id: Option<String>,
    /// The name of the team the profile acts for, in any case. Without it,
    /// the call fails with [`ProfileProblem::Team`] after the login, which
    /// lists the teams to choose from.
    pub team: Option<String>,
}

/// The PKCE session that [`create_profile`] used.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SessionUsed {
    /// The session's name, in lowercase.
    pub session: String,
    /// The endpoint its tokens are for.
    pub endpoint: String,
    /// Whether the call logged in. `false` means that the session had valid
    /// tokens, refreshed if need be.
    pub logged_in: bool,
}

/// What [`create_profile`] did.
#[derive(Debug)]
#[non_exhaustive]
pub struct ProfileCreated {
    /// The profile as saved, with its API key masked.
    pub profile: NamedProfile,
    /// Whether the OIDC settings of the endpoint replaced different ones,
    /// stored or built in, so that the host can warn about it.
    pub oidc_overridden: bool,
    /// The session of a PKCE profile.
    pub session: Option<SessionUsed>,
    /// The team of a PKCE profile.
    pub team: Option<Team>,
    /// The profile's previous session, if no profile uses it anymore, and
    /// what happened to its tokens.
    pub stale_session: Option<UnusedSession>,
}

/// Creates the profile `name`, or replaces it, in the FICO home `home`,
/// whatever its type, and saves it.
///
/// An API-key profile is checked ([`Config::set_profile`]) and saved.
///
/// A PKCE profile goes through these steps, all before `config.yaml` is
/// written, so a failure leaves it as it was:
///
/// 1. The profile, with the session and the endpoint, is checked.
/// 2. The endpoint's OIDC settings are those given over the stored ones (the
///    stored `callback_port` stays), or the given ones when none are stored.
/// 3. The session's tokens are used, refreshed if need be; without usable
///    ones, the session logs in: `on_login` gets the [`PendingLogin`] to show
///    or open its authorization URL, and the call waits for the browser.
/// 4. The user's teams are listed, and `params.team` is looked up.
///
/// Then the OIDC settings and the profile are saved in one write, and the
/// tokens of a session that the change leaves unused are deleted.
///
/// A host without a team asks the user and calls again with the team: the
/// tokens are kept, so the second call does not log in.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if the name, the endpoint, the API key or the
/// session break the rules of [`Config::set_profile`], and
/// [`Error::Profile`] with
/// [`NoEndpoint`](ProfileProblem::NoEndpoint) or
/// [`NoSession`](ProfileProblem::NoSession) for the two they type,
/// [`NoOidc`](ProfileProblem::NoOidc) if the endpoint has no OIDC settings
/// and `params` lacks the discovery URL or the client ID, and
/// [`Team`](ProfileProblem::Team) if the user has no team, `params.team` is
/// unset, or no team has that name. It also returns the errors of
/// [`Config::load`], [`Config::save`], the login ([`Error::Login`],
/// [`Error::Wait`], the transport's errors) and of listing the teams.
pub async fn create_profile(
    home: &Path,
    name: &str,
    params: ProfileParams,
    make_default: bool,
    mut on_login: impl FnMut(&PendingLogin),
) -> Result<ProfileCreated> {
    match params {
        ProfileParams::ApiKey { endpoint, api_key } => {
            let profile = Profile::ApiKey { endpoint, api_key };
            let mut config = Config::load(home).await?;
            let set = config.set_profile(name, profile, make_default)?;
            config.save(home).await?;
            let stale_session = forget(home, set.stale_session.clone()).await;
            Ok(ProfileCreated {
                profile: named(&config, &set),
                oidc_overridden: false,
                session: None,
                team: None,
                stale_session,
            })
        }
        ProfileParams::Pkce(params) => {
            create_pkce_profile(home, name, params, make_default, &mut on_login).await
        }
    }
}

/// The PKCE case of [`create_profile`].
async fn create_pkce_profile(
    home: &Path,
    name: &str,
    params: PkceParams,
    make_default: bool,
    on_login: &mut impl FnMut(&PendingLogin),
) -> Result<ProfileCreated> {
    let profile = |team_id: Option<String>| Profile::Pkce {
        endpoint: params.endpoint.clone(),
        auth_session: params.auth_session.clone(),
        team_id,
        sso_domain: params.sso_email.as_deref().and_then(email_domain),
    };

    // A dry run, before any login or request: the profile's rules, and the
    // session, which another endpoint may use already.
    let mut trial = Config::load(home).await?;
    let name = trial.set_profile(name, profile(None), make_default)?.name;
    let mut session = auth::sessions(&trial, Some(&name))?
        .pop()
        .ok_or_else(|| profile_not_found(&name))?;
    let oidc = merged_oidc(&trial, &session, &params, &name)?;
    let mut oidc_overridden = false;
    if let Some(oidc) = &oidc {
        oidc_overridden = trial.set_endpoint_oidc(&session.endpoint, oidc.clone())?;
        session.oidc = trial.endpoint_oidc(&session.endpoint);
    }

    // The session's tokens, refreshed if need be, else a login.
    let mut logged_in = false;
    let token = match auth::ensure_token(home, &session).await {
        Ok(token) => token,
        Err(Error::NotLoggedIn { .. } | Error::Login { .. }) => {
            auth::login_session(home, &session, LoginOptions::new(), on_login).await?;
            logged_in = true;
            auth::ensure_token(home, &session).await?
        }
        Err(error) => return Err(error),
    };
    let client = Client::builder(&session.endpoint)
        .bearer_token(token)
        .build()?;
    let team = pick_team(
        auth::list_teams(&client).await?,
        params.team.as_deref(),
        &session.endpoint,
        &name,
    )?;

    // Read again: the login may have taken long, and the file may have
    // changed meanwhile.
    let mut config = Config::load(home).await?;
    if let Some(oidc) = oidc {
        config.set_endpoint_oidc(&session.endpoint, oidc)?;
    }
    let set = config.set_profile(&name, profile(Some(team.team_id.clone())), make_default)?;
    config.save(home).await?;
    // The previous session, unless the login just stored its tokens again,
    // for the new endpoint.
    let stale = set
        .stale_session
        .clone()
        .filter(|stale| *stale != session.session);
    Ok(ProfileCreated {
        profile: named(&config, &set),
        oidc_overridden,
        session: Some(SessionUsed {
            session: session.session,
            endpoint: session.endpoint,
            logged_in,
        }),
        team: Some(team),
        stale_session: forget(home, stale).await,
    })
}

/// The OIDC settings to store for the endpoint of `session`: those in
/// `params` over the stored ones, which keep their `callback_port`. `None`
/// when the stored ones stay.
fn merged_oidc(
    config: &Config,
    session: &Session,
    params: &PkceParams,
    name: &str,
) -> Result<Option<EndpointOidc>> {
    let given = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let (url, client_id) = (given(&params.oidc_discovery_url), given(&params.client_id));
    match config.endpoint_oidc(&session.endpoint) {
        Some(_) if url.is_none() && client_id.is_none() => Ok(None),
        Some(stored) => Ok(Some(EndpointOidc {
            oidc_discovery_url: url.unwrap_or(stored.oidc_discovery_url),
            client_id: client_id.unwrap_or(stored.client_id),
            callback_port: stored.callback_port,
        })),
        None => match (url, client_id) {
            (Some(oidc_discovery_url), Some(client_id)) => Ok(Some(EndpointOidc {
                oidc_discovery_url,
                client_id,
                callback_port: None,
            })),
            _ => Err(Error::Profile {
                profile: name.to_owned(),
                problem: ProfileProblem::NoOidc {
                    endpoint: session.endpoint.clone(),
                },
            }),
        },
    }
}

/// The team named `wanted` among `teams`, ignoring case.
fn pick_team(
    mut teams: Vec<Team>,
    wanted: Option<&str>,
    endpoint: &str,
    profile: &str,
) -> Result<Team> {
    teams.sort_by(|a, b| a.name.cmp(&b.name));
    let wanted = wanted.map(str::trim).filter(|wanted| !wanted.is_empty());
    let found = wanted.and_then(|wanted| {
        let lower = wanted.to_lowercase();
        teams
            .iter()
            .position(|team| team.name.to_lowercase() == lower)
    });
    match found {
        Some(index) => Ok(teams.swap_remove(index)),
        None => Err(Error::Profile {
            profile: profile.to_owned(),
            problem: ProfileProblem::Team {
                endpoint: endpoint.to_owned(),
                team: wanted.map(str::to_owned),
                teams: teams.into_iter().map(|team| team.name).collect(),
            },
        }),
    }
}

/// The saved profile `set` names in `config`, masked.
fn named(config: &Config, set: &ProfileSet) -> NamedProfile {
    let profile = config
        .masked()
        .profiles
        .remove(&set.name)
        .expect("`set_profile` stored the profile");
    NamedProfile {
        default: config.default_profile.as_deref() == Some(set.name.as_str()),
        name: set.name.clone(),
        profile,
    }
}

/// Deletes the tokens of `session`, if there is one.
async fn forget(home: &Path, session: Option<String>) -> Option<UnusedSession> {
    let session = session?;
    Some(UnusedSession {
        deleted: auth::logout(home, &session).await,
        session,
    })
}
