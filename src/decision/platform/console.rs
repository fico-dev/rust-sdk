//! Links to the console. No tool serves them: they make no request, and
//! the run reads put the run's link in `console_url`.

use super::Platform;
use crate::decision::path::api_path;

/// The console's base URL that [`Platform::new`] starts from, whatever the
/// client's endpoint: `https://cloud.nextmv.io`, Nextmv's, until FICO has
/// its own.
///
/// It is written without a trailing `/`. Runs and workspaces on every
/// endpoint link to that console, unless the caller sets another base with
/// [`Platform::with_console_url`], or none with `with_console_url("")`.
pub const DEFAULT_CONSOLE_URL: Option<&str> = Some("https://cloud.nextmv.io");

impl Platform {
    /// The console link of a run,
    /// `<base>/app/<workspace_id>/run/<run_id>?view=details`. Each ID is
    /// percent-encoded, as in request paths. Makes no request.
    ///
    /// Returns `None` without a console base URL
    /// ([`Platform::with_console_url`]), or if an ID is empty, `.` or `..`,
    /// or contains `/` or `\`.
    pub fn run_console_url(&self, workspace_id: &str, run_id: &str) -> Option<String> {
        let link = self.console_link(&["app", workspace_id, "run", run_id])?;
        Some(link + "?view=details")
    }

    /// The console link of a workspace's runs,
    /// `<base>/app/<workspace_id>/runs`. The ID is percent-encoded, as in
    /// request paths. Makes no request.
    ///
    /// Returns `None` without a console base URL
    /// ([`Platform::with_console_url`]), or if the ID is empty, `.` or `..`,
    /// or contains `/` or `\`.
    pub fn workspace_console_url(&self, workspace_id: &str) -> Option<String> {
        self.console_link(&["app", workspace_id, "runs"])
    }

    /// The console's base URL followed by `segments`, each percent-encoded,
    /// or `None` without a base URL or if a segment would change the path.
    fn console_link(&self, segments: &[&str]) -> Option<String> {
        let base = self.console_url.as_deref()?;
        let path = api_path(segments).ok()?;
        Some(format!("{base}{path}"))
    }
}

/// `url` as a console base URL: without its trailing `/`, or `None` if
/// nothing is left.
pub(super) fn base_url(url: &str) -> Option<String> {
    let base = url.trim_end_matches('/');
    (!base.is_empty()).then(|| base.to_owned())
}
