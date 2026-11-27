//! Profiles and `config.yaml`: the file, its profiles, and the resolution
//! of a profile and the environment into an endpoint and a credential.
//!
//! The file is `<home>/config.yaml`, where `<home>` is the FICO home
//! ([`home_dir`]). The CLI, the MCP server and the Python SDK share it, so
//! [`Config::save`] writes it atomically, with mode 0600 on Unix. Changes
//! take no lock: when two processes change the file at the same time, the
//! last one to save wins.
//!
//! This module prints nothing and names no CLI command: callers word every
//! message. Its errors never quote an API key.
//!
//! Whole-call functions do what a host needs in one step, over the FICO
//! home: [`create_profile`] (with the PKCE login and the team),
//! [`get_profile`], [`list_profiles`] and [`delete_profile`] (with the
//! tokens of a session it leaves unused). The host only asks the user and
//! shows the messages.
//!
//! The PKCE login, its tokens and the teams are in [`auth`](crate::auth).
//! They build on [`Credential::Session`], [`Config::endpoint_oidc`], the
//! session names that [`Config::set_profile`] checks,
//! [`ProfileSet::stale_session`] and [`ProfileDeleted::unused_session`].

mod profiles;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::io::AsyncWriteExt;

use crate::auth::SessionTokens;
use crate::rand::random_fraction;
use crate::{Client, ClientBuilder, Error, ProfileProblem, Result};

pub use self::profiles::{
    NamedProfile, PkceParams, ProfileCreated, ProfileParams, ProfileRemoved, SessionUsed,
    UnusedSession, create_profile, delete_profile, get_profile, list_profiles,
};

/// The default platform endpoint, which profiles without an endpoint use,
/// and which a resolution without a profile falls back to:
/// `https://api.cloud.nextmv.io`, Nextmv's, until FICO has its own. It is
/// written as profiles store endpoints.
pub const DEFAULT_ENDPOINT: Option<&str> = Some("https://api.cloud.nextmv.io");

/// The built-in OIDC settings of [`DEFAULT_ENDPOINT`], as `(discovery URL,
/// client ID)`, which [`Config::endpoint_oidc`] falls back to: Nextmv's
/// Cognito user pool and its client, until FICO has its own. The client
/// redirects to `http://127.0.0.1:56734`, the login's default callback port,
/// with the scopes the login asks for.
const DEFAULT_ENDPOINT_OIDC: Option<(&str, &str)> = Some((
    "https://cognito-idp.us-east-2.amazonaws.com/us-east-2_1jHS2b9HU/.well-known/openid-configuration",
    "4k91vdlr1m52v9v45h9vc7a25e",
));

/// The header that carries a PKCE profile's team ID. A placeholder: it is
/// the name today's backend reads.
pub const ACCOUNT_HEADER: &str = "nextmv-account";

/// The session of PKCE profiles that set none. It belongs to the default
/// endpoint, so profiles on other endpoints need a session of their own.
pub const DEFAULT_SESSION: &str = "default";

/// The environment variable with the FICO home, which [`home_dir`] reads.
pub const HOME_ENV: &str = "FICO_HOME";

/// The environment variable with a profile name, which
/// [`Env::from_process`] reads.
pub const PROFILE_ENV: &str = "FICO_PROFILE";

/// The environment variable with an endpoint, which [`Env::from_process`]
/// reads.
pub const ENDPOINT_ENV: &str = "FICO_ENDPOINT";

/// The environment variable with an API key, which [`Env::from_process`]
/// reads.
pub const API_KEY_ENV: &str = "FICO_API_KEY";

/// Name of the configuration file in the FICO home.
const FILE: &str = "config.yaml";
/// Name of the FICO home in the user's home directory.
const HOME_DIR: &str = ".fico";
/// What an OIDC discovery URL ends with.
const OIDC_DISCOVERY_SUFFIX: &str = "/.well-known/openid-configuration";
/// The values of a profile's `auth_type`, one per variant of [`Profile`].
const AUTH_TYPES: [&str; 2] = ["api_key", "pkce"];
/// The keys of a profile that hold strings.
const PROFILE_KEYS: [&str; 6] = [
    "auth_type",
    "endpoint",
    "api_key",
    "auth_session",
    "team_id",
    "sso_domain",
];
/// Names that Windows reserves for devices, with or without an extension.
const WINDOWS_DEVICE_NAMES: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// The FICO home: `FICO_HOME` when it is set and not blank, else `.fico` in
/// the user's home directory (`$HOME/.fico`, `%USERPROFILE%\.fico` on
/// Windows). Does no I/O.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if `FICO_HOME` is unset and the user's home
/// directory cannot be found.
pub fn home_dir() -> Result<PathBuf> {
    home_dir_from(std::env::var_os(HOME_ENV), std::env::home_dir())
}

/// The FICO home for the value of `FICO_HOME` and the user's home
/// directory. See [`home_dir`].
pub(crate) fn home_dir_from(fico_home: Option<OsString>, home: Option<PathBuf>) -> Result<PathBuf> {
    let blank = |value: &OsString| value.to_str().is_some_and(|value| value.trim().is_empty());
    if let Some(fico_home) = fico_home.filter(|value| !blank(value)) {
        return Ok(PathBuf::from(fico_home));
    }
    home.filter(|home| !home.as_os_str().is_empty())
        .map(|home| home.join(HOME_DIR))
        .ok_or_else(|| Error::Invalid {
            reason: format!("could not find the home directory: set `{HOME_ENV}`"),
        })
}

/// The contents of `config.yaml`: the profiles, which one is the default,
/// and the OIDC settings of endpoints.
///
/// Read it with [`Config::load`], change it with [`Config::set_profile`],
/// [`Config::delete_profile`] and [`Config::set_endpoint_oidc`], and write
/// it with [`Config::save`]. Serialized, it is also the JSON that lists the
/// profiles: [`Config::masked`] hides their API keys first.
///
/// Only the keys below are read and written. Others are dropped when the
/// file is saved.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Name of the default profile. [`Config::set_profile`] and
    /// [`Config::delete_profile`] keep it naming a profile, and absent when
    /// there are none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_profile: Option<String>,
    /// The profiles, by name. `default` is an ordinary name.
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    /// The OIDC settings of endpoints, by endpoint without its scheme, e.g.
    /// `api.staging.example.com`. The default endpoint's are built in.
    #[serde(default)]
    pub endpoints: BTreeMap<String, EndpointOidc>,
}

