//! The SDK's own `uv`, which bundles a Python workspace's dependencies for
//! push and runs Python workspaces locally.
//!
//! The SDK needs no `uv` on the `PATH`. It pins one version,
//! [`UV_VERSION`], downloads that release from GitHub the first time it is
//! needed, checks it against the SHA-256 the SDK pins for the platform, and
//! keeps it in the FICO home, `<home>/bin/uv-<version>/`. [`install`] does
//! that ahead of time, for a host that wants to, and [`path`] says where it
//! is.
//!
//! On a machine that can't reach GitHub, set `FICO_UV` ([`UV_ENV`]) to a
//! `uv` binary, or give one to
//! [`Platform::with_uv`](crate::decision::platform::Platform::with_uv) and
//! [`Local::with_uv`](crate::decision::local::Local::with_uv).

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use aws_lc_rs::digest::{SHA256, digest};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};

use crate::rand::random_fraction;
use crate::{Client, Error, Result};

/// The version of `uv` that the SDK installs and runs.
pub const UV_VERSION: &str = "0.13.5";

/// The environment variable that names a `uv` binary to run instead of
/// the one the SDK installs, e.g. on a machine that can't reach GitHub. A
/// blank value counts as unset.
pub const UV_ENV: &str = "FICO_UV";

/// Where `uv`'s releases are, by version.
const RELEASES: &str = "https://github.com/astral-sh/uv/releases/download";

/// The read limit for downloading `uv`, about 15 MB.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// What [`install`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct UvInstalled {
    /// The `uv` binary.
    pub path: PathBuf,
    /// Its version, [`UV_VERSION`].
    pub version: String,
    /// Whether it was downloaded now, rather than installed already.
    pub downloaded: bool,
}

/// A `uv` release archive for one platform, and its SHA-256.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Asset {
    /// The platform's Rust target triple, which names the archive.
    pub(crate) target: &'static str,
    /// The archive's extension: `tar.gz`, or `zip` on Windows.
    pub(crate) extension: &'static str,
    /// The archive's SHA-256, from the release's `sha256.sum`.
    pub(crate) sha256: [u8; 32],
}

impl Asset {
    /// The archive's file name, e.g. `uv-aarch64-apple-darwin.tar.gz`.
    pub(crate) fn file_name(&self) -> String {
        format!("uv-{}.{}", self.target, self.extension)
    }
}

