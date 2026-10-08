use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};

pub fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// `30d`, `12h` or `2w` as seconds.
pub fn parse_duration(spec: &str) -> Option<i64> {
    let spec = spec.trim();
    [("h", 3600), ("d", 86_400), ("w", 604_800)].into_iter().find_map(|(suffix, mul)| {
        let n = spec.strip_suffix(suffix)?.parse::<i64>().ok().filter(|n| *n >= 0)?;
        Some(n * mul)
    })
}

/// UTC `YYYY-MM-DDTHH:MM:SSZ`, comparable as text with the timestamps in session files.
pub fn iso(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from day count (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// `YYYYMMDDTHHMMSSZ`: sortable and safe in file names.
pub fn stamp(secs: i64) -> String {
    iso(secs).replace(['-', ':'], "")
}

/// A point in time given as an age (`7d`) or a date (`2026-01-31`), as the ISO string
/// sessions are compared against.
pub fn parse_cutoff(spec: &str, now: i64) -> Result<String> {
    if let Some(d) = parse_duration(spec) {
        return Ok(iso(now - d));
    }
    let b = spec.as_bytes();
    let shape = b.len() == 10
        && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() });
    let month = spec.get(5..7).and_then(|m| m.parse::<u32>().ok()).unwrap_or(0);
    let day = spec.get(8..10).and_then(|d| d.parse::<u32>().ok()).unwrap_or(0);
    if shape && (1..=12).contains(&month) && (1..=31).contains(&day) {
        return Ok(format!("{spec}T00:00:00Z"));
    }
    bail!("cannot parse '{spec}': use an age like 7d, 12h, 2w or a date like 2026-01-31")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_utc_timestamps() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(iso(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn stamps_are_compact_and_sortable() {
        assert_eq!(stamp(1_700_000_000), "20231114T221320Z");
        assert!(stamp(1_700_000_000) < stamp(1_700_000_001));
    }

    #[test]
    fn parses_durations_and_dates() {
        assert_eq!(parse_duration("30d"), Some(30 * 86_400));
        assert_eq!(parse_duration("12h"), Some(43_200));
        assert_eq!(parse_duration("2w"), Some(1_209_600));
        assert_eq!(parse_duration("d"), None);
        assert_eq!(parse_duration("-1d"), None);
        assert_eq!(parse_cutoff("1d", 1_700_000_000 + 86_400).unwrap(), "2023-11-14T22:13:20Z");
        assert_eq!(parse_cutoff("2026-01-31", 0).unwrap(), "2026-01-31T00:00:00Z");
        assert!(parse_cutoff("2026-13-01", 0).is_err());
        assert!(parse_cutoff("soon", 0).is_err());
    }
}