/// A profile: the endpoint to reach and how to authenticate there.
///
/// In `config.yaml`, its `auth_type` (`api_key` or `pkce`) says which
/// variant it is, so switching a profile to the other type drops the other
/// type's keys. [`Config::set_profile`] checks and normalizes it.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "auth_type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Profile {
    /// Authenticates with an API key, sent as `Authorization: Bearer <key>`.
    ApiKey {
        /// The platform endpoint, e.g. `https://api.example.com`. `None`
        /// means [`DEFAULT_ENDPOINT`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        endpoint: Option<String>,
        /// The API key.
        api_key: String,
    },
    /// Authenticates with a PKCE login, whose tokens belong to its session.
    Pkce {
        /// The platform endpoint, e.g. `https://api.example.com`. `None`
        /// means [`DEFAULT_ENDPOINT`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        endpoint: Option<String>,
        /// The session whose tokens the profile uses: profiles that share a
        /// session share one login. `None` means [`DEFAULT_SESSION`].
        /// [`Config::set_profile`] stores it in lowercase.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth_session: Option<String>,
        /// The team (organization) the profile acts for, sent in
        /// [`ACCOUNT_HEADER`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        team_id: Option<String>,
        /// The email domain whose SSO provider the login goes through.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sso_domain: Option<String>,
    },
}

impl fmt::Debug for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Profile::ApiKey { endpoint, .. } => f
                .debug_struct("ApiKey")
                .field("endpoint", endpoint)
                .field("api_key", &"<redacted>")
                .finish(),
            Profile::Pkce {
                endpoint,
                auth_session,
                team_id,
                sso_domain,
            } => f
                .debug_struct("Pkce")
                .field("endpoint", endpoint)
                .field("auth_session", auth_session)
                .field("team_id", team_id)
                .field("sso_domain", sso_domain)
                .finish(),
        }
    }
}

/// The OIDC settings of an endpoint, which a PKCE login on it needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointOidc {
    /// URL of the identity provider's OIDC discovery document, ending in
    /// `/.well-known/openid-configuration`.
    pub oidc_discovery_url: String,
    /// The OAuth2 client ID at that identity provider.
    pub client_id: String,
    /// The port of the login's loopback callback,
    /// `http://127.0.0.1:<port>`. An identity provider accepts only the
    /// redirect URIs registered for its client, so it is the registered
    /// port. `None` means 56734; `0` means any free port, for a provider
    /// that accepts any loopback port (RFC 8252, section 7.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_port: Option<u16>,
}

/// What [`Config::set_profile`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProfileSet {
    /// Name of the profile, trimmed.
    pub name: String,
    /// The profile's previous PKCE session, in lowercase, when no PKCE
    /// profile uses it with its previous endpoint anymore: after a switch to
    /// an API key, to another session, or to another endpoint. Its tokens
    /// came from that endpoint, so they should be deleted. `None` too when
    /// the session's name, edited by hand into the file, is not a valid
    /// session name.
    pub stale_session: Option<String>,
}

/// What [`Config::delete_profile`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProfileDeleted {
    /// The default profile after the deletion: unchanged, the `new_default`
    /// that was given, or `None` once no profile is left.
    pub new_default: Option<String>,
    /// The session of the deleted PKCE profile, in lowercase, when no other
    /// profile uses it, so that its tokens can be deleted too. `None` too
    /// when the session's name, edited by hand into the file, is not a
    /// valid session name.
    pub unused_session: Option<String>,
}

/// The environment variables that [`Config::resolve`] reads. A plain
/// struct, so callers and tests can set them without the process
/// environment.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Env {
    /// The profile name (`FICO_PROFILE`), which replaces the profile given.
    pub profile: Option<String>,
    /// The endpoint (`FICO_ENDPOINT`), which replaces the profile's.
    pub endpoint: Option<String>,
    /// An API key (`FICO_API_KEY`), which replaces the profile's API key or
    /// PKCE session.
    pub api_key: Option<String>,
}

impl Env {
    /// Reads `FICO_PROFILE`, `FICO_ENDPOINT` and `FICO_API_KEY` from the
    /// process environment. A variable that is empty, blank or not Unicode
    /// counts as unset.
    pub fn from_process() -> Env {
        Env::from_vars(|name| std::env::var(name).ok())
    }

    /// The environment that `var` gives for each variable name.
    pub(crate) fn from_vars(var: impl Fn(&str) -> Option<String>) -> Env {
        let read = |name| var(name).filter(|value| !value.trim().is_empty());
        Env {
            profile: read(PROFILE_ENV),
            endpoint: read(ENDPOINT_ENV),
            api_key: read(API_KEY_ENV),
        }
    }
}

impl fmt::Debug for Env {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let api_key = self.api_key.as_ref().map(|_| "<redacted>");
        f.debug_struct("Env")
            .field("profile", &self.profile)
            .field("endpoint", &self.endpoint)
            .field("api_key", &api_key)
            .finish()
    }
}

/// A resolved profile: the endpoint and the credential to reach the
/// platform with. [`Resolved::client_builder`] builds the client.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Resolved {
    /// Name of the profile, or `None` when only the environment was used.
    pub profile: Option<String>,
    /// The endpoint, normalized, e.g. `https://api.example.com`.
    pub endpoint: String,
    /// How to authenticate.
    pub credential: Credential,
    /// The team of a PKCE profile, when its session is the credential.
    pub team_id: Option<String>,
    /// The OIDC settings of the endpoint ([`Config::endpoint_oidc`]), when
    /// a PKCE session is the credential: its tokens are refreshed there.
    pub oidc: Option<EndpointOidc>,
    /// The environment variables that replaced a value of the profile, so
    /// that the caller can warn about them.
    pub overrides: Vec<EnvOverride>,
}

impl Resolved {
    /// A [`ClientBuilder`] for the endpoint. The caller can add more, such
    /// as its user agent.
    ///
    /// An API key is the bearer token. A PKCE session's bearer comes from
    /// its tokens in the FICO home `home`, which the client reads before
    /// each request and refreshes when they expire within 30 seconds
    /// ([`auth`](crate::auth)), so building the client makes no request; a
    /// request fails with [`Error::NotLoggedIn`] when the session has no
    /// usable tokens. A PKCE profile's team goes in [`ACCOUNT_HEADER`].
    pub fn client_builder(&self, home: &Path) -> ClientBuilder {
        self.client_parts(home).0
    }

