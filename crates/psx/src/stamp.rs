//! Hashes and times for psx's records.

pub(crate) fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

/// The time now, UTC, as 2026-10-26T12:34:56Z.
pub(crate) fn utc_now() -> String {
    iso(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
}

/// Seconds since 1970 as 2026-10-26T12:34:56Z.
fn iso(secs: u64) -> String {
    let (days, rest) = ((secs / 86400) as i64, secs % 86400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest / 60 % 60, rest % 60)
}

#[cfg(test)]
mod tests {
    #[test]
    fn time_is_iso() {
        assert_eq!(super::iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::iso(951_782_400 + 3661), "2000-02-29T01:01:01Z");
    }
}
