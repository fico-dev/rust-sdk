//! The error and result types shared by the whole SDK.

use std::error::Error as StdError;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

/// Boxed source error, used to keep transport crates out of the public API.
pub(crate) type BoxError = Box<dyn StdError + Send + Sync + 'static>;

/// Longest response body or stderr, in bytes, that [`Error::Status`] and
/// [`Error::Command`] print.
const MAX_DISPLAYED_BODY: usize = 1024;

/// A `Result` whose error type defaults to [`Error`].
///
/// Because this alias shadows `std::result::Result` where it is imported,
/// the second parameter keeps `Result<T, OtherError>` usable too.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The error type of every fallible operation in the SDK.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A URL could not be parsed or is not usable as an HTTP URL.
    #[non_exhaustive]
    InvalidUrl {
        /// The offending URL.
        url: String,
        /// Why the URL was rejected.
        reason: String,
    },
    /// A header name or value is not valid HTTP.
    #[non_exhaustive]
    InvalidHeader {
        /// Name of the offending header.
        name: String,
    },
    /// A request body or query string could not be encoded.
    #[non_exhaustive]
    Encode {
        /// What could not be encoded, and why.
        reason: String,
        /// The underlying serialization error, if there is one.
        source: Option<BoxError>,
    },
    /// The underlying HTTP client could not be initialized (e.g. TLS setup).
    #[non_exhaustive]
    Init {
        /// The underlying initialization error.
        source: BoxError,
    },
    /// A connection to the server could not be established: it was refused,
    /// the host is unknown, TLS failed, or the connect limit ran out
    /// ([`ClientBuilder::connect_timeout`]), in which case the source says
    /// the operation timed out. The server never got the request.
    ///
    /// [`ClientBuilder::connect_timeout`]: crate::ClientBuilder::connect_timeout
    #[non_exhaustive]
    Connect {
        /// URL of the request.
        url: String,
        /// The underlying connection error.
        source: BoxError,
    },
    /// The request did not complete within a read limit or the
    /// whole-attempt limit (see [`ClientBuilder`'s time
    /// limits](crate::ClientBuilder#time-limits)). Running out of the
    /// connect limit is [`Error::Connect`] instead.
    #[non_exhaustive]
    Timeout {
        /// URL of the request.
        url: String,
        /// The timeout that was exceeded.
        after: Duration,
    },
    /// The request failed for another transport-level reason.
    #[non_exhaustive]
    Request {
        /// URL of the request.
        url: String,
        /// The underlying transport error.
        source: BoxError,
    },
    /// The server responded with an error status code (4xx or 5xx).
    #[non_exhaustive]
    Status {
        /// HTTP status code of the response.
        status: u16,
        /// URL of the request.
        url: String,
        /// Response body, decoded lossily as UTF-8. The error message only
        /// shows its first 1 KiB.
        body: String,
    },
    /// The server answered with a redirect that was not followed. Redirects
    /// from the base URL's origin to another origin are not followed, so
    /// that default headers and the bearer token stay on the base origin.
    /// Send a new request to `location` to follow it.
    #[non_exhaustive]
    Redirect {
        /// HTTP status code of the response (3xx).
        status: u16,
        /// URL of the request.
        url: String,
        /// Target of the redirect, from the `Location` header, resolved
        /// against `url`.
        location: String,
    },
    /// The response body could not be decoded.
    #[non_exhaustive]
    Decode {
        /// URL of the request.
        url: String,
        /// The underlying decoding error.
        source: BoxError,
    },
    /// A paginated response was malformed or exceeded the page limit.
    #[non_exhaustive]
    Pagination {
        /// What went wrong.
        reason: String,
    },
    /// An argument was rejected before any request was sent, or an
    /// operation needs a context that is not configured (e.g. a tool that
    /// reads files, called in remote mode).
    #[non_exhaustive]
    Invalid {
        /// Why the argument or the call was rejected.
        reason: String,
    },
    /// A local file or directory could not be read or written.
    #[non_exhaustive]
    Io {
        /// The file or directory.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// A workspace manifest (`workspace.yaml`) is invalid. A manifest that
    /// cannot be read is [`Error::Io`].
    #[non_exhaustive]
    Manifest {
        /// The manifest file.
        path: PathBuf,
        /// What is wrong with it.
        reason: String,
    },
    /// A child process failed: a build or pre-push command, or a local run.
    #[non_exhaustive]
    Command {
        /// The program that was run.
        program: String,
        /// Its exit code, or `None` if a signal ended it.
        status: Option<i32>,
        /// What it wrote to stderr. The error message only shows its first
        /// 1 KiB.
        stderr: String,
    },
    /// A local resource, such as a registered workspace or a local run,
    /// does not exist. A missing platform resource is reported as
    /// [`Error::Status`] with status `404` instead.
    #[non_exhaustive]
    NotFound {
        /// What kind of resource it is, e.g. `"local run"`.
        kind: &'static str,
        /// The ID that was looked up.
        id: String,
    },
    /// A PKCE session has no valid tokens: it never logged in, logged out,
    /// or its refresh token was rejected (`invalid_grant`), which deletes
    /// its tokens. Log in again.
    #[non_exhaustive]
    NotLoggedIn {
        /// The session, e.g. `default`.
        session: String,
    },
    /// A PKCE login failed: its callback port is taken, or the identity
    /// provider answered but reported an error, the callback did not match
    /// the login, or the tokens it issued are not valid for this client.
    /// The reason never quotes a token or a code.
    #[non_exhaustive]
    Login {
        /// Why the login failed.
        reason: String,
    },
    /// A profile can't be used for the call: it breaks a rule that the host
    /// words itself, or a choice is missing. The data says which, and never
    /// holds a token, a code, a verifier or an API key.
    #[non_exhaustive]
    Profile {
        /// Name of the profile.
        profile: String,
        /// What is wrong.
        problem: ProfileProblem,
    },
    /// Polling ended without the change it waited for: it ran out of time
    /// or tries, or the platform reports that the change failed, such as a
    /// delete that the platform could not finish.
    #[non_exhaustive]
    Wait {
        /// Why polling ended, e.g. the time limit it ran out of.
        reason: String,
    },
    /// The MCP server could not listen on its address, or its transport
    /// failed.
    #[non_exhaustive]
    Serve {
        /// What failed, e.g. `could not listen on 127.0.0.1:8080`.
        reason: String,
        /// The underlying error, if there is one.
        source: Option<BoxError>,
    },
    /// The function is a scaffold stub that is not implemented yet. No
    /// release ships with stubs; this variant is deleted before release.
    #[non_exhaustive]
    Unimplemented {
        /// The function, e.g. `"Platform::get_run"`.
        function: &'static str,
    },
}

