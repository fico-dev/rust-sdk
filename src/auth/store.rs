//! The token store: `<home>/auth/<session>/tokens.json`.
//!
//! Each session has one directory, mode 0700 on Unix, whose `tokens.json`
//! (mode 0600) is written atomically: a temporary file in the same
//! directory, renamed over the old one. The file also names the endpoint
//! that issued the tokens, so a session's tokens are never sent to another
//! endpoint.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::config::check_session;
use crate::rand::random_fraction;
use crate::{Error, Result};

/// Directory, in the FICO home, that holds the sessions' tokens.
const DIR: &str = "auth";
/// Name of a session's token file.
const FILE: &str = "tokens.json";

/// The tokens of a session, as stored.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Tokens {
    /// The endpoint the tokens are for, normalized.
    pub(crate) endpoint: String,
    /// The token type, `Bearer`.
    pub(crate) token_type: String,
    /// The OAuth2 access token.
    pub(crate) access_token: String,
    /// The OIDC ID token, a JWT.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) id_token: Option<String>,
    /// The refresh token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) refresh_token: Option<String>,
    /// When the access token expires, as an RFC 3339 timestamp in UTC.
    pub(crate) expires_at: String,
}

impl fmt::Debug for Tokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redacted = |token: &Option<String>| token.as_ref().map(|_| "<redacted>");
        f.debug_struct("Tokens")
            .field("endpoint", &self.endpoint)
            .field("token_type", &self.token_type)
            .field("access_token", &"<redacted>")
            .field("id_token", &redacted(&self.id_token))
            .field("refresh_token", &redacted(&self.refresh_token))
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// The path of the token file of `session` in `home`. The session name is
/// read in lowercase, as profiles store it.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if `session` is not a valid session name, so
/// the path never leaves `<home>/auth`.
pub(crate) fn path(home: &Path, session: &str) -> Result<PathBuf> {
    let session = session.trim().to_ascii_lowercase();
    check_session(&session)?;
    Ok(home.join(DIR).join(session).join(FILE))
}

/// The stored tokens of `session`, or `None` if it has none.
///
/// # Errors
///
/// Returns [`Error::Invalid`] for an invalid session name, and
/// [`Error::Io`] if the file cannot be read or is not a token file. The
/// error quotes nothing from the file.
pub(crate) async fn load(home: &Path, session: &str) -> Result<Option<Tokens>> {
    let path = path(home, session)?;
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(Error::Io { path, source }),
    };
    // serde's errors can quote the values they reject.
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| Error::Io {
            path,
            source: io::Error::new(io::ErrorKind::InvalidData, "not a valid token file"),
        })
}

/// Stores `tokens` as the tokens of `session`, atomically, creating its
/// directory with mode 0700 and the file with mode 0600 on Unix.
///
/// # Errors
///
/// Returns [`Error::Invalid`] for an invalid session name, and
/// [`Error::Io`] if the directory or the file cannot be written.
pub(crate) async fn save(home: &Path, session: &str, tokens: &Tokens) -> Result<()> {
    let path = path(home, session)?;
    let dir = path.parent().unwrap_or(home).to_owned();
    let io_error = |path: &Path| {
        let path = path.to_owned();
        move |source| Error::Io { path, source }
    };
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(&dir).await.map_err(io_error(&dir))?;
    // The directory may be older, with a wider mode.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::Permissions::from_mode(0o700);
        tokio::fs::set_permissions(&dir, mode)
            .await
            .map_err(io_error(&dir))?;
    }

    let json = serde_json::to_vec_pretty(tokens).map_err(|e| Error::Io {
        path: path.clone(),
        source: io::Error::new(io::ErrorKind::InvalidData, e),
    })?;
    let suffix = (random_fraction() * (1u64 << 53) as f64) as u64;
    let temp = dir.join(format!(".{FILE}.{suffix:x}.tmp"));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temp).await.map_err(io_error(&temp))?;
    let written = async {
        file.write_all(&json).await?;
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

/// Deletes the tokens of `session`, and its directory once it is empty.
/// Returns whether there were tokens.
///
/// # Errors
///
/// Returns [`Error::Invalid`] for an invalid session name, and
/// [`Error::Io`] if the file exists but cannot be deleted.
pub(crate) async fn delete(home: &Path, session: &str) -> Result<bool> {
    let path = path(home, session)?;
    match tokio::fs::remove_file(&path).await {
        Ok(()) => {
            if let Some(dir) = path.parent() {
                // Only an empty directory is removed.
                let _ = tokio::fs::remove_dir(dir).await;
            }
            Ok(true)
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(Error::Io { path, source }),
    }
}

/// Logs `session` out of the FICO home `home`: deletes its tokens. Returns
/// whether it had any.
///
/// Nothing is revoked at the identity provider, so its browser session
/// stays: a login with `force` starts a fresh one. Call it for the
/// [`ProfileSet::stale_session`](crate::config::ProfileSet) and the
/// [`ProfileDeleted::unused_session`](crate::config::ProfileDeleted) that
/// changing `config.yaml` reports, whose tokens no profile uses anymore.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if `session` is not a valid session name, and
/// [`Error::Io`] if its tokens cannot be deleted.
pub async fn logout(home: &Path, session: &str) -> Result<bool> {
    delete(home, session).await
}