/// The archives of [`UV_VERSION`] for the platforms the SDK supports, with
/// the SHA-256s of its release's `sha256.sum`. Written as bytes, so that
/// secret scanners don't read the hashes as credentials. To move to a new
/// `uv`, change [`UV_VERSION`] and every hash here.
const ASSETS: [Asset; 8] = [
    Asset {
        target: "aarch64-apple-darwin",
        extension: "tar.gz",
        sha256: [
            0x74, 0x3f, 0x85, 0x03, 0x5a, 0x19, 0x96, 0xec, //
            0xb3, 0x50, 0x65, 0x9e, 0x01, 0xcd, 0x9f, 0xae, //
            0xa9, 0x70, 0x98, 0x29, 0xbe, 0x09, 0xb1, 0xb4, //
            0xb0, 0x3c, 0x73, 0xea, 0xf3, 0xc9, 0xcd, 0x73,
        ],
    },
    Asset {
        target: "x86_64-apple-darwin",
        extension: "tar.gz",
        sha256: [
            0xb0, 0xf5, 0x2b, 0x0e, 0x39, 0x45, 0xb4, 0x10, //
            0x7d, 0x89, 0x02, 0x4e, 0xa3, 0x8e, 0x62, 0x56, //
            0x83, 0x01, 0x1f, 0xbf, 0xc0, 0x5a, 0x0d, 0x98, //
            0xf9, 0xfa, 0x2d, 0x6d, 0xce, 0xe2, 0x8c, 0x66,
        ],
    },
    Asset {
        target: "aarch64-unknown-linux-gnu",
        extension: "tar.gz",
        sha256: [
            0x4b, 0xb6, 0x9c, 0xf9, 0x5e, 0xd1, 0x3f, 0x23, //
            0x71, 0x3b, 0xb7, 0xbc, 0x09, 0xf2, 0x41, 0x89, //
            0x6e, 0xa0, 0xc7, 0x5b, 0x7c, 0x55, 0x7e, 0x61, //
            0x5a, 0x20, 0x60, 0x4b, 0x82, 0xf9, 0xc4, 0x03,
        ],
    },
    Asset {
        target: "x86_64-unknown-linux-gnu",
        extension: "tar.gz",
        sha256: [
            0xf6, 0x8d, 0x29, 0x1c, 0x75, 0x78, 0x31, 0x22, //
            0xda, 0xd5, 0x8a, 0xb7, 0x7d, 0xef, 0x0e, 0x13, //
            0xf3, 0x23, 0x3f, 0x35, 0x87, 0x31, 0xdb, 0x5d, //
            0x6d, 0x7e, 0x87, 0x15, 0x60, 0x3b, 0x92, 0x8f,
        ],
    },
    Asset {
        target: "aarch64-unknown-linux-musl",
        extension: "tar.gz",
        sha256: [
            0x79, 0x06, 0xc7, 0x9a, 0xdb, 0x21, 0xc8, 0x1a, //
            0xbd, 0x55, 0x98, 0x41, 0xba, 0x76, 0xbb, 0x1d, //
            0x9a, 0xf9, 0xd7, 0x91, 0xac, 0xac, 0x7a, 0x04, //
            0x1b, 0x29, 0x72, 0xe0, 0x3a, 0xe1, 0x65, 0x38,
        ],
    },
    Asset {
        target: "x86_64-unknown-linux-musl",
        extension: "tar.gz",
        sha256: [
            0x57, 0xef, 0x9d, 0x9c, 0xcb, 0x01, 0xfb, 0x1b, //
            0xd9, 0x98, 0xdf, 0x99, 0x5b, 0x0d, 0xd2, 0x21, //
            0xb5, 0x2f, 0x7d, 0x78, 0x41, 0xfb, 0xea, 0x93, //
            0xf2, 0xd4, 0x46, 0x3b, 0xf6, 0x1f, 0xb7, 0xa9,
        ],
    },
    Asset {
        target: "aarch64-pc-windows-msvc",
        extension: "zip",
        sha256: [
            0x09, 0xf4, 0x7e, 0xdf, 0x90, 0x72, 0x60, 0x84, //
            0xa7, 0x0f, 0x94, 0x26, 0xf6, 0xe3, 0xca, 0x4f, //
            0x46, 0xe9, 0xbf, 0x7c, 0xbf, 0x23, 0xb1, 0x71, //
            0x6e, 0xc6, 0x4d, 0x60, 0xb0, 0x55, 0xfa, 0xe3,
        ],
    },
    Asset {
        target: "x86_64-pc-windows-msvc",
        extension: "zip",
        sha256: [
            0xa3, 0x8b, 0x8d, 0x19, 0x02, 0xb7, 0xcf, 0xbc, //
            0x76, 0xb4, 0x23, 0xba, 0x1b, 0xb4, 0x56, 0xf6, //
            0x23, 0xf4, 0x20, 0x64, 0xcd, 0xc7, 0x26, 0x28, //
            0xb6, 0x07, 0x5b, 0x89, 0xaf, 0xd1, 0x01, 0x9a,
        ],
    },
];

/// The Rust target triple of `uv`'s release for this machine, from what
/// the SDK was compiled for, or `None` where `uv` publishes no release the
/// SDK pins.
pub(crate) fn this_target() -> Option<&'static str> {
    let musl = cfg!(target_env = "musl");
    let target = match (std::env::consts::OS, std::env::consts::ARCH, musl) {
        ("macos", "aarch64", _) => "aarch64-apple-darwin",
        ("macos", "x86_64", _) => "x86_64-apple-darwin",
        ("linux", "aarch64", false) => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64", false) => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64", true) => "aarch64-unknown-linux-musl",
        ("linux", "x86_64", true) => "x86_64-unknown-linux-musl",
        ("windows", "aarch64", _) => "aarch64-pc-windows-msvc",
        ("windows", "x86_64", _) => "x86_64-pc-windows-msvc",
        _ => return None,
    };
    Some(target)
}