/// What is wrong with a profile in an [`Error::Profile`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProfileProblem {
    /// The profile is not a PKCE profile, so it has no login.
    NotPkce {
        /// Its `auth_type`, e.g. `api_key`.
        auth_type: &'static str,
    },
    /// The profile has no endpoint, and there is no default endpoint.
    NoEndpoint,
    /// A PKCE profile on an endpoint other than the default one needs a
    /// session other than `default`.
    NoSession {
        /// The endpoint it uses, normalized.
        endpoint: String,
    },
    /// The endpoint has no OIDC settings: give its discovery URL and client
    /// ID.
    NoOidc {
        /// The endpoint, normalized.
        endpoint: String,
    },
    /// The team is missing or unknown. The user logged in already, so a
    /// second call with a team from `teams` needs no new login.
    Team {
        /// The endpoint, normalized.
        endpoint: String,
        /// The team name that was given, if any.
        team: Option<String>,
        /// The names of the user's teams, sorted.
        teams: Vec<String>,
    },
    /// The profile is the default one and others exist: name the profile
    /// that becomes the default.
    NewDefaultNeeded {
        /// The names of the other profiles, sorted.
        profiles: Vec<String>,
    },
}

impl Error {
    /// Whether a failed attempt with this error may be retried. Redirect,
    /// builder and decode errors fail the same way every time, so they are
    /// not retried.
    pub(crate) fn is_retryable(&self) -> bool {
        match self {
            Error::Connect { .. } | Error::Timeout { .. } => true,
            Error::Request { source, .. } => source
                .downcast_ref::<reqwest::Error>()
                .is_some_and(|e| !(e.is_redirect() || e.is_builder() || e.is_decode())),
            _ => false,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidUrl { url, reason } => write!(f, "invalid URL `{url}`: {reason}"),
            Error::InvalidHeader { name } => write!(f, "invalid header `{name}`"),
            Error::Encode { reason, .. } => write!(f, "could not encode request: {reason}"),
            Error::Init { .. } => f.write_str("could not initialize HTTP client"),
            Error::Connect { url, .. } => write!(f, "could not connect to {url}"),
            Error::Timeout { url, after } => {
                write!(f, "request to {url} timed out after {after:?}")
            }
            Error::Request { url, .. } => write!(f, "request to {url} failed"),
            Error::Status { status, url, body } => {
                write!(
                    f,
                    "request to {url} failed with status code {status} and message: "
                )?;
                write_truncated(f, body)
            }
            Error::Redirect {
                status,
                url,
                location,
            } => write!(
                f,
                "request to {url} was redirected to {location} with status code {status}, \
                 which was not followed"
            ),
            Error::Decode { url, .. } => write!(f, "could not decode response from {url}"),
            Error::Pagination { reason } => write!(f, "pagination failed: {reason}"),
            Error::Invalid { reason } => write!(f, "invalid argument: {reason}"),
            Error::Io { path, .. } => write!(f, "could not access `{}`", path.display()),
            Error::Manifest { path, reason } => {
                write!(f, "invalid manifest `{}`: {reason}", path.display())
            }
            Error::Command {
                program,
                status,
                stderr,
            } => {
                match status {
                    Some(code) => write!(f, "command `{program}` failed with exit code {code}")?,
                    None => write!(f, "command `{program}` was ended by a signal")?,
                }
                if stderr.is_empty() {
                    return Ok(());
                }
                f.write_str(" and stderr: ")?;
                write_truncated(f, stderr)
            }
            Error::NotFound { kind, id } => write!(f, "{kind} `{id}` not found"),
            Error::NotLoggedIn { session } => write!(f, "session `{session}` is not logged in"),
            Error::Login { reason } => write!(f, "login failed: {reason}"),
            Error::Profile { profile, problem } => write_profile_problem(f, profile, problem),
            Error::Wait { reason } => write!(f, "stopped waiting: {reason}"),
            Error::Serve { reason, .. } => write!(f, "MCP server failed: {reason}"),
            Error::Unimplemented { function } => write!(f, "`{function}` is not implemented yet"),
        }
    }
}

