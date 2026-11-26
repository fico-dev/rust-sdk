//! Uploads to presigned URLs.

use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::Url;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::Platform;
use crate::decision::archive;
use crate::tool::Tool;
use crate::{Error, Result};

/// Read limit for uploading to a presigned URL: the transport gives sending
/// the body and waiting for the answer this limit each. The platform's
/// upload URLs are valid for 10 minutes.
pub(super) const UPLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// The slowest upload, in bytes per second, that [`upload_timeout`] leaves
/// time for: 128 KiB/s, about 1 Mbit/s.
const MIN_UPLOAD_RATE: u64 = 128 * 1024;

/// The read limit for uploading `len` bytes to a presigned URL:
/// [`UPLOAD_TIMEOUT`], or the time to send them at [`MIN_UPLOAD_RATE`] if
/// that is longer. The transport sends the body within the limit on the
/// response head, so a fixed limit would fail a large upload on a slow link
/// because of its size.
pub(super) fn upload_timeout(len: usize) -> Duration {
    let len = u64::try_from(len).unwrap_or(u64::MAX);
    UPLOAD_TIMEOUT.max(Duration::from_secs(len / MIN_UPLOAD_RATE))
}

/// Parameters for [`Platform::upload_data`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
pub struct DataUploadParams {
    /// Presigned URL to upload to: the `upload_url` of a new upload URL.
    pub upload_url: String,
    /// Data to upload, as a JSON value, sent as compact JSON. Set exactly one
    /// of `data`, `data_file` and `data_dir`.
    #[serde(default)]
    pub data: Option<Value>,
    /// File to upload, such as a JSON file or a tar.gz of input files. Its
    /// bytes are sent as they are. Set exactly one of `data`, `data_file` and
    /// `data_dir`.
    #[serde(default)]
    pub data_file: Option<PathBuf>,
    /// Directory whose files to upload, sent as a tar.gz, for multi-file
    /// input. Set exactly one of `data`, `data_file` and `data_dir`.
    #[serde(default)]
    pub data_dir: Option<PathBuf>,
    /// Bytes to upload as they are, instead of `data`, `data_file` or
    /// `data_dir`. Not a tool argument.
    #[serde(skip)]
    #[schemars(skip)]
    pub bytes: Option<Vec<u8>>,
}

impl Platform {
    /// Uploads data or files to an upload URL.
    ///
    /// A directory is sent as a tar.gz. The URL is presigned and on another
    /// origin, so the platform's bearer token is not sent to it.
    ///
    /// The upload uses a read limit of 10 minutes, whatever the client's, or
    /// longer for a body that takes more than that to send at 128 KiB/s:
    /// one more minute per 7.5 MiB. With the transport's limit on the
    /// response head (see [`ClientBuilder`'s time
    /// limits](crate::ClientBuilder#time-limits)), which also covers sending
    /// the body, an attempt can take about twice that, and attempts are
    /// retried as the client's [`RetryPolicy`](crate::RetryPolicy) says. A
    /// whole-attempt limit set on the client
    /// ([`ClientBuilder::timeout`](crate::ClientBuilder::timeout)) still
    /// applies. The upload URL expires 10 minutes after the platform issues
    /// it. Storage checks that when an upload starts, so an upload that
    /// started in time completes, but a retry after it fails with
    /// [`Error::Status`] (403).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `params.upload_url` is not an absolute
    /// `http` or `https` URL, if `params` does not set exactly one of
    /// `data`, `data_file`, `data_dir` and `bytes`, or if `data_dir` is not a
    /// directory, and [`Error::Io`] if a file cannot be read.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn upload_data(&self, params: DataUploadParams) -> Result<()> {
        check_upload_url(&params.upload_url)?;
        let file = set(params.data_file.as_deref());
        let dir = set(params.data_dir.as_deref());
        let body = match (params.data, file, dir, params.bytes) {
            (Some(data), None, None, None) => data.to_string().into_bytes(),
            (None, Some(file), None, None) => read_file(file).await?,
            (None, None, Some(dir), None) => pack_dir(dir, "data_dir").await?,
            (None, None, None, Some(bytes)) => bytes,
            _ => {
                return Err(Error::Invalid {
                    reason: "set exactly one of `data`, `data_file` and `data_dir`".to_owned(),
                });
            }
        };
        self.put_upload(&params.upload_url, body).await
    }

    /// PUTs `body` to the presigned `upload_url`, with no bearer token and
    /// no `Content-Type`.
    pub(super) async fn put_upload(&self, upload_url: &str, body: Vec<u8>) -> Result<()> {
        check_upload_url(upload_url)?;
        self.client
            .put(upload_url)
            .no_auth()
            .read_timeout(upload_timeout(body.len()))
            .body(body)
            .send()
            .await?;
        Ok(())
    }
}

/// `path`, unless it is unset or empty.
pub(super) fn set(path: Option<&Path>) -> Option<&Path> {
    path.filter(|path| !path.as_os_str().is_empty())
}

/// Reads the whole file at `path`.
pub(super) async fn read_file(path: &Path) -> Result<Vec<u8>> {
    tokio::fs::read(path).await.map_err(|source| Error::Io {
        path: path.to_owned(),
        source,
    })
}

/// Packs the files under `dir`, the parameter `field`, into a tar.gz.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if `dir` is not a directory, and [`Error::Io`]
/// if it cannot be read.
pub(super) async fn pack_dir(dir: &Path, field: &str) -> Result<Vec<u8>> {
    let metadata = tokio::fs::metadata(dir).await.map_err(|source| Error::Io {
        path: dir.to_owned(),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(Error::Invalid {
            reason: format!(
                "`{field}` must be a directory, and `{}` is not one",
                dir.display()
            ),
        });
    }
    archive::pack_dir(dir.to_owned()).await
}

/// Checks that `upload_url` is an absolute `http` or `https` URL. The
/// client would resolve anything else against the platform's base URL.
fn check_upload_url(upload_url: &str) -> Result<()> {
    let absolute = upload_url.split_once("://").is_some_and(|(scheme, _)| {
        scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http")
    }) && Url::parse(upload_url).is_ok_and(|url| url.has_host());
    if absolute {
        return Ok(());
    }
    Err(Error::Invalid {
        reason: "`upload_url` must be an absolute `http` or `https` URL".to_owned(),
    })
}

pub(crate) fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "decision_platform_data_upload",
            "Uploads data or files to an upload URL.",
            |cx, params: DataUploadParams| async move {
                cx.local()?;
                cx.platform()?.upload_data(params).await
            },
        )
        .local_only(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gives_large_uploads_more_time() {
        let ten_minutes = Duration::from_secs(600);
        assert_eq!(upload_timeout(0), ten_minutes);
        // 75 MiB take 10 minutes at 128 KiB/s.
        assert_eq!(upload_timeout(75 * 1024 * 1024), ten_minutes);
        assert_eq!(
            upload_timeout(75 * 1024 * 1024 + 128 * 1024),
            Duration::from_secs(601)
        );
        assert_eq!(upload_timeout(500 * 1024 * 1024), Duration::from_secs(4000));
        assert_eq!(
            upload_timeout(usize::MAX),
            Duration::from_secs(u64::MAX / (128 * 1024))
        );
    }
}
