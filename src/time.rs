//! Dates without a crate: RFC 3339 timestamps, the calendar math behind
//! them, and the current time as Unix seconds.
//!
//! The standard library has no calendar, so this is the SDK's one copy of
//! it: `decision::time`, the local executor, the token store and the
//! `Retry-After` dates all use it.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The current time, in seconds since the Unix epoch.
pub(crate) fn now_unix() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(since) => i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

/// `secs` since the Unix epoch as an RFC 3339 timestamp in UTC, as
/// [`rfc3339`] writes it.
pub(crate) fn unix_rfc3339(secs: i64) -> String {
    rfc3339(UNIX_EPOCH + Duration::from_secs(u64::try_from(secs).unwrap_or(0)))
}

/// The RFC 3339 timestamp `value` in seconds since the Unix epoch, or
/// `None` if it is not one (see [`parse_rfc3339`]). A fraction of a
/// second is dropped.
pub(crate) fn parse_unix(value: &str) -> Option<i64> {
    parse_rfc3339(value).map(|micros| micros.div_euclid(1_000_000))
}

/// `time` as an RFC 3339 timestamp in UTC, with microseconds, e.g.
/// `2026-10-07T14:19:02.123456Z`. The fraction always has 6 digits, so
/// timestamps sort as strings. Times before the Unix epoch are written as
/// the epoch.
pub(crate) fn rfc3339(time: SystemTime) -> String {
    let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = i64::try_from(since_epoch.as_secs()).unwrap_or(i64::MAX);
    let (year, month, day) = civil_from_days(secs / 86_400);
    let secs_of_day = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:06}Z",
        secs_of_day / 3_600,
        secs_of_day / 60 % 60,
        secs_of_day % 60,
        since_epoch.subsec_micros()
    )
}