    /// [`Resolved::client_builder`], and the token source it sets for a
    /// PKCE session.
    pub(crate) fn client_parts(&self, home: &Path) -> (ClientBuilder, Option<SessionTokens>) {
        let builder = Client::builder(&self.endpoint);
        let session = match &self.credential {
            Credential::ApiKey(api_key) => return (builder.bearer_token(api_key.as_str()), None),
            Credential::Session(session) => session,
        };
        let tokens = SessionTokens::new(home, session, &self.endpoint, self.oidc.clone());
        let builder = builder.token_source(tokens.clone());
        let builder = match &self.team_id {
            Some(team_id) => builder.header(ACCOUNT_HEADER, team_id.as_str()),
            None => builder,
        };
        (builder, Some(tokens))
    }
}

/// How a resolved profile authenticates.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Credential {
    /// An API key, sent as `Authorization: Bearer <key>`.
    ApiKey(String),
    /// A PKCE session, by name. Its tokens are in
    /// `<home>/auth/<session>/tokens.json` ([`auth`](crate::auth)).
    Session(String),
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Credential::ApiKey(_) => f.debug_tuple("ApiKey").field(&"<redacted>").finish(),
            Credential::Session(session) => f.debug_tuple("Session").field(session).finish(),
        }
    }
}

/// An environment variable that replaced a value of the resolved profile.
/// It names the profile, never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EnvOverride {
    /// `FICO_PROFILE` replaced a different profile that the caller gave.
    Profile {
        /// Name of the profile that `FICO_PROFILE` selected.
        profile: String,
        /// Name of the profile that the caller gave.
        given: String,
    },
    /// `FICO_ENDPOINT` replaced the profile's endpoint.
    Endpoint {
        /// Name of the profile.
        profile: String,
    },
    /// `FICO_API_KEY` replaced the profile's API key or its PKCE session.
    ApiKey {
        /// Name of the profile.
        profile: String,
    },
}

impl Config {
    /// The path of `config.yaml` in the FICO home `home`.
    pub fn path(home: &Path) -> PathBuf {
        home.join(FILE)
    }

