//! UTC timestamps as RFC 3339 strings (`2026-11-05T19:00:00.123Z`), without
//! a date library.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Now, in UTC, with millisecond precision.
pub fn utc_now() -> String {
    format_utc(SystemTime::now())
}

pub fn format_utc(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as i64;
    let millis = d.subsec_millis();
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, day) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Parse exactly `YYYY-MM-DDTHH:MM:SS[.f…]Z` (one to nine fraction digits,
/// UTC only, years 1970–9999). Impossible dates (`02-31`), leap seconds and
/// anything else return `None`; nothing is normalized.
pub fn parse_utc(s: &str) -> Option<SystemTime> {
    let b = s.as_bytes();
    if b.len() < 20 || *b.last()? != b'Z' {
        return None;
    }
    let digits = |r: std::ops::Range<usize>| -> Option<u64> {
        let part = s.get(r)?;
        if part.bytes().all(|c| c.is_ascii_digit()) {
            part.parse().ok()
        } else {
            None
        }
    };
    if b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let y = digits(0..4)?;
    let m = digits(5..7)? as u32;
    let d = digits(8..10)? as u32;
    let hh = digits(11..13)?;
    let mm = digits(14..16)?;
    let ss = digits(17..19)?;
    let millis = match s.get(19..s.len() - 1)? {
        "" => 0,
        frac => {
            let f = frac.strip_prefix('.')?;
            if f.is_empty() || f.len() > 9 || !f.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            format!("{f:0<3}")[..3].parse().ok()?
        }
    };
    if !(1970..=9999).contains(&y) || hh > 23 || mm > 59 || ss > 59 {
        return None;
    }
    if !(1..=12).contains(&m) || d == 0 || d > days_in_month(y as i64, m) {
        return None;
    }
    let days = u64::try_from(days_from_civil(y as i64, m, d)).ok()?;
    let secs = days
        .checked_mul(86_400)?
        .checked_add(hh * 3600 + mm * 60 + ss)?;
    UNIX_EPOCH
        .checked_add(Duration::from_secs(secs))?
        .checked_add(Duration::from_millis(millis))
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

// Howard Hinnant's civil-date algorithms.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let t = UNIX_EPOCH + Duration::from_millis(1_793_905_200_123);
        let s = format_utc(t);
        assert_eq!(s, "2026-11-05T19:00:00.123Z");
        assert_eq!(parse_utc(&s), Some(t));
        assert_eq!(format_utc(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            parse_utc("2024-02-29T23:59:59Z").map(format_utc).as_deref(),
            Some("2024-02-29T23:59:59.000Z")
        );
    }

    #[test]
    fn rejects_non_utc() {
        assert_eq!(parse_utc("2026-11-05T19:00:00+01:00"), None);
        assert_eq!(parse_utc("2026-13-05T19:00:00Z"), None);
        assert_eq!(parse_utc("2026-11-05 19:00:00Z"), None);
        assert_eq!(parse_utc("2026-11-05T19:00:00.Z"), None);
    }

    #[test]
    fn rejects_impossible_and_extreme_inputs() {
        for bad in [
            "2026-02-31T00:00:00Z",
            "2026-04-31T00:00:00Z",
            "2025-02-29T00:00:00Z",
            "2026-11-05T23:59:60Z",
            "2026-11-05T24:00:00Z",
            "1969-12-31T23:59:59Z",
            "9223372036854775807-01-01T00:00:00Z",
            "+026-11-05T19:00:00Z",
            "2026-1-05T19:00:00Z",
            "2026-11-05T19:00:00.1234567890Z",
            "2026-11-05T19:00:00.12aZ",
            "",
            "Z",
            "2026-11-05T19:00:00.\u{e9}Z",
            "2026-11-05T19:00:0\u{e9}Z",
        ] {
            assert_eq!(parse_utc(bad), None, "{bad}");
        }
        assert!(parse_utc("2000-02-29T00:00:00Z").is_some());
        assert!(parse_utc("9999-12-31T23:59:59.999999999Z").is_some());
        assert_eq!(
            parse_utc("2026-11-05T19:00:00.5Z")
                .map(format_utc)
                .as_deref(),
            Some("2026-11-05T19:00:00.500Z")
        );
    }
}