/// The `uv` binary's file name on this machine.
fn binary_name() -> &'static str {
    if cfg!(windows) { "uv.exe" } else { "uv" }
}

/// Where the SDK keeps [`UV_VERSION`] in the FICO home `home`.
fn install_dir(home: &Path) -> PathBuf {
    home.join("bin").join(format!("uv-{UV_VERSION}"))
}

/// The `uv` binary that the SDK installed in the FICO home `home`, if
/// [`install`] put one there. Does no I/O beyond checking that the file
/// exists.
pub fn path(home: &Path) -> Option<PathBuf> {
    let path = install_dir(home).join(binary_name());
    path.is_file().then_some(path)
}

/// Installs [`UV_VERSION`] of `uv` into the FICO home `home`, unless it is
/// there already, and returns where it is.
///
/// It downloads `uv`'s release for this platform from GitHub, checks it
/// against the SHA-256 the SDK pins before it unpacks anything, and puts
/// the `uv` binary in `<home>/bin/uv-<version>/`. Installs that run at the
/// same time, from several processes too, leave one working binary.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if `uv` publishes no release for this
/// platform (set `FICO_UV` then), or the download doesn't match its
/// SHA-256 or holds no `uv`, [`Error::Io`] if it can't be written, and the
/// transport's errors if the download fails.
pub async fn install(home: &Path) -> Result<UvInstalled> {
    install_from(home, &Source::default(), this_asset()?).await
}

/// The pinned archive for this machine.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if `uv` publishes no release for it.
fn this_asset() -> Result<Asset> {
    let target = this_target().ok_or_else(|| Error::Invalid {
        reason: format!(
            "`uv` publishes no release for this platform ({}, {}): install `uv` and set \
             `{UV_ENV}` to it",
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    })?;
    ASSETS
        .iter()
        .find(|asset| asset.target == target)
        .copied()
        .ok_or_else(|| Error::Invalid {
            reason: format!("the SDK pins no `uv` for `{target}`: set `{UV_ENV}`"),
        })
}

/// Where [`install_from`] downloads `uv` from: GitHub's releases, or a
/// test server with an archive of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Source {
    /// The URL of the release directory of [`UV_VERSION`].
    pub(crate) base_url: String,
    /// The archive to download, if not this machine's pinned one.
    pub(crate) asset: Option<Asset>,
}

impl Default for Source {
    fn default() -> Source {
        Source {
            base_url: format!("{RELEASES}/{UV_VERSION}"),
            asset: None,
        }
    }
}

/// [`install`], with `asset` from `source`.
pub(crate) async fn install_from(
    home: &Path,
    source: &Source,
    asset: Asset,
) -> Result<UvInstalled> {
    let dir = install_dir(home);
    let binary = dir.join(binary_name());
    let installed = |downloaded| UvInstalled {
        path: binary.clone(),
        version: UV_VERSION.to_owned(),
        downloaded,
    };
    if binary.is_file() {
        return Ok(installed(false));
    }
    let archive = download(&format!("{}/{}", source.base_url, asset.file_name())).await?;
    let actual = digest(&SHA256, &archive);
    if actual.as_ref() != asset.sha256 {
        return Err(Error::Invalid {
            reason: format!(
                "the download of `{}` doesn't match the SHA-256 the SDK pins for it",
                asset.file_name()
            ),
        });
    }
    let name = binary_name();
    let contents = blocking(dir.clone(), move || unpack(&archive, asset, name)).await?;
    let (dir_for_write, binary_for_write) = (dir.clone(), binary.clone());
    blocking(dir, move || {
        place(&dir_for_write, &binary_for_write, &contents)
    })
    .await?;
    Ok(installed(true))
}

