//! Whole-call login and logout over profiles, and the login step that
//! `config::create_profile` shares.

use std::path::Path;

use super::{LoggedIn, LoginOptions, PendingLogin, Session, login, logout, sessions};
use crate::config::Config;
use crate::{Error, Result};

/// Logs in `session`: starts the login, hands it to `on_login`, which shows
/// or opens its authorization URL, then waits for the callback.
///
/// # Errors
///
/// The errors of [`login`] and of [`PendingLogin::complete`].
pub(crate) async fn login_session(
    home: &Path,
    session: &Session,
    options: LoginOptions,
    on_login: &mut impl FnMut(&PendingLogin),
) -> Result<LoggedIn> {
    let pending = login(home, session, options).await?;
    on_login(&pending);
    pending.complete().await
}

/// What [`login_profiles`] did for one session.
#[derive(Debug)]
#[non_exhaustive]
pub enum SessionLogin {
    /// The session logged in.
    LoggedIn(LoggedIn),
    /// The session's login failed. The other sessions went on.
    #[non_exhaustive]
    Failed {
        /// The session, with the profiles that use it.
        session: Session,
        /// Why it failed.
        error: Error,
    },
    /// The session was not tried, because its endpoint has no OIDC settings.
    /// Creating a profile for the endpoint stores them.
    #[non_exhaustive]
    Skipped {
        /// The session, with the profiles that use it.
        session: Session,
    },
}

/// What [`logout_profiles`] did for one session.
#[derive(Debug)]
#[non_exhaustive]
pub enum SessionLogout {
    /// The session's tokens are deleted.
    #[non_exhaustive]
    LoggedOut {
        /// The session, with the profiles that use it.
        session: Session,
        /// Whether it had tokens.
        had_tokens: bool,
    },
    /// The tokens could not be deleted. The other sessions went on.
    #[non_exhaustive]
    Failed {
        /// The session, with the profiles that use it.
        session: Session,
        /// Why it failed.
        error: Error,
    },
}

/// Logs in the PKCE session of the profile `profile`, or the sessions of
/// every PKCE profile, in the FICO home `home`. Profiles that share a
/// session log in once.
///
/// The sessions are those of [`sessions`]: the default profile's first.
/// Each one logs in on its own, one after the other, and `on_login` gets
/// each [`PendingLogin`] to show or open its authorization URL. A session
/// whose endpoint has no OIDC settings is [`SessionLogin::Skipped`], and one
/// that fails is [`SessionLogin::Failed`]; neither stops the others. The
/// result has one entry per session, in that order, and is empty when there
/// is no PKCE profile.
///
/// # Errors
///
/// Returns [`Error::NotFound`] if `profile` does not exist,
/// [`Error::Profile`] with [`ProfileProblem::NotPkce`] if it is not a PKCE
/// profile, [`Error::Invalid`] for the other rules of [`sessions`], and the
/// errors of [`Config::load`].
///
/// [`ProfileProblem::NotPkce`]: crate::ProfileProblem::NotPkce
pub async fn login_profiles(
    home: &Path,
    profile: Option<&str>,
    options: LoginOptions,
    mut on_login: impl FnMut(&PendingLogin),
) -> Result<Vec<SessionLogin>> {
    let config = Config::load(home).await?;
    let mut results = Vec::new();
    for session in sessions(&config, profile)? {
        if session.oidc.is_none() {
            results.push(SessionLogin::Skipped { session });
            continue;
        }
        match login_session(home, &session, options.clone(), &mut on_login).await {
            Ok(logged_in) => results.push(SessionLogin::LoggedIn(logged_in)),
            Err(error) => results.push(SessionLogin::Failed { session, error }),
        }
    }
    Ok(results)
}

/// Logs out the PKCE session of the profile `profile`, or the sessions of
/// every PKCE profile, in the FICO home `home`: deletes their tokens
/// ([`logout`]). Profiles that share a session are logged out together.
///
/// The result has one entry per session, sorted by session name, and is
/// empty when there is no PKCE profile. A session whose tokens can't be
/// deleted is [`SessionLogout::Failed`] and does not stop the others.
///
/// # Errors
///
/// The same as [`login_profiles`].
pub async fn logout_profiles(home: &Path, profile: Option<&str>) -> Result<Vec<SessionLogout>> {
    let config = Config::load(home).await?;
    let mut sessions = sessions(&config, profile)?;
    // A session belongs to one endpoint, so each name comes once.
    sessions.sort_by(|a, b| a.session.cmp(&b.session));
    let mut results = Vec::new();
    for session in sessions {
        match logout(home, &session.session).await {
            Ok(had_tokens) => results.push(SessionLogout::LoggedOut {
                session,
                had_tokens,
            }),
            Err(error) => results.push(SessionLogout::Failed { session, error }),
        }
    }
    Ok(results)
}
