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

/// Parse `YYYY-MM-DDTHH:MM:SS[.fff]Z`. Only UTC (`Z`) is accepted.
pub fn parse_utc(s: &str) -> Option<SystemTime> {
    let s = s.strip_suffix('Z')?;
    let (date, clock) = s.split_once('T')?;
    let mut dp = date.splitn(3, '-');
    let y: i64 = dp.next()?.parse().ok()?;
    let m: u32 = dp.next()?.parse().ok()?;
    let d: u32 = dp.next()?.parse().ok()?;
    let (hms, frac) = match clock.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (clock, None),
    };
    let mut cp = hms.splitn(3, ':');
    let hh: u64 = cp.next()?.parse().ok()?;
    let mm: u64 = cp.next()?.parse().ok()?;
    let ss: u64 = cp.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    let millis: u64 = match frac {
        Some(f) if !f.is_empty() && f.len() <= 9 && f.chars().all(|c| c.is_ascii_digit()) => {
            let padded = format!("{f:0<3}");
            padded[..3].parse().ok()?
        }
        Some(_) => return None,
        None => 0,
    };
    let days = days_from_civil(y, m, d);
    if days < 0 {
        return None;
    }
    let secs = days as u64 * 86_400 + hh * 3600 + mm * 60 + ss;
    Some(UNIX_EPOCH + Duration::from_secs(secs) + Duration::from_millis(millis))
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
}