    /// Reads `config.yaml` from the FICO home `home`. A missing or empty
    /// file is an empty configuration. Keys this version does not know are
    /// ignored, and so are the `endpoints` entries without both values.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the file cannot be read, and
    /// [`Error::Invalid`] if it is not a valid configuration. The message
    /// names the file, but quotes none of its values.
    pub async fn load(home: &Path) -> Result<Config> {
        let path = Config::path(home);
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Ok(Config::default());
            }
            Err(source) => return Err(Error::Io { path, source }),
        };
        decode(&bytes).map_err(|why| Error::Invalid {
            reason: format!("`{}` is not a valid configuration: {why}", path.display()),
        })
    }

    /// Writes `config.yaml` to the FICO home `home`, creating the directory
    /// when it is missing.
    ///
    /// The write is atomic: a temporary file in the same directory, renamed
    /// over the old file, so a reader sees the old file or the new one. On
    /// Unix the file is created with mode 0600. It takes no lock, so of two
    /// changes saved at the same time, the last one wins.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the directory or the file cannot be written.
    pub async fn save(&self, home: &Path) -> Result<()> {
        let path = Config::path(home);
        let yaml = serde_saphyr::to_string(self).map_err(|e| Error::Io {
            path: path.clone(),
            source: io::Error::new(io::ErrorKind::InvalidData, e.to_string()),
        })?;
        tokio::fs::create_dir_all(home)
            .await
            .map_err(|source| Error::Io {
                path: home.to_owned(),
                source,
            })?;
        let suffix = (random_fraction() * (1u64 << 53) as f64) as u64;
        let temp = home.join(format!(".{FILE}.{suffix:x}.tmp"));
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temp).await.map_err(|source| Error::Io {
            path: temp.clone(),
            source,
        })?;
        let written = async {
            file.write_all(yaml.as_bytes()).await?;
            file.flush().await?;
            file.sync_all().await
        }
        .await;
        drop(file);
        if let Err(source) = written {
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(Error::Io { path: temp, source });
        }
        if let Err(source) = tokio::fs::rename(&temp, &path).await {
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(Error::Io { path, source });
        }
        Ok(())
    }

    /// Creates or replaces the profile `name`. It becomes the default
    /// profile if `make_default` is set, or if there is no default profile
    /// yet, so the first profile always is. Returns the trimmed name, and
    /// the session whose tokens the change leaves stale
    /// ([`ProfileSet::stale_session`]).
    ///
    /// The profile is stored normalized:
    ///
    /// * The endpoint is trimmed and has no trailing `/`. A bare host means
    ///   `https://<host>`; `https://` and `http://` are kept. `None` means
    ///   [`DEFAULT_ENDPOINT`], so a profile needs an endpoint while there is
    ///   none.
    /// * An API key is trimmed.
    /// * A PKCE session is trimmed and stored in lowercase, so that each
    ///   session has one directory of tokens, also where file names ignore
    ///   case (macOS, Windows). `default` is stored as `None`,
    ///   [`DEFAULT_SESSION`]. A session belongs to one endpoint, so the
    ///   default session is only for the default endpoint, and a session
    ///   that another profile uses with another endpoint is rejected.
    /// * `sso_domain` takes an email or a domain and keeps the domain (see
    ///   [`email_domain`]), and `team_id` is trimmed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`], before any change, if the name or the API
    /// key is empty, the API key has control characters, the endpoint is
    /// not an `https` or `http` URL, or the session breaks the rules above,
    /// and [`Error::Profile`] with [`ProfileProblem::NoEndpoint`] if there is
    /// no endpoint, or [`ProfileProblem::NoSession`] if the session is the
    /// default one on another endpoint than the default. A session name is
    /// one or more ASCII letters, digits, `.`, `_` and `-`, starts with a
    /// letter or a digit, does not end with `.`, and is not a name Windows
    /// reserves for a device, such as `con`, because it names the directory
    /// of its tokens.
    pub fn set_profile(
        &mut self,
        name: &str,
        profile: Profile,
        make_default: bool,
    ) -> Result<ProfileSet> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::Invalid {
                reason: "the profile name must not be empty".to_owned(),
            });
        }
        let profile = self.checked(name, profile)?;
        let before = self.profiles.get(name).and_then(session_on);
        self.profiles.insert(name.to_owned(), profile);
        let has_default = self
            .default_profile
            .as_ref()
            .is_some_and(|default| self.profiles.contains_key(default));
        if make_default || !has_default {
            self.default_profile = Some(name.to_owned());
        }
        // The previous session's tokens came from its previous endpoint, so
        // they are stale once no profile uses the session there.
        let stale_session = before
            .filter(|old| {
                let mut now = self.profiles.values().filter_map(session_on);
                !now.any(|now| now == *old)
            })
            .map(|(session, _)| session)
            .filter(|session| check_session(session).is_ok());
        Ok(ProfileSet {
            name: name.to_owned(),
            stale_session,
        })
    }

    /// Deletes the profile `name`. With `new_default`, that other profile
    /// becomes the default.
    ///
    /// Deleting the default profile while other profiles exist needs
    /// `new_default`. Deleting the last profile leaves no default.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] if `name` or `new_default` is not a
    /// profile, [`Error::Invalid`] if `new_default` is `name`, and
    /// [`Error::Profile`] with [`ProfileProblem::NewDefaultNeeded`] if `name`
    /// is the default profile, others exist and `new_default` is `None`.
    /// Nothing changes then.
    pub fn delete_profile(
        &mut self,
        name: &str,
        new_default: Option<&str>,
    ) -> Result<ProfileDeleted> {
        let name = name.trim();
        let Some(deleted) = self.profiles.get(name).cloned() else {
            return Err(profile_not_found(name));
        };
        let new_default = new_default.map(str::trim).filter(|other| !other.is_empty());
        if let Some(new_default) = new_default {
            if new_default == name {
                return Err(Error::Invalid {
                    reason: format!(
                        "the new default profile must be another profile than `{name}`, \
                         which is deleted"
                    ),
                });
            }
            if !self.profiles.contains_key(new_default) {
                return Err(profile_not_found(new_default));
            }
        }
        let is_default = self.default_profile.as_deref() == Some(name);
        if is_default && new_default.is_none() && self.profiles.len() > 1 {
            let mut profiles: Vec<String> = self.profiles.keys().cloned().collect();
            profiles.retain(|other| other != name);
            return Err(Error::Profile {
                profile: name.to_owned(),
                problem: ProfileProblem::NewDefaultNeeded { profiles },
            });
        }

        self.profiles.remove(name);
        if let Some(new_default) = new_default {
            self.default_profile = Some(new_default.to_owned());
        } else if is_default || self.profiles.is_empty() {
            self.default_profile = None;
        }
        // A session name edited by hand into the file may be no safe name
        // for a directory, so only a valid one is returned.
        let unused_session = match deleted {
            Profile::Pkce { auth_session, .. } => {
                let session = session_name(auth_session.as_deref());
                let used = self.profiles.values().any(|profile| {
                    matches!(profile, Profile::Pkce { auth_session, .. }
                        if session_name(auth_session.as_deref()) == session)
                });
                (!used && check_session(&session).is_ok()).then_some(session)
            }
            Profile::ApiKey { .. } => None,
        };
        Ok(ProfileDeleted {
            new_default: self.default_profile.clone(),
            unused_session,
        })
    }

    /// Sets the OIDC settings of `endpoint`, and returns whether they
    /// replaced different ones, stored or built in.
    ///
    /// The discovery URL gets `/.well-known/openid-configuration` appended
    /// when it does not end with it, and the client ID is trimmed. The
    /// settings are stored under the endpoint without its scheme, replacing
    /// those stored under another spelling of it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`], before any change, if the endpoint or the
    /// discovery URL is not an `https` or `http` URL, or the client ID is
    /// empty.
    pub fn set_endpoint_oidc(&mut self, endpoint: &str, oidc: EndpointOidc) -> Result<bool> {
        let endpoint = normalize_endpoint(endpoint)?;
        let client_id = oidc.client_id.trim();
        if client_id.is_empty() {
            return Err(Error::Invalid {
                reason: "the OIDC client ID must not be empty".to_owned(),
            });
        }
        let oidc = EndpointOidc {
            oidc_discovery_url: normalize_discovery_url(&oidc.oidc_discovery_url)?,
            client_id: client_id.to_owned(),
            callback_port: oidc.callback_port,
        };
        let replaced = self
            .endpoint_oidc(&endpoint)
            .is_some_and(|current| current != oidc);
        let key = endpoint_key(&endpoint).to_owned();
        self.endpoints
            .retain(|stored, _| stored_endpoint_key(stored).as_deref() != Some(&key));
        self.endpoints.insert(key, oidc);
        Ok(replaced)
    }

    /// The OIDC settings of `endpoint`: the stored ones, else the default
    /// endpoint's built-in ones. `None` when there are none, or `endpoint`
    /// is not a valid endpoint.
    pub fn endpoint_oidc(&self, endpoint: &str) -> Option<EndpointOidc> {
        let endpoint = normalize_endpoint(endpoint).ok()?;
        let key = endpoint_key(&endpoint);
        let stored = self.endpoints.get(key).or_else(|| {
            let mut endpoints = self.endpoints.iter();
            endpoints
                .find(|(stored, _)| stored_endpoint_key(stored).as_deref() == Some(key))
                .map(|(_, oidc)| oidc)
        });
        if let Some(oidc) = stored {
            // A stored discovery URL gets the suffix on read too.
            let url = normalize_discovery_url(&oidc.oidc_discovery_url)
                .unwrap_or_else(|_| oidc.oidc_discovery_url.clone());
            return Some(EndpointOidc {
                oidc_discovery_url: url,
                client_id: oidc.client_id.clone(),
                callback_port: oidc.callback_port,
            });
        }
        if Some(&endpoint) != default_endpoint().as_ref() {
            return None;
        }
        DEFAULT_ENDPOINT_OIDC.map(|(url, client_id)| EndpointOidc {
            oidc_discovery_url: url.to_owned(),
            client_id: client_id.to_owned(),
            callback_port: None,
        })
    }

    /// A copy whose API keys are masked as `ab****yz` (see
    /// [`mask_api_key`]), to list the profiles.
    pub fn masked(&self) -> Config {
        let mut config = self.clone();
        for profile in config.profiles.values_mut() {
            if let Profile::ApiKey { api_key, .. } = profile {
                *api_key = mask_api_key(api_key);
            }
        }
        config
    }

    /// Resolves the profile to use, and the endpoint and the credential,
    /// with the environment's overrides.
    ///
    /// 1. **Profile:** `env.profile`, else `profile` (a caller's flag), else
    ///    the default profile. The environment variable beats the flag, as
    ///    the FICO CLI's env vars beat their options.
    /// 2. **Endpoint:** `env.endpoint`, else the profile's, else
    ///    [`DEFAULT_ENDPOINT`], normalized as [`Config::set_profile`]
    ///    stores endpoints. A scheme in `env.endpoint` is accepted.
    /// 3. **Credential:** `env.api_key`, which also replaces a PKCE
    ///    session, else the profile's API key or its PKCE session.
    ///
    /// When a profile resolved, [`Resolved::overrides`] says whether
    /// `env.profile` replaced a different `profile`, and whether
    /// `env.endpoint` and `env.api_key` replaced a different value of it.
    /// Without any profile, `env.api_key` is enough, with `env.endpoint`
    /// while there is no default endpoint, so no file is needed. Empty and
    /// blank values count as unset. The profile is checked with the rules
    /// of [`Config::set_profile`], since the file may have been edited by
    /// hand.
    ///
    /// Returns `Ok(None)` when nothing is configured: no profile resolves
    /// and `env.api_key` is unset.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] if the profile it names does not exist,
    /// [`Error::Profile`] with [`ProfileProblem::NoEndpoint`] if the profile
    /// has no endpoint, and [`Error::Invalid`] if `env.api_key` is set
    /// without a profile or an endpoint, an endpoint is not valid or the
    /// profile breaks the other rules of [`Config::set_profile`].
    pub fn resolve(&self, profile: Option<&str>, env: &Env) -> Result<Option<Resolved>> {
        let env_profile = set(env.profile.as_deref());
        let given = set(profile);
        let name = env_profile
            .or(given)
            .or_else(|| set(self.default_profile.as_deref()));
        let selected = match name {
            Some(name) => {
                let stored = self
                    .profiles
                    .get(name)
                    .ok_or_else(|| profile_not_found(name))?;
                Some((name, self.checked(name, stored.clone())?))
            }
            None => None,
        };
        let env_api_key = set(env.api_key.as_deref());
        let env_endpoint = set(env.endpoint.as_deref())
            .map(normalize_endpoint)
            .transpose()?;

        let Some((name, profile)) = selected else {
            let Some(api_key) = env_api_key else {
                return Ok(None);
            };
            let endpoint =
                env_endpoint
                    .or_else(default_endpoint)
                    .ok_or_else(|| Error::Invalid {
                        reason: format!(
                            "no endpoint: set `{ENDPOINT_ENV}`, since there is no default endpoint"
                        ),
                    })?;
            return Ok(Some(Resolved {
                profile: None,
                endpoint,
                credential: Credential::ApiKey(api_key.to_owned()),
                team_id: None,
                oidc: None,
                overrides: Vec::new(),
            }));
        };

        let (profile_endpoint, profile_api_key) = match &profile {
            Profile::ApiKey { endpoint, api_key } => (endpoint, Some(api_key.as_str())),
            Profile::Pkce { endpoint, .. } => (endpoint, None),
        };
        let profile_endpoint = profile_endpoint.clone().or_else(default_endpoint);
        let mut overrides = Vec::new();
        if let (Some(env_profile), Some(given)) = (env_profile, given)
            && env_profile != given
        {
            overrides.push(EnvOverride::Profile {
                profile: env_profile.to_owned(),
                given: given.to_owned(),
            });
        }
        if env_endpoint.is_some() && env_endpoint != profile_endpoint {
            overrides.push(EnvOverride::Endpoint {
                profile: name.to_owned(),
            });
        }
        let endpoint = env_endpoint
            .or(profile_endpoint)
            .ok_or_else(|| Error::Profile {
                profile: name.to_owned(),
                problem: ProfileProblem::NoEndpoint,
            })?;
        let credential = match (env_api_key, &profile) {
            (Some(api_key), _) => {
                if profile_api_key != Some(api_key) {
                    overrides.push(EnvOverride::ApiKey {
                        profile: name.to_owned(),
                    });
                }
                Credential::ApiKey(api_key.to_owned())
            }
            (None, Profile::ApiKey { api_key, .. }) => Credential::ApiKey(api_key.clone()),
            (None, Profile::Pkce { auth_session, .. }) => {
                Credential::Session(session_name(auth_session.as_deref()))
            }
        };
        // The team goes with the PKCE session only.
        let team_id = match (&credential, &profile) {
            (Credential::Session(_), Profile::Pkce { team_id, .. }) => team_id.clone(),
            _ => None,
        };
        let oidc = match &credential {
            Credential::Session(_) => self.endpoint_oidc(&endpoint),
            Credential::ApiKey(_) => None,
        };
        Ok(Some(Resolved {
            profile: Some(name.to_owned()),
            endpoint,
            credential,
            team_id,
            oidc,
            overrides,
        }))
    }

    /// `profile` as [`Config::set_profile`] stores it under `name`, or why
    /// it breaks its rules.
    fn checked(&self, name: &str, profile: Profile) -> Result<Profile> {
        match profile {
            Profile::ApiKey { endpoint, api_key } => {
                let (endpoint, _) = checked_endpoint(name, endpoint.as_deref())?;
                let api_key = api_key.trim();
                if api_key.is_empty() {
                    return Err(Error::Invalid {
                        reason: format!("the API key of profile `{name}` must not be empty"),
                    });
                }
                if api_key.chars().any(char::is_control) {
                    return Err(Error::Invalid {
                        reason: format!(
                            "the API key of profile `{name}` must not contain control characters"
                        ),
                    });
                }
                Ok(Profile::ApiKey {
                    endpoint,
                    api_key: api_key.to_owned(),
                })
            }
            Profile::Pkce {
                endpoint,
                auth_session,
                team_id,
                sso_domain,
            } => {
                let (endpoint, used_endpoint) = checked_endpoint(name, endpoint.as_deref())?;
                let auth_session = normalize_session(auth_session.as_deref())?;
                // The default session is the default endpoint's.
                if auth_session.is_none() && Some(&used_endpoint) != default_endpoint().as_ref() {
                    return Err(Error::Profile {
                        profile: name.to_owned(),
                        problem: ProfileProblem::NoSession {
                            endpoint: used_endpoint,
                        },
                    });
                }
                self.check_session_endpoint(name, auth_session.as_deref(), &used_endpoint)?;
                Ok(Profile::Pkce {
                    endpoint,
                    auth_session,
                    team_id: team_id
                        .as_deref()
                        .map(str::trim)
                        .filter(|team_id| !team_id.is_empty())
                        .map(str::to_owned),
                    sso_domain: sso_domain.as_deref().and_then(email_domain),
                })
            }
        }
    }

    /// Checks that no PKCE profile other than `name` uses `session` with an
    /// endpoint other than `endpoint`: a session's tokens belong to one
    /// endpoint, so two endpoints never overwrite each other's tokens.
    fn check_session_endpoint(
        &self,
        name: &str,
        session: Option<&str>,
        endpoint: &str,
    ) -> Result<()> {
        let session = session_name(session);
        for (other, profile) in &self.profiles {
            let Some((other_session, other_endpoint)) = session_on(profile) else {
                continue;
            };
            if other == name || other_session != session {
                continue;
            }
            if other_endpoint.as_deref() != Some(endpoint) {
                return Err(Error::Invalid {
                    reason: format!(
                        "session `{session}` belongs to profile `{other}`, which uses another \
                         endpoint: choose another session"
                    ),
                });
            }
        }
        Ok(())
    }
}