/// The `uv` binary named `name` in `archive`, `uv`'s release archive for
/// `asset`: `<archive name>/uv` in a tar.gz, or `uv.exe` in a zip.
fn unpack(archive: &[u8], asset: Asset, name: &str) -> Result<Vec<u8>> {
    let invalid = |problem: String| Error::Invalid {
        reason: format!("`{}` {problem}", asset.file_name()),
    };
    if asset.extension == "zip" {
        return crate::zip::extract(archive, name)
            .map_err(|problem| invalid(format!("is not a valid zip archive: {problem}")))?
            .ok_or_else(|| invalid(format!("holds no `{name}`")));
    }
    let wanted = Path::new(&format!("uv-{}", asset.target)).join(name);
    let mut tar = tar::Archive::new(GzDecoder::new(archive));
    let malformed = |error: io::Error| invalid(format!("is not a valid tar.gz archive: {error}"));
    for entry in tar.entries().map_err(malformed)? {
        let mut entry = entry.map_err(malformed)?;
        if entry.header().entry_type().is_file() && entry.path().map_err(malformed)? == wanted {
            let mut contents = Vec::new();
            entry.read_to_end(&mut contents).map_err(malformed)?;
            return Ok(contents);
        }
    }
    Err(invalid(format!("holds no `{}`", wanted.display())))
}

/// Writes `contents` as the executable `binary` in `dir`, through a staging
/// file and a rename, so that a reader never sees a partial binary. A
/// binary that another install put there first is kept.
fn place(dir: &Path, binary: &Path, contents: &[u8]) -> Result<()> {
    fs::create_dir_all(dir).map_err(io_error(dir))?;
    let staging = dir.join(format!(
        ".{}.tmp-{:013x}",
        binary_name(),
        (random_fraction() * (1u64 << 52) as f64) as u64
    ));
    let written = write_executable(&staging, contents).and_then(|()| {
        match fs::rename(&staging, binary) {
            // Windows can't replace a binary another install put there and
            // that is running.
            Err(_) if binary.is_file() => Ok(()),
            renamed => renamed,
        }
    });
    if written.is_err() {
        let _ = fs::remove_file(&staging);
    }
    written.map_err(io_error(binary))
}

/// Writes `contents` to the new file `path`, executable by everyone and
/// writable only by this user (mode 0755 on Unix).
fn write_executable(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o755);
    }
    let mut file = options.open(path)?;
    io::Write::write_all(&mut file, contents)?;
    file.sync_all()
}

/// The body of `url`, a public download: no token, a generous read limit,
/// and, unlike the SDK's own requests, a redirect to another origin is
/// followed once, as GitHub sends its release downloads to a CDN.
async fn download(url: &str) -> Result<Vec<u8>> {
    let client = Client::builder(url)
        .read_timeout(DOWNLOAD_TIMEOUT)
        .build()?;
    match client.get(url).no_auth().send().await {
        Ok(response) => Ok(response.into_bytes()),
        Err(Error::Redirect { location, .. }) => {
            let client = Client::builder(&location)
                .read_timeout(DOWNLOAD_TIMEOUT)
                .build()?;
            let response = client.get(&location).no_auth().send().await?;
            Ok(response.into_bytes())
        }
        Err(error) => Err(error),
    }
}

/// The `uv` binary to run, in this order: `explicit`, the one the caller
/// set; `FICO_UV`, if set and not blank; else the SDK's own in the FICO
/// home `home`, installed now if needed, in which case `on_install` is
/// called first, e.g. to report it.
///
/// # Errors
///
/// Returns the errors of [`install`].
pub(crate) async fn resolve(
    explicit: Option<&Path>,
    home: impl FnOnce() -> Result<PathBuf>,
    on_install: impl FnOnce(),
) -> Result<OsString> {
    resolve_from(explicit, home, on_install, &Source::default()).await
}

