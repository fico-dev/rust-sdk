//! Request paths built from IDs.
//!
//! IDs come from users and from LLMs, so every path is built with
//! [`api_path`], never with `format!`: it percent-encodes each segment and
//! rejects the ones that would change the path's shape.

use crate::{Error, Result};

/// Joins `segments` into a request path such as `/v1/applications/routing`,
/// percent-encoding each one.
///
/// Every byte except the RFC 3986 unreserved characters (`A-Z a-z 0-9 - . _
/// ~`) is encoded, so a segment can never add a query or a fragment.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if a segment is empty, `.` or `..`, which
/// [`Client::request`](crate::Client::request) would otherwise resolve into
/// another resource's path, or if it contains `/` or `\`. The platform
/// decodes an encoded `/` and resolves the `..` it makes, so `x/../y` would
/// reach `y`; no ID the platform makes has either character.
pub(crate) fn api_path(segments: &[&str]) -> Result<String> {
    let mut path = String::new();
    for segment in segments {
        if matches!(*segment, "" | "." | "..") {
            return Err(Error::Invalid {
                reason: format!("`{segment}` is not a valid ID or path segment"),
            });
        }
        if segment.contains(['/', '\\']) {
            return Err(Error::Invalid {
                reason: format!("`{segment}` is not a valid ID: it contains `/` or `\\`"),
            });
        }
        path.push('/');
        for byte in segment.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                path.push(char::from(byte));
            } else {
                path.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_plain_segments() {
        assert_eq!(
            api_path(&["v1", "applications", "routing-1.2_x~y"]).unwrap(),
            "/v1/applications/routing-1.2_x~y"
        );
        assert_eq!(api_path(&[]).unwrap(), "");
    }

    #[test]
    fn encodes_everything_else() {
        assert_eq!(
            api_path(&["v1", "a?b#c d%"]).unwrap(),
            "/v1/a%3Fb%23c%20d%25"
        );
        assert_eq!(api_path(&["é"]).unwrap(), "/%C3%A9");
        assert_eq!(api_path(&["..."]).unwrap(), "/...");
        assert_eq!(api_path(&["%2e%2e"]).unwrap(), "/%252e%252e");
    }

    #[test]
    fn rejects_segments_that_change_the_path() {
        for segment in ["", ".", "..", "a/b", "x/../y", "a\\b", "/", "x/.."] {
            let err = api_path(&["v1", "applications", segment]).unwrap_err();
            assert!(matches!(err, Error::Invalid { .. }), "{segment}: {err}");
        }
    }
}