/// `api_key` masked for display: its first and last 2 characters around
/// `****`, e.g. `ab****yz`. A key of 4 characters or fewer is all `*`.
pub fn mask_api_key(api_key: &str) -> String {
    let chars: Vec<char> = api_key.chars().collect();
    if chars.len() <= 4 {
        return "*".repeat(chars.len());
    }
    let start: String = chars[..2].iter().collect();
    let end: String = chars[chars.len() - 2..].iter().collect();
    format!("{start}****{end}")
}

/// The domain of `email`, which may also be just a domain: `user@x`, `@x`
/// and `x` all give `x`. `None` when nothing is left.
pub fn email_domain(email: &str) -> Option<String> {
    let email = email.trim();
    let domain = email.trim_start_matches('@').rsplit('@').next()?.trim();
    (!domain.is_empty()).then(|| domain.to_owned())
}

/// The configuration in `bytes`, or why it is not one. The reason never
/// quotes a value of the file, since the file holds API keys.
fn decode(bytes: &[u8]) -> Result<Config, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "the file is not UTF-8".to_owned())?;
    // Of duplicate keys, the last one wins.
    let options = serde_saphyr::options! {
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::LastWins,
    };
    // Read as plain JSON values first, so that the typed decoding below sees
    // only checked values.
    let root: Value =
        serde_saphyr::from_str_with_options(text, options).map_err(|e| yaml_error(&e))?;
    let mut root = match root {
        Value::Null => return Ok(Config::default()),
        Value::Object(root) => root,
        _ => return Err("the file must be a mapping".to_owned()),
    };
    let default_profile = match root.remove("default_profile") {
        None | Some(Value::Null) => None,
        Some(Value::String(name)) => Some(name),
        Some(_) => return Err("`default_profile` must be a string".to_owned()),
    };
    let mut profiles = BTreeMap::new();
    for (name, profile) in mapping(root.remove("profiles"), "profiles")? {
        let profile = decode_profile(&name, profile)?;
        profiles.insert(name, profile);
    }
    let mut endpoints = BTreeMap::new();
    for (endpoint, oidc) in mapping(root.remove("endpoints"), "endpoints")? {
        if let Some(oidc) = decode_oidc(&endpoint, &oidc)? {
            endpoints.insert(endpoint, oidc);
        }
    }
    Ok(Config {
        default_profile,
        profiles,
        endpoints,
    })
}