/// The RFC 3339 timestamp `value` in microseconds since the Unix epoch, or
/// `None` if it is not one. Digits past the microseconds are cut.
///
/// It accepts what the platform's parser (Go's `time.Parse` with
/// `time.RFC3339`) accepts of RFC 3339: `YYYY-MM-DDTHH:MM:SS`, an optional
/// fraction of a second, then `Z` or an offset such as `+05:30`; `T` and `Z`
/// are uppercase, the date must exist, and seconds go up to 59.
pub(crate) fn parse_rfc3339(value: &str) -> Option<i64> {
    let (date_time, rest) = value.as_bytes().split_at_checked(19)?;
    let separators = [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')];
    if separators
        .iter()
        .any(|&(at, separator)| date_time[at] != separator)
    {
        return None;
    }
    let field = |from: usize, to: usize| digits(&date_time[from..to]);
    let (year, month, day) = (field(0, 4)?, field(5, 7)?, field(8, 10)?);
    let (hour, minute, second) = (field(11, 13)?, field(14, 16)?, field(17, 19)?);
    if !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let (micros, offset) = match rest {
        [b'.', fraction @ ..] => {
            let len = fraction.iter().take_while(|b| b.is_ascii_digit()).count();
            if len == 0 {
                return None;
            }
            let mut micros = digits(&fraction[..len.min(6)])?;
            for _ in len..6 {
                micros *= 10;
            }
            (micros, &fraction[len..])
        }
        _ => (0, rest),
    };
    let offset_secs = match offset {
        [b'Z'] => 0,
        [sign @ (b'+' | b'-'), h1, h2, b':', m1, m2] => {
            let (hours, minutes) = (digits(&[*h1, *h2])?, digits(&[*m1, *m2])?);
            if hours > 23 || minutes > 59 {
                return None;
            }
            let secs = hours * 3_600 + minutes * 60;
            if *sign == b'-' { -secs } else { secs }
        }
        _ => return None,
    };
    let secs = days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second
        - offset_secs;
    Some(secs * 1_000_000 + micros)
}

/// `bytes` as a decimal number, if they are all ASCII digits.
fn digits(bytes: &[u8]) -> Option<i64> {
    bytes.iter().try_fold(0, |number, &byte| {
        byte.is_ascii_digit()
            .then(|| number * 10 + i64::from(byte - b'0'))
    })
}

/// Number of days in `month` (1 to 12) of `year`, in the Gregorian calendar.
fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The days from 1970-01-01 to the civil date `year`-`month`-`day`
/// (Howard Hinnant's `days_from_civil`), negative before it.
pub(crate) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The civil date `days` after 1970-01-01, as `(year, month, day)` (Howard
/// Hinnant's `civil_from_days`), the inverse of [`days_from_civil`].
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_rfc3339_timestamps() {
        let at = |secs: u64, nanos: u32| rfc3339(UNIX_EPOCH + Duration::new(secs, nanos));
        assert_eq!(at(0, 0), "1970-01-01T00:00:00.000000Z");
        assert_eq!(at(784_111_777, 0), "1994-11-06T08:49:37.000000Z");
        assert_eq!(at(951_782_400, 0), "2000-02-29T00:00:00.000000Z");
        assert_eq!(
            at(1_835_438_400, 123_456_789),
            "2028-02-29T12:00:00.123456Z"
        );
        assert_eq!(at(4_107_542_400, 0), "2100-03-01T00:00:00.000000Z");
        assert_eq!(
            at(253_402_300_799, 999_999_999),
            "9999-12-31T23:59:59.999999Z"
        );
        let before_epoch = UNIX_EPOCH - Duration::from_secs(1);
        assert_eq!(rfc3339(before_epoch), "1970-01-01T00:00:00.000000Z");
    }

    #[test]
    fn parses_what_it_formats() {
        for secs in [0, 784_111_777, 951_782_400, 1_835_438_400, 253_402_300_799] {
            let time = UNIX_EPOCH + Duration::new(secs, 123_456_000);
            let micros = i64::try_from(secs).unwrap() * 1_000_000 + 123_456;
            assert_eq!(parse_rfc3339(&rfc3339(time)), Some(micros), "{secs}");
        }
    }

    #[test]
    fn parses_fractions_and_offsets() {
        let at = parse_rfc3339;
        assert_eq!(at("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(at("1970-01-01T00:00:01.5Z"), Some(1_500_000));
        assert_eq!(
            at("2026-10-07T14:19:02.123456Z"),
            Some(1_791_382_742_123_456)
        );
        // Digits past the microseconds are cut.
        assert_eq!(
            at("2026-10-07T09:19:02.1234569-05:00"),
            at("2026-10-07T14:19:02.123456Z")
        );
        // Microseconds with a `+00:00` offset instead of `Z`.
        assert_eq!(
            at("2026-09-21T14:13:20.000000+00:00"),
            Some(1_790_000_000_000_000)
        );
        assert_eq!(at("1969-12-31T23:59:59Z"), Some(-1_000_000));
        assert_eq!(at("0000-01-01T00:00:00Z"), Some(-62_167_219_200_000_000));
    }

    #[test]
    fn accepts_rfc3339_timestamps() {
        for value in [
            "2026-10-01T12:00:00Z",
            "2026-10-01T12:00:00.5Z",
            "2026-10-01T12:00:00.123456Z",
            "2026-10-01T12:00:00.123456789Z",
            "2026-10-01T12:00:00.1+05:30",
            "2026-10-01T12:00:00-05:00",
            "2026-10-01T00:00:00+23:59",
            "2026-12-31T23:59:59-23:59",
            "2026-12-31T00:00:00-00:00",
            "2000-02-29T00:00:00+00:00",
            "2024-02-29T00:00:00Z",
            "0000-01-01T00:00:00Z",
        ] {
            assert!(parse_rfc3339(value).is_some(), "{value}");
        }
    }

    #[test]
    fn rejects_other_timestamps() {
        for value in [
            "",
            "now",
            "2026-10-01",
            "2026-10-01T12:00:00",
            "2026-10-01T12:00Z",
            "2026-10-01 12:00:00Z",
            "2026-10-01t12:00:00Z",
            "2026-10-01T12:00:00z",
            "2026-10-01T12:00:00Z ",
            "2026-10-01T12:00:00ZZ",
            "2026/10/01T12:00:00Z",
            "26-10-01T12:00:00Z",
            "+026-10-01T12:00:00Z",
            "2026-1-01T12:00:00Z",
            "2026-1a-01T12:00:00Z",
            "2026-10-01T1:00:00Z",
            "２026-10-01T12:00:00Z",
            "2026-10-01T12:00:00.Z",
            "2026-10-01T12:00:00,5Z",
            "2026-10-01T12:00:00+0530",
            "2026-10-01T12:00:00+05",
            "2026-10-01T12:00:00+5:30",
            "2026-10-01T12:00:00 +05:30",
            "2026-10-01T12:00:00+0a:30",
            "2026-10-01T12:00:00+05:3a",
            "2026-10-01T12:00:00+24:00",
            "2026-10-01T12:00:00+05:60",
            "2026-00-01T00:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-10-00T00:00:00Z",
            "2026-10-32T00:00:00Z",
            "2026-04-31T00:00:00Z",
            "2026-02-29T00:00:00Z",
            "1900-02-29T00:00:00Z",
            "2026-10-01T24:00:00Z",
            "2026-10-01T12:60:00Z",
            "2026-10-01T12:00:60Z",
            "2026-10-01T12:00:00é",
        ] {
            assert_eq!(parse_rfc3339(value), None, "{value}");
        }
    }

    #[test]
    fn counts_the_days_of_each_month() {
        let days: Vec<i64> = (1..=12).map(|month| days_in_month(2026, month)).collect();
        assert_eq!(days, [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]);
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(2100, 2), 28);
    }

    #[test]
    fn converts_between_days_and_dates() {
        for (days, date) in [
            (0, (1970, 1, 1)),
            (-1, (1969, 12, 31)),
            (11_016, (2000, 2, 29)),
            (2_932_896, (9999, 12, 31)),
            (-719_528, (0, 1, 1)),
        ] {
            assert_eq!(civil_from_days(days), date, "{days}");
            assert_eq!(days_from_civil(date.0, date.1, date.2), days, "{date:?}");
        }
    }

    #[test]
    fn converts_unix_seconds() {
        assert_eq!(unix_rfc3339(1_790_000_000), "2026-09-21T14:13:20.000000Z");
        assert_eq!(unix_rfc3339(-5), "1970-01-01T00:00:00.000000Z");
        assert_eq!(parse_unix("2026-09-21T14:13:20.999Z"), Some(1_790_000_000));
        assert_eq!(parse_unix("1969-12-31T23:59:59.5Z"), Some(-1));
        assert_eq!(parse_unix("2026-09-21 14:13:20Z"), None);
    }

    #[test]
    fn knows_the_time() {
        assert!(now_unix() > 1_790_000_000);
    }
}