/// `names` in backticks, separated by commas, or `none`.
fn quoted_names(names: &[String]) -> String {
    match names.is_empty() {
        true => "none".to_owned(),
        false => names
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// The message of an [`Error::Profile`].
fn write_profile_problem(
    f: &mut fmt::Formatter<'_>,
    profile: &str,
    problem: &ProfileProblem,
) -> fmt::Result {
    match problem {
        ProfileProblem::NotPkce { auth_type } => write!(
            f,
            "profile `{profile}` is not a PKCE profile (it is `{auth_type}`), so it has no login"
        ),
        ProfileProblem::NoEndpoint => write!(
            f,
            "profile `{profile}` needs an endpoint, since there is no default endpoint"
        ),
        ProfileProblem::NoSession { endpoint } => write!(
            f,
            "PKCE profile `{profile}` uses `{endpoint}`, not the default endpoint, so it needs a \
             session other than `default`"
        ),
        ProfileProblem::NoOidc { endpoint } => write!(
            f,
            "endpoint `{endpoint}` of profile `{profile}` has no OIDC settings: give the \
             discovery URL and the client ID"
        ),
        ProfileProblem::Team {
            endpoint,
            team,
            teams,
        } => {
            let names = quoted_names(teams);
            match team {
                Some(team) => write!(
                    f,
                    "no team is named `{team}` on `{endpoint}`; the teams are: {names}"
                ),
                None => write!(
                    f,
                    "profile `{profile}` needs a team on `{endpoint}`; the teams are: {names}"
                ),
            }
        }
        ProfileProblem::NewDefaultNeeded { profiles } => write!(
            f,
            "profile `{profile}` is the default profile: name the profile that becomes the \
             default; the others are: {}",
            quoted_names(profiles)
        ),
    }
}

/// Writes `text`, cut to its first [`MAX_DISPLAYED_BODY`] bytes.
fn write_truncated(f: &mut fmt::Formatter<'_>, text: &str) -> fmt::Result {
    if text.len() <= MAX_DISPLAYED_BODY {
        return f.write_str(text);
    }
    let end = text.floor_char_boundary(MAX_DISPLAYED_BODY);
    write!(f, "{}…", &text[..end])
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Error::Encode { source, .. } | Error::Serve { source, .. } => {
                source.as_deref().map(|source| source as _)
            }
            Error::Init { source }
            | Error::Connect { source, .. }
            | Error::Request { source, .. }
            | Error::Decode { source, .. } => Some(source.as_ref()),
            Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_error(source: BoxError) -> Error {
        Error::Request {
            url: "https://host".to_owned(),
            source,
        }
    }

    #[test]
    fn does_not_retry_deterministic_request_errors() {
        let builder = reqwest::Client::new().get("not a url").build().unwrap_err();
        assert!(builder.is_builder());
        assert!(!request_error(Box::new(builder)).is_retryable());
        assert!(!request_error("other".into()).is_retryable());
    }

    #[test]
    fn leaves_the_source_out_of_the_message() {
        let error = request_error("cause".into());
        assert_eq!(error.to_string(), "request to https://host failed");
        assert_eq!(error.source().unwrap().to_string(), "cause");
    }

    fn status_error(body: String) -> String {
        Error::Status {
            status: 500,
            url: "https://host".to_owned(),
            body,
        }
        .to_string()
    }

    #[test]
    fn truncates_long_status_bodies() {
        let prefix = "request to https://host failed with status code 500 and message: ";
        let short = "a".repeat(MAX_DISPLAYED_BODY);
        assert_eq!(status_error(short.clone()), format!("{prefix}{short}"));

        let long = format!("{}é", "a".repeat(MAX_DISPLAYED_BODY - 1));
        let shown = "a".repeat(MAX_DISPLAYED_BODY - 1);
        assert_eq!(status_error(long), format!("{prefix}{shown}…"));
    }

    fn command_error(status: Option<i32>, stderr: String) -> String {
        Error::Command {
            program: "go".to_owned(),
            status,
            stderr,
        }
        .to_string()
    }

    #[test]
    fn shows_the_exit_code_and_truncated_stderr_of_commands() {
        assert_eq!(
            command_error(Some(2), String::new()),
            "command `go` failed with exit code 2"
        );
        assert_eq!(
            command_error(None, "killed".to_owned()),
            "command `go` was ended by a signal and stderr: killed"
        );
        let shown = "a".repeat(MAX_DISPLAYED_BODY);
        assert_eq!(
            command_error(Some(1), format!("{shown}b")),
            format!("command `go` failed with exit code 1 and stderr: {shown}…")
        );
    }

    #[test]
    fn keeps_the_io_error_as_the_source() {
        let error = Error::Io {
            path: PathBuf::from("workspace.yaml"),
            source: io::Error::from(io::ErrorKind::NotFound),
        };
        assert_eq!(error.to_string(), "could not access `workspace.yaml`");
        assert_eq!(error.source().unwrap().to_string(), "entity not found");
    }

    #[test]
    fn keeps_the_cause_of_server_failures_as_the_source() {
        let error = Error::Serve {
            reason: "could not listen on 127.0.0.1:8080".to_owned(),
            source: Some(Box::new(io::Error::from(io::ErrorKind::AddrInUse))),
        };
        assert_eq!(
            error.to_string(),
            "MCP server failed: could not listen on 127.0.0.1:8080"
        );
        assert_eq!(error.source().unwrap().to_string(), "address in use");
    }

    #[test]
    fn names_the_session_that_is_not_logged_in() {
        let error = Error::NotLoggedIn {
            session: "work".to_owned(),
        };
        assert_eq!(error.to_string(), "session `work` is not logged in");
        let error = Error::Login {
            reason: "the state does not match".to_owned(),
        };
        assert_eq!(error.to_string(), "login failed: the state does not match");
    }

    #[test]
    fn words_profile_problems_with_the_names_they_carry() {
        let error = Error::Profile {
            profile: "work".to_owned(),
            problem: ProfileProblem::Team {
                endpoint: "https://api.example.com".to_owned(),
                team: None,
                teams: vec!["Hare".to_owned(), "Owl".to_owned()],
            },
        };
        assert_eq!(
            error.to_string(),
            "profile `work` needs a team on `https://api.example.com`; the teams are: `Hare`, `Owl`"
        );
    }

    #[test]
    fn stays_small_enough_to_return_by_value() {
        // `clippy::result_large_err` fires at 128 bytes.
        assert!(std::mem::size_of::<Error>() < 128);
    }
}
