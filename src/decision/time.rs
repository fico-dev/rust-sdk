//! RFC 3339 timestamps: the ones the SDK writes, and timestamp parameters,
//! checked before any request. The calendar is `crate::time`'s.

use std::time::SystemTime;

use crate::time::{parse_rfc3339, rfc3339};
use crate::{Error, Result};

/// The current time as an RFC 3339 timestamp in UTC, with microseconds,
/// e.g. `2026-10-07T14:19:02.123456Z`. The fraction always has 6 digits, so
/// timestamps sort as strings.
pub(crate) fn now_rfc3339() -> String {
    rfc3339(SystemTime::now())
}

/// `value`, the timestamp parameter `field`, if it is set.
///
/// `None` and `""` both count as unset and give `Ok(None)`, so the caller
/// leaves the value out of the request. The platform's `optional-RFC3339`
/// validator reads `""` the same way, and the SDK treats empty IDs and names
/// as unset too.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if `value` is set but is not an RFC 3339
/// timestamp, as the platform reads one (see [`parse_rfc3339`]). The message
/// names `field`, not the value.
pub(crate) fn optional_timestamp(field: &str, value: Option<String>) -> Result<Option<String>> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if parse_rfc3339(&value).is_none() {
        return Err(Error::Invalid {
            reason: format!(
                "`{field}` must be an RFC 3339 timestamp, such as `2026-10-01T12:00:00Z`"
            ),
        });
    }
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_unset_and_empty_values_as_unset() {
        assert_eq!(optional_timestamp("start_time", None).unwrap(), None);
        assert_eq!(
            optional_timestamp("start_time", Some(String::new())).unwrap(),
            None
        );
    }

    #[test]
    fn keeps_rfc3339_values() {
        let value = "2026-10-01T12:00:00.5+05:30".to_owned();
        assert_eq!(
            optional_timestamp("start_time", Some(value.clone())).unwrap(),
            Some(value)
        );
    }

    #[test]
    fn rejects_other_values_without_repeating_them() {
        let err = optional_timestamp("start_events.time", Some("tomorrow".to_owned())).unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
        assert_eq!(
            err.to_string(),
            "invalid argument: `start_events.time` must be an RFC 3339 timestamp, such as \
             `2026-10-01T12:00:00Z`"
        );
    }

    #[test]
    fn now_is_an_rfc3339_timestamp() {
        let now = now_rfc3339();
        assert_eq!(now.len(), "2026-10-07T14:19:02.123456Z".len(), "{now}");
        assert!(parse_rfc3339(&now).is_some(), "{now}");
    }
}
