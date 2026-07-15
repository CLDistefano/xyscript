//! Wall-clock decomposition for the date/time special variables (AutoIt-style `$YEAR`,
//! `$MON`, `$MDAY`, `$HOUR`, `$MIN`, `$SEC`, `$MSEC`, `$WDAY`, `$YDAY`, plus `$DA`/`$TM`)
//! and the stopwatch. Pure integer civil-date math (Howard Hinnant's algorithms) — no date
//! crate. LOCAL time is the caller's UTC value shifted by the offset configured in
//! [`crate::dir`] (`set_local_offset`).

/// Broken-down LOCAL date/time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    /// Full year (e.g. 2026).
    pub year: i64,
    /// Month, 1–12.
    pub month: u32,
    /// Day of month, 1–31.
    pub mday: u32,
    /// Hour, 0–23.
    pub hour: u32,
    /// Minute, 0–59.
    pub min: u32,
    /// Second, 0–59.
    pub sec: u32,
    /// Millisecond, 0–999.
    pub msec: u32,
    /// Day of week, 1–7 (Sunday–Saturday), per AutoIt's `@WDAY`.
    pub wday: u32,
    /// Day of year, 1–366.
    pub yday: u32,
}

/// Decompose `unix_millis` (UTC) shifted by `offset_secs` (east of UTC) into LOCAL fields.
pub fn fields_from_millis(unix_millis: i64, offset_secs: i64) -> DateTime {
    let local_ms = unix_millis + offset_secs * 1000;
    let secs = local_ms.div_euclid(1000);
    let msec = local_ms.rem_euclid(1000) as u32;
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (hour, min, sec) = ((tod / 3600) as u32, ((tod % 3600) / 60) as u32, (tod % 60) as u32);
    let (year, month, mday) = civil_from_days(days);
    // 1970-01-01 was a Thursday; `(days + 4) mod 7` gives 0 = Sunday. AutoIt's WDAY is
    // 1–7 Sunday–Saturday, so add 1.
    let wday = ((days + 4).rem_euclid(7) + 1) as u32;
    let yday = (days - days_from_civil(year, 1, 1) + 1) as u32;
    DateTime { year, month, mday, hour, min, sec, msec, wday, yday }
}

/// Current Unix time in milliseconds (UTC) from the system clock; 0 if the clock is before
/// the epoch.
pub fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Days since 1970-01-01 → (year, month, day). Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    let year = if month <= 2 { y + 1 } else { y };
    (year, month, day)
}

/// (year, month, day) → days since 1970-01-01. Hinnant's `days_from_civil` (used for the
/// day-of-year against Jan 1).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_decomposes_to_thursday_jan_1_1970() {
        let dt = fields_from_millis(0, 0);
        assert_eq!(
            dt,
            DateTime {
                year: 1970,
                month: 1,
                mday: 1,
                hour: 0,
                min: 0,
                sec: 0,
                msec: 0,
                wday: 5, // Thursday (Sun=1)
                yday: 1,
            }
        );
    }

    #[test]
    fn known_timestamp_with_millis() {
        // 2021-07-15 13:37:42.123 UTC (a Thursday; day 196 of 2021).
        let dt = fields_from_millis(1_626_356_262_123, 0);
        assert_eq!(
            dt,
            DateTime {
                year: 2021,
                month: 7,
                mday: 15,
                hour: 13,
                min: 37,
                sec: 42,
                msec: 123,
                wday: 5,
                yday: 196,
            }
        );
    }

    #[test]
    fn negative_offset_crosses_midnight_and_year() {
        // 2021-01-01 00:00:00 UTC minus 1h → 2020-12-31 23:00, a Thursday, day 366 (leap).
        let dt = fields_from_millis(1_609_459_200_000, -3600);
        assert_eq!(dt.year, 2020);
        assert_eq!((dt.month, dt.mday), (12, 31));
        assert_eq!((dt.hour, dt.min, dt.sec), (23, 0, 0));
        assert_eq!(dt.yday, 366); // 2020 is a leap year
        assert_eq!(dt.wday, 5); // Thursday
    }
}