/// Why serde-saphyr could not read the file, quoting none of its values.
///
/// Only messages that are fixed text are kept: the parser's, which show at
/// most one character of the input, and two others. The rest can quote the
/// value they reject, such as a number too large for a float, so they give
/// only where the error is.
fn yaml_error(e: &serde_saphyr::Error) -> String {
    use serde_saphyr::Error as YamlError;
    let e = e.without_snippet();
    let what = match e {
        YamlError::ExternalMessage { .. }
        | YamlError::UnknownAnchor { .. }
        | YamlError::Eof { .. } => return e.to_string(),
        YamlError::NonFiniteFloat { .. } => "is not a finite number: quote it",
        // A merge key (`<<`) wraps the error of the value it merges.
        YamlError::AliasError { error, .. }
            if matches!(unaliased(error), YamlError::NonFiniteFloat { .. }) =>
        {
            "is not a finite number: quote it"
        }
        _ => "is not valid here",
    };
    match e.location() {
        Some(at) => format!(
            "the value at line {}, column {} {what}",
            at.line(),
            at.column()
        ),
        None => format!("a value {what}"),
    }
}

/// The error that `e` wraps in alias errors and snippets, if any.
fn unaliased(mut e: &serde_saphyr::Error) -> &serde_saphyr::Error {
    while let serde_saphyr::Error::AliasError { error, .. } = e.without_snippet() {
        e = error;
    }
    e.without_snippet()
}

/// The mapping under the root key `key`, empty when it is missing.
fn mapping(value: Option<Value>, key: &str) -> Result<Map<String, Value>, String> {
    match value {
        None | Some(Value::Null) => Ok(Map::new()),
        Some(Value::Object(map)) => Ok(map),
        Some(_) => Err(format!("`{key}` must be a mapping")),
    }
}

/// The profile `name` in `value`. Its keys are checked to hold strings, and
/// its `auth_type` to be known, first, because serde's errors quote the
/// values they reject.
fn decode_profile(name: &str, value: Value) -> Result<Profile, String> {
    let Value::Object(fields) = value else {
        return Err(format!("profile `{name}` must be a mapping"));
    };
    for key in PROFILE_KEYS {
        if fields
            .get(key)
            .is_some_and(|value| !value.is_string() && !value.is_null())
        {
            return Err(format!(
                "`{key}` of profile `{name}` must be a string; quote it if it is a number"
            ));
        }
    }
    let auth_type = fields.get("auth_type").and_then(Value::as_str);
    if auth_type.is_some_and(|auth_type| !AUTH_TYPES.contains(&auth_type)) {
        return Err(format!(
            "`auth_type` of profile `{name}` must be `api_key` or `pkce`"
        ));
    }
    serde_json::from_value(Value::Object(fields)).map_err(|e| format!("profile `{name}`: {e}"))
}

