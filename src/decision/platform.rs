//! The `decision platform` capability: operations on the FICO® platform.
//!
//! [`Platform`] is the only type with behavior. Every operation is a method
//! on it, defined in the file of its noun (`platform/run.rs` holds the run
//! methods), and returns plain data from [`decision`](super). The
//! parameters of each operation are re-exported here.

mod acceptance;
mod account;
mod build;
mod console;
mod data;
mod ensemble;
mod environment;
mod input_set;
mod integration;
mod managed_input;
mod run;
mod run_asset;
mod run_logs;
mod scenario;
mod secrets;
mod shadow;
mod sso;
mod switchback;
mod upload;
mod workspace;

pub use self::acceptance::*;
pub use self::account::*;
pub use self::build::*;
pub use self::console::*;
pub use self::data::*;
pub use self::ensemble::*;
pub use self::environment::*;
pub use self::input_set::*;
pub use self::integration::*;
pub use self::managed_input::*;
pub use self::run::*;
pub use self::run_asset::*;
pub use self::run_logs::*;
pub use self::scenario::*;
pub use self::secrets::*;
pub use self::shadow::*;
pub use self::sso::*;
pub use self::switchback::*;
pub use self::workspace::*;

use std::path::PathBuf;

use crate::Client;
use crate::tool::Tool;

/// Entry point for the `decision platform` capability. Build one with
/// [`Platform::new`].
///
/// Cloning a `Platform` is cheap: clones share the [`Client`] and its
/// connection pool.
///
/// # Errors
///
/// Every method that sends requests fails as [`RequestBuilder::send`]
/// does: with [`Error::Status`] when the platform answers 4xx or 5xx, such
/// as 404 when a resource does not exist, with [`Error::Connect`],
/// [`Error::Timeout`] or [`Error::Request`] when a request fails after all
/// retries, and with [`Error::Redirect`]. An answer that is not what the
/// method expects is [`Error::Decode`], and a malformed page of a list is
/// [`Error::Pagination`]. An ID that is empty, `.` or `..`, or that
/// contains `/` or `\`, is [`Error::Invalid`], before any request. A client with a [`TokenSource`]
/// also fails with its errors, such as [`Error::NotLoggedIn`] for a PKCE
/// session with no usable tokens. The `# Errors` section of each method
/// adds its own errors to these.
///
/// [`RequestBuilder::send`]: crate::RequestBuilder::send
/// [`TokenSource`]: crate::TokenSource
/// [`Error::Status`]: crate::Error::Status
/// [`Error::Connect`]: crate::Error::Connect
/// [`Error::Timeout`]: crate::Error::Timeout
/// [`Error::Request`]: crate::Error::Request
/// [`Error::Redirect`]: crate::Error::Redirect
/// [`Error::Decode`]: crate::Error::Decode
/// [`Error::Pagination`]: crate::Error::Pagination
/// [`Error::Invalid`]: crate::Error::Invalid
/// [`Error::NotLoggedIn`]: crate::Error::NotLoggedIn
#[derive(Debug, Clone)]
pub struct Platform {
    client: Client,
    /// Where push reports its progress, if the caller set a sink with
    /// [`Platform::with_push_progress`].
    push_progress: Option<PushProgress>,
    /// The console's base URL, without a trailing `/`, for the links of
    /// [`Platform::run_console_url`] and [`Platform::workspace_console_url`].
    console_url: Option<String>,
    /// The FICO home whose dependency cache and `uv` push uses, if the
    /// caller set one with [`Platform::with_home`].
    home: Option<PathBuf>,
    /// The `uv` binary push runs, if the caller set one with
    /// [`Platform::with_uv`].
    uv: Option<PathBuf>,
}

impl Platform {
    /// Creates the platform root, which sends every request with `client`.
    /// Its console base URL is [`DEFAULT_CONSOLE_URL`]. Makes no request.
    pub fn new(client: Client) -> Self {
        Self {
            client,
            push_progress: None,
            console_url: DEFAULT_CONSOLE_URL.and_then(console::base_url),
            home: None,
            uv: None,
        }
    }

    /// Reports the progress of [`Platform::push_workspace`] to `progress`.
    /// Without a sink, nothing is reported.
    pub fn with_push_progress(mut self, progress: PushProgress) -> Platform {
        self.push_progress = Some(progress);
        self
    }

    /// Links runs and workspaces to the console at `url`, e.g.
    /// `https://console.example.com`, instead of [`DEFAULT_CONSOLE_URL`]:
    /// the base of [`Platform::run_console_url`] and
    /// [`Platform::workspace_console_url`], and of the `console_url` that
    /// [`Platform::get_run`] and [`Platform::get_run_information`] fill.
    /// Trailing `/`s are dropped, and an empty `url` means no links.
    pub fn with_console_url(mut self, url: impl Into<String>) -> Platform {
        self.console_url = console::base_url(&url.into());
        self
    }

    /// Makes [`Platform::push_workspace`] use the dependency cache of the
    /// FICO home `home`, in `<home>/cache/deps`, and the SDK's `uv` in it
    /// ([`uv::install`](crate::uv::install)). Give it the home that the
    /// host passes to [`Local::with_home`](super::local::Local::with_home)
    /// and [`cache::info`](crate::cache::info), so that they share them.
    /// Without one, push uses [`config::home_dir`](crate::config::home_dir).
    /// Does no I/O.
    pub fn with_home(mut self, home: impl Into<PathBuf>) -> Platform {
        self.home = Some(home.into());
        self
    }

    /// Makes [`Platform::push_workspace`] bundle Python dependencies with
    /// the `uv` binary at `uv`, instead of `FICO_UV` or the SDK's own,
    /// which it installs in the FICO home ([`crate::uv`]). Does no I/O.
    pub fn with_uv(mut self, uv: impl Into<PathBuf>) -> Platform {
        self.uv = Some(uv.into());
        self
    }
}

/// Every `decision_platform_*` tool.
pub(crate) fn tools() -> Vec<Tool> {
    [
        acceptance::tools(),
        account::tools(),
        build::tools(),
        data::tools(),
        ensemble::tools(),
        environment::tools(),
        input_set::tools(),
        integration::tools(),
        managed_input::tools(),
        run::tools(),
        run_asset::tools(),
        scenario::tools(),
        secrets::tools(),
        shadow::tools(),
        sso::tools(),
        switchback::tools(),
        upload::tools(),
        workspace::tools(),
    ]
    .into_iter()
    .flatten()
    .collect()
}
