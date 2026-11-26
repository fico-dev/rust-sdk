//! The MCP server's [`Context`] from a profile.

use std::fmt;
use std::path::Path;

use super::SessionTokens;
use crate::config::{Config, Env, Resolved};
use crate::decision::local::Local;
use crate::decision::platform::Platform;
use crate::mcp::RequestInfo;
use crate::tool::{BoxFuture, Context};
use crate::{ClientBuilder, Error, Result};

/// The provider that serves [`mcp::serve_stdio`](crate::mcp::serve_stdio)
/// and [`mcp::serve_http`](crate::mcp::serve_http) from a profile, as `fico
/// mcp serve --profile` does.
///
/// The profile is resolved once, when the server starts: its host chooses
/// the credentials, which a running server never changes. Each call gets a
/// context whose platform client sends the profile's API key, or its PKCE
/// session's tokens, refreshed first when they expire within 30 seconds,
/// so a long-lived server never sends an expired token. A session that is
/// not logged in fails the call with [`Error::NotLoggedIn`] before its tool
/// runs. The request's own `Authorization` header is never read, so it is
/// never passed through to the platform.
///
/// # Examples
///
/// ```no_run
/// # async fn run() -> fico_sdk::Result<()> {
/// use fico_sdk::auth::ProfileProvider;
/// use fico_sdk::config::{self, Env};
/// use fico_sdk::decision::local::Local;
/// use fico_sdk::mcp::{SurfaceOptions, serve_stdio};
///
/// let home = config::home_dir()?;
/// let provider = ProfileProvider::from_profile(&home, None, &Env::from_process())
///     .await?
///     .with_local(Local::with_home(&home));
/// serve_stdio(move |info| provider.context(info), SurfaceOptions::default()).await
/// # }
/// ```
#[derive(Clone)]
pub struct ProfileProvider {
    platform: Platform,
    tokens: Option<SessionTokens>,
    local: Option<Local>,
}

impl ProfileProvider {
    /// The provider for `resolved`, whose PKCE tokens are in the FICO home
    /// `home`, as is the dependency cache that push uses
    /// ([`Platform::with_home`]). `configure` adds to its client, e.g. the
    /// host's user agent. Makes no request.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`ClientBuilder::build`].
    pub fn new(
        home: &Path,
        resolved: &Resolved,
        configure: impl FnOnce(ClientBuilder) -> ClientBuilder,
    ) -> Result<ProfileProvider> {
        let (builder, tokens) = resolved.client_parts(home);
        Ok(ProfileProvider {
            platform: Platform::new(configure(builder).build()?).with_home(home),
            tokens,
            local: None,
        })
    }

    /// The provider for `profile`, resolved from `<home>/config.yaml` and
    /// `env` as [`Config::resolve`] does. Reads the file and makes no
    /// request.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Config::load`] and [`Config::resolve`], and
    /// [`Error::Invalid`] if nothing is configured.
    pub async fn from_profile(
        home: &Path,
        profile: Option<&str>,
        env: &Env,
    ) -> Result<ProfileProvider> {
        let config = Config::load(home).await?;
        let resolved = config
            .resolve(profile, env)?
            .ok_or_else(|| Error::Invalid {
                reason: "no profile is configured".to_owned(),
            })?;
        ProfileProvider::new(home, &resolved, |builder| builder)
    }

    /// Gives every context `local` too, for local mode
    /// ([`serve_stdio`](crate::mcp::serve_stdio)). Remote mode drops it.
    pub fn with_local(mut self, local: Local) -> ProfileProvider {
        self.local = Some(local);
        self
    }

    /// The context of one call. `info` is not read: the call acts with the
    /// profile.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotLoggedIn`], or another error of the PKCE
    /// session's refresh, when its token can't be had.
    pub fn context(&self, info: RequestInfo) -> BoxFuture<'static, Result<Context>> {
        // Never passed through: the profile's credentials serve every call.
        let _ = info;
        let provider = self.clone();
        Box::pin(async move {
            if let Some(tokens) = &provider.tokens {
                tokens.bearer().await?;
            }
            let context = Context::new().with_platform(provider.platform);
            Ok(match provider.local {
                Some(local) => context.with_local(local),
                None => context,
            })
        })
    }
}

impl fmt::Debug for ProfileProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProfileProvider")
            .field("platform", &self.platform)
            .field("tokens", &self.tokens)
            .field("local", &self.local.is_some())
            .finish()
    }
}