/// The OIDC settings of `endpoint` in `value`. An entry counts only with
/// both values. Its `callback_port`, if set, must be a port number.
fn decode_oidc(endpoint: &str, value: &Value) -> Result<Option<EndpointOidc>, String> {
    let callback_port = match value.get("callback_port") {
        None | Some(Value::Null) => None,
        Some(port) => Some(
            port.as_u64()
                .and_then(|port| u16::try_from(port).ok())
                .ok_or_else(|| {
                    format!("`callback_port` of endpoint `{endpoint}` must be a port number")
                })?,
        ),
    };
    let text = |key| {
        let text = value.get(key)?.as_str()?.trim();
        (!text.is_empty()).then(|| text.to_owned())
    };
    let (Some(oidc_discovery_url), Some(client_id)) =
        (text("oidc_discovery_url"), text("client_id"))
    else {
        return Ok(None);
    };
    Ok(Some(EndpointOidc {
        oidc_discovery_url,
        client_id,
        callback_port,
    }))
}

/// `value`, trimmed, when it is set and not blank.
fn set(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// The error for a profile that does not exist.
fn profile_not_found(name: &str) -> Error {
    Error::NotFound {
        kind: "profile",
        id: name.to_owned(),
    }
}

/// [`DEFAULT_ENDPOINT`], normalized, if there is one.
fn default_endpoint() -> Option<String> {
    DEFAULT_ENDPOINT.and_then(|endpoint| normalize_endpoint(endpoint).ok())
}

/// The endpoint that profile `name` stores, from `endpoint`, and the one it
/// uses, which is the default endpoint when it stores none.
fn checked_endpoint(name: &str, endpoint: Option<&str>) -> Result<(Option<String>, String)> {
    if let Some(endpoint) = endpoint
        .map(str::trim)
        .filter(|endpoint| !endpoint.is_empty())
    {
        let endpoint = normalize_endpoint(endpoint)?;
        return Ok((Some(endpoint.clone()), endpoint));
    }
    match default_endpoint() {
        Some(default) => Ok((None, default)),
        None => Err(Error::Profile {
            profile: name.to_owned(),
            problem: ProfileProblem::NoEndpoint,
        }),
    }
}

/// `endpoint` as profiles store it: trimmed, with `https://` when it has no
/// scheme, the host in lowercase, and no trailing `/`. `https://` and
/// `http://` are kept.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if `endpoint` is empty, has another scheme, no
/// host, a user name or password, a query or a fragment, or is not a URL.
/// The message quotes `endpoint` only when it has no `@`, which could
/// follow a password.
pub(crate) fn normalize_endpoint(endpoint: &str) -> Result<String> {
    let endpoint = endpoint.trim();
    let invalid = |why: &str| Error::Invalid {
        reason: format!("invalid endpoint {}: {why}", quoted_url(endpoint)),
    };
    if endpoint.is_empty() {
        return Err(Error::Invalid {
            reason: "the endpoint must not be empty".to_owned(),
        });
    }
    let url = match endpoint.split_once("://") {
        Some((scheme, _))
            if scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http") =>
        {
            endpoint.to_owned()
        }
        Some((scheme, _))
            if !scheme.is_empty()
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) =>
        {
            return Err(invalid("use `https://`, `http://` or no scheme"));
        }
        _ => format!("https://{endpoint}"),
    };
    let url = Url::parse(&url).map_err(|e| invalid(&e.to_string()))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::Invalid {
            reason: "an endpoint must not contain a user name or a password".to_owned(),
        });
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(invalid("it has no host"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(invalid("it must not have a query or a fragment"));
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// `endpoint`, normalized, without its scheme: the key of its OIDC settings
/// in [`Config::endpoints`].
fn endpoint_key(endpoint: &str) -> &str {
    endpoint
        .strip_prefix("https://")
        .or_else(|| endpoint.strip_prefix("http://"))
        .unwrap_or(endpoint)
}

/// The key that the stored [`Config::endpoints`] key `stored` stands for,
/// if it is a valid endpoint.
fn stored_endpoint_key(stored: &str) -> Option<String> {
    normalize_endpoint(stored)
        .ok()
        .map(|endpoint| endpoint_key(&endpoint).to_owned())
}

/// `url` as an OIDC discovery URL: trimmed, without a trailing `/`, and
/// ending in `/.well-known/openid-configuration`.
fn normalize_discovery_url(url: &str) -> Result<String> {
    let url = url.trim().trim_end_matches('/');
    let url = match url.ends_with(OIDC_DISCOVERY_SUFFIX) {
        true => url.to_owned(),
        false => format!("{url}{OIDC_DISCOVERY_SUFFIX}"),
    };
    match Url::parse(&url) {
        Ok(parsed) if matches!(parsed.scheme(), "https" | "http") && parsed.has_host() => Ok(url),
        _ => Err(Error::Invalid {
            reason: format!(
                "invalid OIDC discovery URL {}: use an `https://` or `http://` URL",
                quoted_url(&url)
            ),
        }),
    }
}

/// `url` in backticks for an error message, or a mention of it when it has
/// an `@`, which could follow a password.
fn quoted_url(url: &str) -> String {
    match url.contains('@') {
        true => "(not shown, since it has an `@`)".to_owned(),
        false => format!("`{url}`"),
    }
}

/// `session` as profiles store it: trimmed and lowercase, with `None` for
/// the default session, which an empty session and `default` also mean.
fn normalize_session(session: Option<&str>) -> Result<Option<String>> {
    let session = session_name(session);
    if session == DEFAULT_SESSION {
        return Ok(None);
    }
    check_session(&session)?;
    Ok(Some(session))
}

/// The session of `profile`, if it is a PKCE profile, and the endpoint it
/// uses there: normalized, or the default endpoint when it stores none.
fn session_on(profile: &Profile) -> Option<(String, Option<String>)> {
    let Profile::Pkce {
        endpoint,
        auth_session,
        ..
    } = profile
    else {
        return None;
    };
    let endpoint = match endpoint.as_deref().map(str::trim) {
        // A profile edited by hand may store an endpoint that is not valid.
        Some(endpoint) if !endpoint.is_empty() => {
            Some(normalize_endpoint(endpoint).unwrap_or_else(|_| endpoint.to_owned()))
        }
        _ => default_endpoint(),
    };
    Some((session_name(auth_session.as_deref()), endpoint))
}

/// The name of the session that `session` stands for, as stored in a
/// profile: trimmed and lowercase, so each session has one name and one
/// directory of tokens on every operating system, and [`DEFAULT_SESSION`]
/// when it is unset or empty.
fn session_name(session: Option<&str>) -> String {
    match session.map(str::trim) {
        Some(session) if !session.is_empty() => session.to_ascii_lowercase(),
        _ => DEFAULT_SESSION.to_owned(),
    }
}

/// Checks that `session` can name the directory of its tokens,
/// `<home>/auth/<session>`, on every operating system, and never leaves
/// `auth`: one or more ASCII letters, digits, `.`, `_` and `-`, starting
/// with a letter or a digit, not ending with `.`, and not a name that
/// Windows reserves for a device.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if it is not such a name.
pub(crate) fn check_session(session: &str) -> Result<()> {
    let invalid = |why: &str| {
        Err(Error::Invalid {
            reason: format!("invalid session name `{session}`: {why}"),
        })
    };
    if session.is_empty() {
        return invalid("it must not be empty");
    }
    if !session
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return invalid("use only ASCII letters, digits, `.`, `_` and `-`");
    }
    if !session.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return invalid("it must start with a letter or a digit");
    }
    if session.ends_with('.') {
        return invalid("it must not end with `.`");
    }
    let stem = session.split('.').next().unwrap_or(session);
    if WINDOWS_DEVICE_NAMES
        .iter()
        .any(|device| stem.eq_ignore_ascii_case(device))
    {
        return invalid("Windows reserves it for a device");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_a_fico_home_that_is_not_blank() {
        let home = Some(PathBuf::from("/home/ada"));
        assert_eq!(
            home_dir_from(Some("/srv/fico".into()), home.clone()).unwrap(),
            PathBuf::from("/srv/fico")
        );
        assert_eq!(
            home_dir_from(Some("/srv/fico".into()), None).unwrap(),
            PathBuf::from("/srv/fico")
        );
        for blank in ["", "  "] {
            assert_eq!(
                home_dir_from(Some(blank.into()), home.clone()).unwrap(),
                PathBuf::from("/home/ada/.fico"),
                "{blank:?}"
            );
        }
        assert_eq!(
            home_dir_from(None, home).unwrap(),
            PathBuf::from("/home/ada/.fico")
        );
        for home in [None, Some(PathBuf::new())] {
            let err = home_dir_from(None, home).unwrap_err();
            assert!(
                matches!(&err, Error::Invalid { reason } if reason.contains("FICO_HOME")),
                "{err}"
            );
        }
    }

    #[test]
    fn reads_the_environment_with_blank_values_unset() {
        let env = Env::from_vars(|name| match name {
            "FICO_PROFILE" => Some("ci".to_owned()),
            "FICO_ENDPOINT" => Some(" ".to_owned()),
            "FICO_API_KEY" => Some("key".to_owned()),
            _ => None,
        });
        assert_eq!(env.profile.as_deref(), Some("ci"));
        assert_eq!(env.endpoint, None);
        assert_eq!(env.api_key.as_deref(), Some("key"));
        assert_eq!(Env::from_vars(|_| Some(String::new())), Env::default());
    }

    #[test]
    fn accepts_portable_session_names_only() {
        for session in ["work", "dev-1", "a.b_c", "9", "Work"] {
            assert!(check_session(session).is_ok(), "{session}");
        }
        for session in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "a\0b",
            ".hidden",
            "-x",
            "_x",
            "x.",
            "a b",
            "é",
            "con",
            "NUL",
            "nul.txt",
            "Com1",
            "lpt9.json",
        ] {
            let err = check_session(session).unwrap_err();
            assert!(matches!(err, Error::Invalid { .. }), "{session:?}: {err}");
        }
        assert!(check_session("console").is_ok());
        assert!(check_session("com10").is_ok());
    }

    #[test]
    fn normalizes_endpoints() {
        for (endpoint, normal) in [
            ("api.example.com", "https://api.example.com"),
            (" https://API.Example.com/ ", "https://api.example.com"),
            ("HTTPS://api.example.com", "https://api.example.com"),
            ("http://127.0.0.1:8080/", "http://127.0.0.1:8080"),
            ("https://api.example.com:443", "https://api.example.com"),
            ("localhost:8080", "https://localhost:8080"),
            (
                "https://api.example.com/base//",
                "https://api.example.com/base",
            ),
        ] {
            assert_eq!(normalize_endpoint(endpoint).unwrap(), normal, "{endpoint}");
        }
        for endpoint in [
            "",
            "  ",
            "ftp://api.example.com",
            "https://",
            "https://api.example.com?x=1",
            "https://api.example.com#top",
            "api example com",
        ] {
            let err = normalize_endpoint(endpoint).unwrap_err();
            assert!(matches!(err, Error::Invalid { .. }), "{endpoint:?}: {err}");
        }
        // Built from parts, so that secret scanners don't read it as a
        // credential.
        let userinfo = ["user", "pa55word"].join(":");
        let err = normalize_endpoint(&format!("https://{userinfo}@api.example.com")).unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
        assert!(!err.to_string().contains("pa55word"), "{err}");
    }

    #[test]
    fn writes_the_default_endpoint_normalized() {
        if let Some(endpoint) = DEFAULT_ENDPOINT {
            assert_eq!(normalize_endpoint(endpoint).unwrap(), endpoint);
        }
        if let Some((url, client_id)) = DEFAULT_ENDPOINT_OIDC {
            assert!(DEFAULT_ENDPOINT.is_some());
            assert_eq!(normalize_discovery_url(url).unwrap(), url);
            assert_eq!(client_id.trim(), client_id);
        }
    }

    #[test]
    fn keys_endpoints_without_their_scheme() {
        assert_eq!(endpoint_key("https://api.example.com"), "api.example.com");
        assert_eq!(endpoint_key("http://127.0.0.1:8080/x"), "127.0.0.1:8080/x");
        assert_eq!(
            stored_endpoint_key("HTTPS://API.example.com/").as_deref(),
            Some("api.example.com")
        );
        assert_eq!(stored_endpoint_key("ftp://x"), None);
    }
}