/// [`resolve`], installing from `source`.
pub(crate) async fn resolve_from(
    explicit: Option<&Path>,
    home: impl FnOnce() -> Result<PathBuf>,
    on_install: impl FnOnce(),
    source: &Source,
) -> Result<OsString> {
    let from_env = std::env::var_os(UV_ENV);
    resolve_with(explicit, from_env, home, on_install, source).await
}

/// [`resolve_from`], with `from_env` as the value of `FICO_UV`, as tests
/// can't set environment variables (`set_var` is `unsafe`).
async fn resolve_with(
    explicit: Option<&Path>,
    from_env: Option<OsString>,
    home: impl FnOnce() -> Result<PathBuf>,
    on_install: impl FnOnce(),
    source: &Source,
) -> Result<OsString> {
    if let Some(explicit) = explicit {
        return Ok(explicit.as_os_str().to_owned());
    }
    if let Some(from_env) = from_env.filter(|value| !is_blank(value)) {
        return Ok(from_env);
    }
    let home = home()?;
    if let Some(installed) = path(&home) {
        return Ok(installed.into_os_string());
    }
    on_install();
    let asset = match source.asset {
        Some(asset) => asset,
        None => this_asset()?,
    };
    Ok(install_from(&home, source, asset)
        .await?
        .path
        .into_os_string())
}

/// Whether `value` is empty or only whitespace.
fn is_blank(value: &std::ffi::OsStr) -> bool {
    value.to_str().is_some_and(|value| value.trim().is_empty())
}

/// Runs `f` on tokio's blocking pool. A panic in `f` resumes here. `path`
/// names the error if the runtime shuts down before `f` runs.
async fn blocking<T, F>(path: PathBuf, f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => Err(Error::Io {
            path,
            source: io::Error::other(error),
        }),
    }
}

