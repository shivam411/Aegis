//! Five-field cron expressions (UTC): `minute hour day-of-month month
//! day-of-week`, with `*`, lists (`1,15`), ranges (`1-5`) and steps (`*/10`,
//! `0-30/5`), plus `@hourly`, `@daily`, `@weekly` and `@monthly`. Sunday is 0
//! or 7. As in classic cron, when both day fields are restricted a time
//! matches if either does.

use chrono::{DateTime, Datelike, Timelike, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronSchedule {
    minutes: u64,
    hours: u32,
    days: u32,
    months: u16,
    weekdays: u8,
    days_any: bool,
    weekdays_any: bool,
}

fn field(spec: &str, lo: u32, hi: u32, name: &str) -> Result<u64, String> {
    let mut bits = 0u64;
    for part in spec.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => {
                let step: u32 = s
                    .parse()
                    .ok()
                    .filter(|s| *s > 0)
                    .ok_or_else(|| format!("bad step '{}' in the {} field", s, name))?;
                (r, step)
            }
            None => (part, 1),
        };
        let (from, to) = if range == "*" {
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            (num(a, lo, hi, name)?, num(b, lo, hi, name)?)
        } else {
            let n = num(range, lo, hi, name)?;
            // "5/10" means 5 to the end, every 10.
            (n, if part.contains('/') { hi } else { n })
        };
        if from > to {
            return Err(format!(
                "'{}' is a backwards range in the {} field",
                part, name
            ));
        }
        let mut v = from;
        while v <= to {
            bits |= 1 << v;
            v += step;
        }
    }
    Ok(bits)
}

fn num(s: &str, lo: u32, hi: u32, name: &str) -> Result<u32, String> {
    s.parse::<u32>()
        .ok()
        .filter(|n| (lo..=hi).contains(n))
        .ok_or_else(|| format!("'{}' is not valid in the {} field ({}-{})", s, name, lo, hi))
}

impl CronSchedule {
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let expanded = match text {
            "@hourly" => "0 * * * *",
            "@daily" | "@midnight" => "0 0 * * *",
            "@weekly" => "0 0 * * 0",
            "@monthly" => "0 0 1 * *",
            other => other,
        };
        let parts: Vec<&str> = expanded.split_whitespace().collect();
        let [min, hour, dom, month, dow] = parts[..] else {
            return Err(
                "A schedule has five fields: minute hour day-of-month month day-of-week (for example `*/15 * * * *`)"
                    .into(),
            );
        };
        let mut weekdays = field(dow, 0, 7, "day-of-week")?;
        if weekdays & (1 << 7) != 0 {
            weekdays = (weekdays & !(1 << 7)) | 1;
        }
        Ok(Self {
            minutes: field(min, 0, 59, "minute")?,
            hours: field(hour, 0, 23, "hour")? as u32,
            days: field(dom, 1, 31, "day-of-month")? as u32,
            months: field(month, 1, 12, "month")? as u16,
            weekdays: weekdays as u8,
            days_any: dom.starts_with('*'),
            weekdays_any: dow.starts_with('*'),
        })
    }

    pub fn matches(&self, at: DateTime<Utc>) -> bool {
        if self.minutes & (1 << at.minute()) == 0
            || self.hours & (1 << at.hour()) == 0
            || self.months & (1 << at.month()) == 0
        {
            return false;
        }
        let day = self.days & (1 << at.day()) != 0;
        let weekday = self.weekdays & (1 << at.weekday().num_days_from_sunday()) != 0;
        match (self.days_any, self.weekdays_any) {
            (true, true) => true,
            (false, true) => day,
            (true, false) => weekday,
            (false, false) => day || weekday,
        }
    }

    /// The first matching minute strictly after `after` (within four years).
    pub fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let mut t = after.with_second(0)?.with_nanosecond(0)? + chrono::Duration::minutes(1);
        for _ in 0..(4 * 366 * 24 * 60) {
            if self.matches(t) {
                return Some(t);
            }
            t += chrono::Duration::minutes(1);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    #[test]
    fn fields_lists_ranges_and_steps() {
        let c = CronSchedule::parse("*/15 9-17 * * 1-5").unwrap();
        // 2026-10-28 is a Wednesday.
        assert!(c.matches(at(2026, 10, 28, 9, 0)));
        assert!(c.matches(at(2026, 10, 28, 17, 45)));
        assert!(!c.matches(at(2026, 10, 28, 9, 10)));
        assert!(!c.matches(at(2026, 10, 28, 18, 0)));
        // Saturday.
        assert!(!c.matches(at(2026, 10, 31, 9, 0)));
        let c = CronSchedule::parse("5,35 0 1 1,7 *").unwrap();
        assert!(c.matches(at(2026, 7, 1, 0, 35)));
        assert!(!c.matches(at(2026, 8, 1, 0, 35)));
    }

    #[test]
    fn aliases_sunday_and_either_day_field() {
        assert!(CronSchedule::parse("@hourly")
            .unwrap()
            .matches(at(2026, 10, 28, 3, 0)));
        assert!(!CronSchedule::parse("@daily")
            .unwrap()
            .matches(at(2026, 10, 28, 3, 0)));
        // Sunday is 0 or 7.
        for s in ["0 0 * * 0", "0 0 * * 7"] {
            assert!(
                CronSchedule::parse(s)
                    .unwrap()
                    .matches(at(2026, 11, 1, 0, 0)),
                "{s}"
            );
        }
        // Both day fields restricted: the 1st, or any Monday.
        let c = CronSchedule::parse("0 0 1 * 1").unwrap();
        assert!(c.matches(at(2026, 11, 1, 0, 0)));
        assert!(c.matches(at(2026, 11, 2, 0, 0)));
        assert!(!c.matches(at(2026, 11, 3, 0, 0)));
    }

    #[test]
    fn next_runs_and_bad_input() {
        let c = CronSchedule::parse("30 2 * * *").unwrap();
        assert_eq!(
            c.next_after(at(2026, 10, 28, 2, 30)),
            Some(at(2026, 10, 29, 2, 30))
        );
        assert_eq!(
            c.next_after(at(2026, 10, 28, 1, 0)),
            Some(at(2026, 10, 28, 2, 30))
        );
        for bad in [
            "",
            "* * * *",
            "60 * * * *",
            "* 24 * * *",
            "*/0 * * * *",
            "5-1 * * * *",
            "a b c d e",
            "* * 0 * *",
            "* * * 13 *",
            "* * * * 8",
        ] {
            assert!(CronSchedule::parse(bad).is_err(), "{bad}");
        }
    }
}
