//! Timestamp helpers with no external dependency. The workspace pins no `chrono`/`time`
//! crate (every persisted timestamp is just the ISO-8601 string `Date.toISOString()`
//! produces in the TypeScript source), so [`now_iso8601`] and [`is_zod_datetime`] implement
//! the two directions of that same format by hand rather than pulling one in for a single
//! formatter/validator pair.

use std::time::{SystemTime, UNIX_EPOCH};

/// Days-since-epoch → (year, month, day) in the proleptic Gregorian calendar. Howard
/// Hinnant's `civil_from_days` (public domain), the standard dependency-free algorithm for
/// this conversion.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// The current instant as `Date.toISOString()` would render it:
/// `YYYY-MM-DDTHH:mm:ss.sssZ`, always UTC, always millisecond precision.
pub fn now_iso8601() -> String {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = since_epoch.as_secs() as i64;
    let millis = since_epoch.subsec_millis();
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.{millis:03}Z")
}

/// Mirrors zod's default `z.string().datetime()` (no `offset`/`local` options, so only a
/// trailing `Z` is accepted, never `+00:00`): `YYYY-MM-DDTHH:mm:ss` with an optional
/// `.`-prefixed fractional-second run of one or more digits, then `Z`. Used to replay the
/// `.catch()` fields that degrade a malformed timestamp instead of failing the whole record
/// (`monitoringWakeAt`, `autoResumeAt` in `runs::store`).
pub fn is_zod_datetime(s: &str) -> bool {
    fn digits(bytes: &[u8], i: &mut usize, n: usize) -> bool {
        if *i + n > bytes.len() || !bytes[*i..*i + n].iter().all(u8::is_ascii_digit) {
            return false;
        }
        *i += n;
        true
    }

    let bytes = s.as_bytes();
    let mut i = 0;
    if !digits(bytes, &mut i, 4) || bytes.get(i) != Some(&b'-') {
        return false;
    }
    i += 1;
    if !digits(bytes, &mut i, 2) || bytes.get(i) != Some(&b'-') {
        return false;
    }
    i += 1;
    if !digits(bytes, &mut i, 2) || bytes.get(i) != Some(&b'T') {
        return false;
    }
    i += 1;
    if !digits(bytes, &mut i, 2) || bytes.get(i) != Some(&b':') {
        return false;
    }
    i += 1;
    if !digits(bytes, &mut i, 2) || bytes.get(i) != Some(&b':') {
        return false;
    }
    i += 1;
    if !digits(bytes, &mut i, 2) {
        return false;
    }
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    bytes.get(i) == Some(&b'Z') && i + 1 == bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_iso8601_round_trips_through_the_validator() {
        assert!(is_zod_datetime(&now_iso8601()));
    }

    #[test]
    fn now_iso8601_matches_the_expected_shape() {
        let s = now_iso8601();
        assert_eq!(s.len(), 24, "{s}");
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], "T");
        assert_eq!(&s[23..24], "Z");
    }

    #[test]
    fn civil_from_days_matches_a_known_epoch_date() {
        // 2026-08-16 is day (2026-01-01's day count + 227) — cross-checked against `date -d`.
        assert_eq!(civil_from_days(20_681), (2026, 8, 16));
    }

    #[test]
    fn is_zod_datetime_rejects_an_offset_and_a_missing_zone() {
        assert!(is_zod_datetime("2026-07-25T10:15:00.000Z"));
        assert!(
            is_zod_datetime("2026-07-25T10:15:00Z"),
            "fractional seconds are optional"
        );
        assert!(
            !is_zod_datetime("2026-07-25T10:15:00+00:00"),
            "offset form is rejected"
        );
        assert!(
            !is_zod_datetime("2026-07-25T10:15:00"),
            "missing Z is rejected"
        );
        assert!(!is_zod_datetime("not-a-date"));
    }
}
