//! RFC 3339 UTC time stamps (ATH-DATA-002: times are RFC 3339 UTC strings).
//!
//! The representation is fixed by the specification, not by a date-time library; the
//! formatter is pure so any layer — including those without a persistence edge
//! (`runtimes/*`, whose §32.2 allow-list reaches `athena-types` only) — can stamp facts
//! without duplicating the rule.

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

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 → (year, month, day).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        if m <= 2 { y + 1 } else { y },
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
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
        // 2026-10-18T00:00:00Z == 1_792_262_400
        assert_eq!(
            format_rfc3339(1_792_281_600, 123),
            "2026-10-18T00:00:00.123Z"
        );
    }

    #[test]
    fn the_leap_day_is_respected() {
        // 2024-02-29T12:00:00Z == 1_709_203_200
        assert_eq!(format_rfc3339(1_709_208_000, 0), "2024-02-29T12:00:00.000Z");
    }
}