/// Turns an I/O error on `path` into [`Error::Io`].
fn io_error(path: &Path) -> impl FnOnce(io::Error) -> Error {
    let path = path.to_path_buf();
    move |source| Error::Io { path, source }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use flate2::Compression;
    use flate2::write::GzEncoder;

    use super::*;
    use crate::http::mock_server::{MockResponse, MockServer};
    use crate::zip::tests::{Entry, zip};

    /// A new directory under the system's temp directory, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> TempDir {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("fico-sdk-uv-{}-{n}", std::process::id()));
            // Left over from an earlier test process with the same ID.
            let _ = fs::remove_dir_all(&path);
            fs::create_dir(&path).unwrap();
            TempDir(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A tar.gz laid out as `uv`'s for `target`: a directory named after it
    /// with `uv` and `uvx` in it.
    pub(crate) fn tar_gz(target: &str, uv: &[u8]) -> Vec<u8> {
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
        for (name, contents) in [("uvx", b"uvx".as_slice()), ("uv", uv)] {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            let path = format!("uv-{target}/{name}");
            builder.append_data(&mut header, path, contents).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    /// An asset for `target` whose SHA-256 is that of `archive`.
    pub(crate) fn asset(target: &'static str, extension: &'static str, archive: &[u8]) -> Asset {
        let mut sha256 = [0; 32];
        sha256.copy_from_slice(digest(&SHA256, archive).as_ref());
        Asset {
            target,
            extension,
            sha256,
        }
    }

    /// A server that serves `archive` `times` times, and the source for it.
    async fn serve(archive: Vec<u8>, times: usize) -> (MockServer, Source) {
        let responses = (0..times)
            .map(|_| MockResponse::new(200).body(archive.clone()))
            .collect();
        let server = MockServer::start(responses).await;
        let source = Source {
            base_url: format!("{}/{UV_VERSION}", server.url()),
            asset: None,
        };
        (server, source)
    }

    #[test]
    fn pins_a_release_for_every_supported_target() {
        assert!(this_target().is_some(), "the test machine is supported");
        for asset in ASSETS {
            assert_eq!(
                asset.extension,
                if asset.target.contains("windows") {
                    "zip"
                } else {
                    "tar.gz"
                },
                "{}",
                asset.target
            );
        }
        let targets: Vec<&str> = ASSETS.iter().map(|asset| asset.target).collect();
        assert!(targets.contains(&this_target().unwrap()));
        assert_eq!(ASSETS[0].file_name(), "uv-aarch64-apple-darwin.tar.gz");
        assert_eq!(
            Source::default().base_url,
            "https://github.com/astral-sh/uv/releases/download/0.13.5"
        );
    }

    #[tokio::test]
    async fn installs_uv_from_a_tar_gz_once() {
        let home = TempDir::new();
        assert_eq!(path(&home.0), None);
        let archive = tar_gz("aarch64-apple-darwin", b"the uv binary");
        let asset = asset("aarch64-apple-darwin", "tar.gz", &archive);
        let (server, source) = serve(archive, 1).await;

        let installed = install_from(&home.0, &source, asset).await.unwrap();
        let expected = home.0.join("bin/uv-0.13.5").join(binary_name());
        assert_eq!(installed.path, expected);
        assert_eq!(installed.version, "0.13.5");
        assert!(installed.downloaded);
        assert_eq!(fs::read(&expected).unwrap(), b"the uv binary");
        assert_eq!(path(&home.0), Some(expected.clone()));
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].target, "/0.13.5/uv-aarch64-apple-darwin.tar.gz");
        assert_eq!(requests[0].header("authorization"), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&expected).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
        // Only the binary, no staging file.
        let names: Vec<_> = fs::read_dir(home.0.join("bin/uv-0.13.5"))
            .unwrap()
            .collect();
        assert_eq!(names.len(), 1);

        // Installed already: no second download.
        let again = install_from(&home.0, &source, asset).await.unwrap();
        assert!(!again.downloaded);
        assert_eq!(server.requests().len(), 1);
    }

    #[tokio::test]
    async fn installs_uv_from_a_zip() {
        let home = TempDir::new();
        let archive = zip(
            &[
                Entry {
                    name: "uvx.exe",
                    contents: b"uvx",
                    deflate: true,
                    flags: 0,
                },
                Entry {
                    name: "uv.exe",
                    contents: b"the uv binary",
                    deflate: true,
                    flags: 0,
                },
                Entry {
                    name: "uv",
                    contents: b"the uv binary",
                    deflate: true,
                    flags: 0,
                },
            ],
            b"",
        );
        let asset = asset("x86_64-pc-windows-msvc", "zip", &archive);
        let (_server, source) = serve(archive, 1).await;
        let installed = install_from(&home.0, &source, asset).await.unwrap();
        assert_eq!(fs::read(&installed.path).unwrap(), b"the uv binary");
    }

    #[tokio::test]
    async fn refuses_a_download_that_does_not_match_its_hash() {
        let home = TempDir::new();
        let archive = tar_gz("aarch64-apple-darwin", b"the uv binary");
        let mut pinned = asset("aarch64-apple-darwin", "tar.gz", &archive);
        pinned.sha256[0] ^= 1;
        let (_server, source) = serve(archive, 1).await;
        let err = install_from(&home.0, &source, pinned).await.unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
        assert!(err.to_string().contains("SHA-256"), "{err}");
        assert_eq!(path(&home.0), None);
        assert!(!home.0.join("bin").exists());
    }

    #[tokio::test]
    async fn refuses_an_archive_without_uv() {
        let home = TempDir::new();
        // The archive of another target.
        let archive = tar_gz("x86_64-apple-darwin", b"the uv binary");
        let asset = asset("aarch64-apple-darwin", "tar.gz", &archive);
        let (_server, source) = serve(archive, 1).await;
        let err = install_from(&home.0, &source, asset).await.unwrap_err();
        assert!(err.to_string().contains("holds no"), "{err}");
        assert_eq!(path(&home.0), None);
    }

    #[tokio::test]
    async fn reports_a_failed_download() {
        let home = TempDir::new();
        let server = MockServer::start(vec![MockResponse::new(404)]).await;
        let source = Source {
            base_url: server.url().to_owned(),
            asset: None,
        };
        let asset = asset("aarch64-apple-darwin", "tar.gz", b"");
        let err = install_from(&home.0, &source, asset).await.unwrap_err();
        assert!(matches!(err, Error::Status { status: 404, .. }), "{err:?}");
    }

    #[tokio::test]
    async fn follows_a_redirect_to_another_origin() {
        let home = TempDir::new();
        let archive = tar_gz("aarch64-apple-darwin", b"the uv binary");
        let asset = asset("aarch64-apple-darwin", "tar.gz", &archive);
        let (cdn, _) = serve(archive, 1).await;
        let location = format!("{}/asset?signature=abc", cdn.url());
        let github =
            MockServer::start(vec![MockResponse::new(302).header("location", &location)]).await;
        let source = Source {
            base_url: github.url().to_owned(),
            asset: None,
        };
        install_from(&home.0, &source, asset).await.unwrap();
        assert_eq!(cdn.requests()[0].target, "/asset?signature=abc");
    }

    #[tokio::test]
    async fn picks_the_set_uv_then_fico_uv_then_its_own() {
        let home = TempDir::new();
        // A download fails fast, so the test sees an install was tried.
        let server = MockServer::start(vec![MockResponse::new(404)]).await;
        let unreachable = Source {
            base_url: server.url().to_owned(),
            asset: None,
        };
        let installs = std::cell::Cell::new(0);
        let resolve = async |explicit: Option<&Path>, from_env: Option<&str>| {
            let home = || Ok(home.0.clone());
            let on_install = || installs.set(installs.get() + 1);
            let from_env = from_env.map(OsString::from);
            resolve_with(explicit, from_env, home, on_install, &unreachable).await
        };

        let set = Path::new("/opt/uv/bin/uv");
        assert_eq!(resolve(Some(set), Some("/env/uv")).await.unwrap(), set);
        assert_eq!(resolve(None, Some("/env/uv")).await.unwrap(), "/env/uv");
        // Blank counts as unset, and then the SDK's own is installed.
        let err = resolve(None, Some("  ")).await.unwrap_err();
        assert!(matches!(err, Error::Status { status: 404, .. }), "{err:?}");
        assert_eq!(installs.get(), 1);
        assert_eq!(server.requests().len(), 1);

        // Once installed, it is used without a download.
        let installed = install_dir(&home.0).join(binary_name());
        place(&install_dir(&home.0), &installed, b"uv").unwrap();
        assert_eq!(resolve(None, None).await.unwrap(), installed.as_os_str());
        assert_eq!(installs.get(), 1);
    }

    #[test]
    fn keeps_a_binary_another_install_placed_first() {
        let home = TempDir::new();
        let dir = install_dir(&home.0);
        let binary = dir.join(binary_name());
        place(&dir, &binary, b"first").unwrap();
        place(&dir, &binary, b"second").unwrap();
        // The rename replaces it atomically on Unix, so either is a whole
        // binary; on Windows the first stays.
        let contents = fs::read(&binary).unwrap();
        assert!(
            contents == b"first" || contents == b"second",
            "{contents:?}"
        );
        let names: Vec<_> = fs::read_dir(&dir).unwrap().collect();
        assert_eq!(names.len(), 1, "no staging file left");
    }

    #[tokio::test]
    async fn installs_once_when_installs_race() {
        let home = TempDir::new();
        let archive = tar_gz("aarch64-apple-darwin", b"the uv binary");
        let asset = asset("aarch64-apple-darwin", "tar.gz", &archive);
        let (_server, source) = serve(archive, 4).await;
        // Separate tasks, so the installs run in parallel.
        let tasks: Vec<_> = (0..4)
            .map(|_| {
                let (home, source) = (home.0.clone(), source.clone());
                tokio::spawn(async move { install_from(&home, &source, asset).await })
            })
            .collect();
        for task in tasks {
            let installed = task.await.unwrap().unwrap();
            assert_eq!(fs::read(installed.path).unwrap(), b"the uv binary");
        }
        let names: Vec<_> = fs::read_dir(install_dir(&home.0)).unwrap().collect();
        assert_eq!(names.len(), 1);
    }
}
