//! Numeric five-field UTC cron with bounded Gregorian searches.
use borgchart::bus::time::{civil_from_days, days_from_civil, days_in_month};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct Cron {
    fields: [u64; 5],
    star: bool,
    pub text: String,
}
fn field(text: &str, min: u32, max: u32) -> Result<u64, String> {
    let number = |s: &str| -> Result<u32, String> {
        s.parse::<u32>()
            .ok()
            .filter(|n| {
                !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && (min..=max).contains(n)
            })
            .ok_or_else(|| format!("use ASCII numbers from {min} to {max}"))
    };
    let mut mask = 0;
    for item in text.split(',') {
        let (base, step) = match item.split_once('/') {
            Some((base, step)) => {
                let step = step
                    .parse::<u32>()
                    .ok()
                    .filter(|n| *n > 0 && step.bytes().all(|b| b.is_ascii_digit()))
                    .ok_or("use a positive whole step after /")?;
                if base != "*" && !base.contains('-') {
                    return Err(format!("write {base}-{max}/{step} instead of N/S"));
                }
                (base, step)
            }
            None => (item, 1),
        };
        let (start, end) = if base == "*" {
            (min, max)
        } else if let Some((a, b)) = base.split_once('-') {
            (number(a)?, number(b)?)
        } else {
            let n = number(base)?;
            (n, n)
        };
        if start > end {
            return Err("list both parts instead of a reversed or wrapping range".into());
        }
        for n in (start..=end).step_by(step as usize) {
            mask |= 1u64 << n;
        }
    }
    Ok(mask)
}
impl Cron {
    fn fields(text: &str) -> Result<Self, String> {
        let parts: Vec<_> = text.split([' ', '\t']).filter(|s| !s.is_empty()).collect();
        if parts.len() != 5 {
            return Err(
                "write five UTC fields without seconds or year, for example 0 0 * * *".into(),
            );
        }
        if text
            .bytes()
            .any(|b| !b.is_ascii_digit() && !b" \t*,-/".contains(&b))
        {
            return Err("use numbers instead of names; list days instead of L, W, # or ?; write 0 0 * * * instead of nicknames".into());
        }
        let mut fields = [0; 5];
        for (i, (min, max)) in [(0, 59), (0, 23), (1, 31), (1, 12), (0, 7)]
            .into_iter()
            .enumerate()
        {
            fields[i] = field(parts[i], min, max)?;
        }
        if fields[4] & (1 << 7) != 0 {
            fields[4] = (fields[4] & !(1 << 7)) | 1;
        }
        Ok(Self {
            fields,
            star: parts[2].starts_with('*') || parts[4].starts_with('*'),
            text: parts.join(" "),
        })
    }
    pub fn parse(text: &str) -> Result<Self, String> {
        let cron = Self::fields(text)?;
        if cron.fields[0].count_ones() > 5 {
            return Err("use at most 5 minute values per hour".into());
        }
        let anchor = UNIX_EPOCH + Duration::from_secs(days_from_civil(2000, 1, 1) as u64 * 86400);
        if cron.next(anchor).is_none() {
            return Err("use a cron expression with a possible calendar date".into());
        }
        Ok(cron)
    }
    fn day(&self, day: i64) -> bool {
        let (year, month, date) = civil_from_days(day);
        let has = |i: usize, n| self.fields[i] & (1u64 << n) != 0;
        let dom = date <= days_in_month(year, month) && has(2, date);
        let dow = has(4, (day + 4).rem_euclid(7) as u32);
        has(3, month) && if self.star { dom && dow } else { dom || dow }
    }
    fn search(&self, now: SystemTime, forward: bool) -> Option<SystemTime> {
        let minute = i64::try_from(now.duration_since(UNIX_EPOCH).ok()?.as_secs() / 60).ok()?
            + i64::from(forward);
        let day = minute.div_euclid(1440);
        for offset in 0..=146097 {
            let d = day + if forward { offset } else { -offset };
            let (year, _, _) = civil_from_days(d);
            if !(1970..=9999).contains(&year) {
                return None;
            }
            if !self.day(d) {
                continue;
            }
            for index in 0..1440 {
                let m = if forward { index } else { 1439 - index };
                let candidate = d * 1440 + m;
                if (forward && candidate < minute) || (!forward && candidate > minute) {
                    continue;
                }
                if self.fields[1] & (1 << (m / 60)) != 0 && self.fields[0] & (1 << (m % 60)) != 0 {
                    return Some(UNIX_EPOCH + Duration::from_secs(candidate as u64 * 60));
                }
            }
        }
        None
    }
    pub fn next(&self, now: SystemTime) -> Option<SystemTime> {
        self.search(now, true)
    }
    pub fn previous(&self, now: SystemTime) -> Option<SystemTime> {
        self.search(now, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use borgchart::bus::time::parse_utc;
    #[test]
    #[rustfmt::skip]
    fn cron_next_matches_reference_table() {
        for (expression, start, expected) in [
            ("* * * * *", "2024-01-01T00:00:00Z", "2024-01-01T00:01:00Z"),
            ("*/15 * * * *", "2024-01-01T00:00:00Z", "2024-01-01T00:15:00Z"),
            ("5,20,50 * * * *", "2024-01-01T00:20:00Z", "2024-01-01T00:50:00Z"),
            ("10-20/5 * * * *", "2024-01-01T00:10:00Z", "2024-01-01T00:15:00Z"),
            ("0 12 * * *", "2024-01-01T12:00:00Z", "2024-01-02T12:00:00Z"),
            ("0 0 29 2 *", "2024-03-01T00:00:00Z", "2028-02-29T00:00:00Z"),
            ("0 0 31 * *", "2024-04-01T00:00:00Z", "2024-05-31T00:00:00Z"),
            ("0 0 1 * 1", "2024-01-01T00:00:00Z", "2024-01-08T00:00:00Z"),
            ("0 0 * * 0", "2024-01-01T00:00:00Z", "2024-01-07T00:00:00Z"),
            ("0 0 * * 7", "2024-01-01T00:00:00Z", "2024-01-07T00:00:00Z"),
            ("0 0 1 1 *", "2024-12-31T23:59:00Z", "2025-01-01T00:00:00Z"),
            ("0 0 * 2 *", "2024-01-31T23:59:00Z", "2024-02-01T00:00:00Z"),
            ("0 0 30 4 *", "2024-04-30T00:00:00Z", "2025-04-30T00:00:00Z"),
            ("0 */6 * * *", "2024-01-01T05:59:59Z", "2024-01-01T06:00:00Z"),
            ("0 0 * * 1-5", "2024-01-05T00:00:00Z", "2024-01-08T00:00:00Z"),
            ("30 23 * * *", "2024-01-01T23:29:59Z", "2024-01-01T23:30:00Z"),
            ("0 0 1 * 1", "2024-01-31T23:59:00Z", "2024-02-01T00:00:00Z"),
            ("0 0 */1 * 1", "2024-01-01T00:00:00Z", "2024-01-08T00:00:00Z"),
            ("0 0 */2 * 1", "2024-01-01T00:00:00Z", "2024-01-15T00:00:00Z"),
            ("0 0 1 * */2", "2024-01-01T00:00:00Z", "2024-02-01T00:00:00Z"),
            ("0 0 * * 6,7", "2024-01-06T00:00:00Z", "2024-01-07T00:00:00Z"),
            ("0 0 * * 5-7", "2024-01-04T00:00:00Z", "2024-01-05T00:00:00Z"),
            ("0 0 * * 0-7", "2024-01-01T00:00:00Z", "2024-01-02T00:00:00Z"),
            ("0 0 * * 7-7", "2024-01-01T00:00:00Z", "2024-01-07T00:00:00Z"),
            ("0 0 * * 0-7/2", "2024-01-01T00:00:00Z", "2024-01-02T00:00:00Z"),
            ("0 0 29 2 *", "2096-03-01T00:00:00Z", "2104-02-29T00:00:00Z"),
            ("0 0 31 12 *", "2024-12-31T00:00:00Z", "2025-12-31T00:00:00Z"),
            ("0 0 * * *", "2024-01-01T23:59:59.999Z", "2024-01-02T00:00:00Z"),
        ] {
            // Syntax/search is also checked for the two overfrequent reference rows.
            let cron = Cron::fields(expression).unwrap();
            assert_eq!(cron.next(parse_utc(start).unwrap()), parse_utc(expected), "{expression} from {start}");
        }
    }
    #[test]
    #[rustfmt::skip]
    fn cron_previous_finds_latest_at_or_before() {
        for (cron, now, expected) in [
            ("0 0 1 1 *", "2025-01-01T00:00:00Z", "2025-01-01T00:00:00Z"),
            ("0 0 1 1 *", "2024-12-31T23:59:00Z", "2024-01-01T00:00:00Z"),
            ("0 0 31 * *", "2024-05-01T00:00:00Z", "2024-03-31T00:00:00Z"),
            ("0 0 29 2 *", "2103-02-28T00:00:00Z", "2096-02-29T00:00:00Z"),
            ("30 23 * * *", "2024-01-01T23:30:59.999Z", "2024-01-01T23:30:00Z"),
        ] {
            assert_eq!(Cron::parse(cron).unwrap().previous(parse_utc(now).unwrap()), parse_utc(expected));
        }
    }
    #[test]
    #[rustfmt::skip]
    fn cron_rejects_non_standard_syntax_with_a_fix() {
        for (bad, fix) in [
            ("1/2 * * * *", "1-59/2"), ("0 22-2 * * *", "list both parts"),
            ("60 * * * *", "0 to 59"), ("0 24 * * *", "0 to 23"),
            ("0 0 0 * *", "1 to 31"), ("0 0 * 13 *", "1 to 12"), ("0 0 * * 8", "0 to 7"),
            ("0,,1 * * * *", "ASCII"), ("0 0 * * MON", "numbers"), ("0 0 * jan *", "numbers"),
            ("@daily", "five UTC fields"), ("0 0 L * *", "list days"), ("0 0 1W * *", "list days"),
            ("0 0 * * 1#2", "list days"), ("0 0 ? * *", "list days"), ("*/0 * * * *", "positive"),
            ("0 0 * * * *", "without seconds or year"), ("0 0 * * * * *", "without seconds or year"),
            ("0 0 * * -1", "ASCII"), ("０ 0 * * *", "numbers"), ("0 0 * * 1\n", "numbers"),
            ("0 0 * * 1/", "positive"), ("0 0 * * 1-2/4294967296", "positive"),
        ] {
            let error = Cron::parse(bad).err().expect(bad);
            assert!(error.contains(fix), "{bad}: {error}");
        }
    }
    #[test]
    #[rustfmt::skip]
    fn cron_rejects_never_firing_and_overfrequent_expressions() {
        for bad in ["0 0 30 2 *", "0 0 31 4,6,9,11 *", "* * * * *", "*/5 * * * *", "0,10,20,30,40,50 * * * *"] {
            assert!(Cron::parse(bad).is_err(), "{bad}");
        }
        for good in ["0,12,24,36,48 * * * *", "0 0 29 2 *", "0 0 30 2 1", "0,0,0 * * * *", "0\t0\t*\t*\t*"] {
            assert!(Cron::parse(good).is_ok(), "{good}");
        }
    }
}
