//! Wall-clock time: the current Unix time, and Unix times shown in the
//! local time zone.

use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

/// A moment in the local time zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i32,
    /// 1–12.
    pub month: u8,
    /// 1–31.
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// `unix_ms` in the local time zone.
#[cfg(unix)]
pub fn local(unix_ms: i64) -> LocalTime {
    let seconds = unix_ms.div_euclid(1000) as libc::time_t;
    // SAFETY: `localtime_r` writes only to the `tm` we pass; a zeroed `tm`
    // is a valid value of the plain C struct.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let converted = unsafe { libc::localtime_r(&seconds, &mut tm) };
    if converted.is_null() {
        return utc(unix_ms);
    }
    LocalTime {
        year: tm.tm_year + 1900,
        month: (tm.tm_mon + 1) as u8,
        day: tm.tm_mday as u8,
        hour: tm.tm_hour as u8,
        minute: tm.tm_min as u8,
        second: tm.tm_sec as u8,
    }
}

/// `unix_ms` in the active time zone, daylight saving time included
/// (`SystemTimeToTzSpecificLocalTime` with no zone given).
#[cfg(windows)]
pub fn local(unix_ms: i64) -> LocalTime {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::Time::SystemTimeToTzSpecificLocalTime;

    let universal = utc(unix_ms);
    // What a SYSTEMTIME can hold.
    if !(1601..=30827).contains(&universal.year) {
        return universal;
    }
    let from = SYSTEMTIME {
        wYear: universal.year as u16,
        wMonth: u16::from(universal.month),
        wDayOfWeek: 0,
        wDay: u16::from(universal.day),
        wHour: u16::from(universal.hour),
        wMinute: u16::from(universal.minute),
        wSecond: u16::from(universal.second),
        wMilliseconds: 0,
    };
    // SAFETY: a zeroed SYSTEMTIME is a valid value of the plain C struct.
    let mut to: SYSTEMTIME = unsafe { std::mem::zeroed() };
    // SAFETY: both structs outlive the call; a null zone means the active
    // one.
    if unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &from, &mut to) } == 0 {
        return universal;
    }
    LocalTime {
        year: i32::from(to.wYear),
        month: to.wMonth as u8,
        day: to.wDay as u8,
        hour: to.wHour as u8,
        minute: to.wMinute as u8,
        second: to.wSecond as u8,
    }
}

/// `unix_ms` in UTC where there is no time zone to read.
#[cfg(not(any(unix, windows)))]
pub fn local(unix_ms: i64) -> LocalTime {
    utc(unix_ms)
}

/// `unix_ms` in UTC (Howard Hinnant's `civil_from_days`).
pub fn utc(unix_ms: i64) -> LocalTime {
    let seconds = unix_ms.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let of_day = seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
    LocalTime {
        year,
        month,
        day,
        hour: (of_day / 3600) as u8,
        minute: (of_day / 60 % 60) as u8,
        second: (of_day % 60) as u8,
    }
}

/// Days since the Unix epoch of a proleptic Gregorian date (Hinnant's
/// `days_from_civil`).
pub fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let month = i64::from(month);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        let moment =
            (days_from_civil(2026, 10, 30) * 86_400 + 8 * 3600 + 34 * 60 + 21) * 1000 + 321;
        assert_eq!(
            utc(moment),
            LocalTime {
                year: 2026,
                month: 10,
                day: 30,
                hour: 8,
                minute: 34,
                second: 21
            }
        );
        assert_eq!(utc(-1000).year, 1969);
        assert!(now_ms() > moment - 86_400_000 * 365);
    }
}
