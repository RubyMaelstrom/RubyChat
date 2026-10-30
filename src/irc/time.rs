//! IRCv3 server-time: `YYYY-MM-DDThh:mm:ss.sssZ`, a UTC time of day in the
//! ISO 8601 extended format with millisecond precision.

use crate::platform::time::days_from_civil;

/// Milliseconds since the Unix epoch, or `None` if `value` is malformed.
pub fn parse_server_time(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<u32> {
        let digits = value.get(range)?;
        digits
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| digits.parse().ok())?
    };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut rest = &value[19..];
    let mut millis = 0;
    if let Some(fraction) = rest.strip_prefix('.') {
        let count = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if count == 0 {
            return None;
        }
        for (index, digit) in fraction.bytes().take(3).enumerate().take(count) {
            millis += i64::from(digit - b'0') * [100, 10, 1][index];
        }
        rest = &fraction[count..];
    }
    if rest != "Z" {
        return None;
    }
    let days = days_from_civil(year as i32, month as u8, day as u8);
    Some(((days * 86_400) + i64::from(hour * 3600 + minute * 60 + second)) * 1000 + millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_specified_format_and_rejects_others() {
        // The specification's example: Wed, 19 Oct 2011 16:40:51.620 UTC.
        assert_eq!(
            parse_server_time("2011-10-19T16:40:51.620Z"),
            Some(1_319_042_451_620)
        );
        assert_eq!(
            parse_server_time("2011-10-19T16:40:51Z"),
            Some(1_319_042_451_000)
        );
        assert_eq!(
            parse_server_time("2011-10-19T16:40:51.6Z"),
            Some(1_319_042_451_600)
        );
        for bad in [
            "",
            "2011-10-19 16:40:51.620Z",
            "2011-13-19T16:40:51Z",
            "2011-10-19T16:40:51.620",
            "2011-10-19T16:40:51.Z",
            "2011-1x-19T16:40:51Z",
        ] {
            assert_eq!(parse_server_time(bad), None, "{bad}");
        }
    }
}
