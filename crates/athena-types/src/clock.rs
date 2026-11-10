//! RFC 3339 UTC time stamps (ATH-DATA-002: times are RFC 3339 UTC strings).
//!
//! The representation is fixed by the specification, not by a date-time library; the
//! formatter is pure so any layer — including those without a persistence edge
//! (`runtimes/*`, whose §32.2 allow-list reaches `athena-types` only) — can stamp facts
//! without duplicating the rule. The parser mirrors the formatter: it reads back the
//! exact shape `format_rfc3339` stamps (millisecond precision, `Z`), because the stall
//! detector's quiet-span arithmetic must read the store's own stamps back (X-37).

/// Format a Unix timestamp as RFC 3339 UTC with millisecond precision.
#[must_use]
pub fn format_rfc3339(unix_secs: u64, millis: u32) -> String {
    let days = i64::try_from(unix_secs / 86_400).unwrap_or(0);
    let secs_of_day = unix_secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3_600;
    let minute = (secs_of_day % 3_600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// The current time as an RFC 3339 UTC string (ATH-DATA-002).
#[must_use]
pub fn now_rfc3339() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format_rfc3339(now.as_secs(), now.subsec_millis())
}

/// Read back the RFC 3339 UTC stamp the formatter stamps, as Unix milliseconds.
///
/// Accepts exactly the production shape — `YYYY-MM-DDTHH:MM:SS.mmmZ` — plus the
/// no-fraction and second-fraction variants some earlier rows may carry; a leap day is
/// validated through the civil calendar. `None` for anything else: the detector never
/// guesses a stamp (never-invent), it falls back to `now`.
///
/// # Errors
/// This pure read-back never faults a caller; malformed text answers `None`.
#[must_use]
pub fn parse_rfc3339_millis(text: &str) -> Option<u64> {
    let text = text.trim();
    let bytes = text.as_bytes();
    // The fixed skeleton: 20 chars `YYYY-MM-DDTHH:MM:SS`, optional fraction, `Z`.
    if bytes.len() < 20 {
        return None;
    }
    if !(bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':')
    {
        return None;
    }
    let year: u64 = text.get(0..4)?.parse().ok()?;
    let month: u64 = text.get(5..7)?.parse().ok()?;
    let day: u64 = text.get(8..10)?.parse().ok()?;
    let hour: u64 = text.get(11..13)?.parse().ok()?;
    let minute: u64 = text.get(14..16)?.parse().ok()?;
    let second: u64 = text.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // Optional fraction: `.` + 1..=9 digits; the production stamp carries millisecond
    // precision, which is what the arithmetic reads back exactly.
    let mut rest = &text[19..];
    let mut millis: u64 = 0;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits = frac.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 || digits > 9 {
            return None;
        }
        let value: u64 = frac.get(..digits)?.parse().ok()?;
        // Scale to milliseconds: 3 digits pass through; fewer scale up; more truncate.
        millis = match digits {
            0 => 0,
            1 => value * 100,
            2 => value * 10,
            3 => value,
            _ => value / 10_u64.pow(u32::try_from(digits - 3).ok()?),
        };
        rest = frac.get(digits..)?;
    }
    if rest != "Z" {
        return None;
    }
    // Civil calendar check: the leap rule and the month lengths decide whether this day
    // exists — February 29th only on a leap year (`y % 400 == 0`, or `y % 4 == 0` and
    // `y % 100 != 0`), and no 30th of February in any year. A stamp the calendar does
    // not hold answers `None` here (never-invent: the detector never guesses a day).
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let day_count = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if day == 0 || day > day_count {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let secs = days
        .checked_mul(86_400)?
        .checked_add(i64::try_from(hour).ok()?.checked_mul(3_600)?)?
        .checked_add(i64::try_from(minute).ok()?.checked_mul(60)?)?
        .checked_add(i64::try_from(second).ok()?)?;
    let secs = u64::try_from(secs).ok()?;
    secs.checked_mul(1_000)?.checked_add(millis)
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 → (year, month, day).
fn civil_from_days(days: i64) -> (u64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let d = doy - (153 * mp + 2) / 5 + 1;
    (
        u64::try_from(if m <= 2 { y + 1 } else { y }).unwrap_or(0),
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

/// Howard Hinnant's `days_from_civil`: (year, month, day) → days since 1970-01-01,
/// the exact inverse of [`civil_from_days`]. Callers validate the day (the leap rule
/// and month lengths above); the formula itself is exact for any civil date.
fn days_from_civil(y: u64, m: u64, d: u64) -> i64 {
    let y = i64::try_from(y).unwrap_or(0);
    let (m, d) = (i64::try_from(m).unwrap_or(1), i64::try_from(d).unwrap_or(1));
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let dof = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + dof;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_formats_as_utc_millis() {
        assert_eq!(format_rfc3339(0, 0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn a_known_instant_formats_exactly() {
        // 2026-10-18T00:00:00Z == 1_792_281_600
        assert_eq!(
            format_rfc3339(1_792_281_600, 123),
            "2026-10-18T00:00:00.123Z"
        );
    }

    #[test]
    fn the_leap_day_is_respected() {
        // 2024-02-29T12:00:00Z == 1_709_208_000
        assert_eq!(format_rfc3339(1_709_208_000, 0), "2024-02-29T12:00:00.000Z");
    }

    #[test]
    fn a_production_stamp_parses_back_to_millis() {
        // 2026-11-12T00:00:00Z == day 20_769 == 1_794_441_600 s
        assert_eq!(
            parse_rfc3339_millis("2026-11-12T00:00:00.000Z"),
            Some(1_794_441_600_000)
        );
        assert_eq!(
            parse_rfc3339_millis("2026-11-12T00:00:00Z"),
            Some(1_794_441_600_000)
        );
    }

    #[test]
    fn the_leap_day_parses_and_a_fake_one_does_not() {
        // 2024-02-29T00:00:00Z == day 19_782 == 1_709_164_800 s
        assert_eq!(
            parse_rfc3339_millis("2024-02-29T00:00:00.000Z"),
            Some(1_709_164_800_000)
        );
        assert_eq!(parse_rfc3339_millis("2023-02-29T00:00:00.000Z"), None);
        assert_eq!(parse_rfc3339_millis("2026-13-01T00:00:00.000Z"), None);
    }

    #[test]
    fn fraction_precision_is_normalized_to_millis() {
        assert_eq!(
            parse_rfc3339_millis("2026-11-12T00:00:00.5Z"),
            Some(1_794_441_600_500)
        );
        assert_eq!(
            parse_rfc3339_millis("2026-11-12T00:00:00.999999Z"),
            Some(1_794_441_600_999)
        );
    }

    #[test]
    fn malformed_stamps_answer_none() {
        assert_eq!(parse_rfc3339_millis(""), None);
        assert_eq!(parse_rfc3339_millis("not-a-stamp"), None);
        assert_eq!(parse_rfc3339_millis("2026-11-12 00:00:00Z"), None);
        assert_eq!(parse_rfc3339_millis("2026-11-12T00:00:00.J"), None);
    }
}
