//! A minimal timestamp for `state.json`.
//!
//! The C# models store `DateTime` values, which System.Text.Json writes as ISO 8601 text
//! (`2026-11-06T12:00:00Z`, with fractional seconds when present, no suffix for an
//! `Unspecified` kind such as `default(DateTime)` = `0001-01-01T00:00:00`). We keep the text
//! verbatim so a load/save round trip does not rewrite it, and parse it to epoch milliseconds
//! only when ordering or ageing needs a number.

use serde::{Deserialize, Serialize};

/// The text C# writes for `default(DateTime)`: the "never" sentinel.
pub const DEFAULT_TEXT: &str = "0001-01-01T00:00:00";

/// Epoch milliseconds of `0001-01-01T00:00:00Z`.
pub(crate) const NEVER_EPOCH_MS: i64 = -62_135_596_800_000;

/// An ISO 8601 timestamp, kept as text. See the module docs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Timestamp(String);

impl Default for Timestamp {
    fn default() -> Self {
        Timestamp(DEFAULT_TEXT.to_owned())
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // C# parity: an explicit null leaves a DateTime at default(DateTime).
        let v = Option::<String>::deserialize(d)?;
        Ok(v.map_or_else(Timestamp::default, Timestamp))
    }
}

impl Timestamp {
    /// The "never" sentinel (`default(DateTime)`).
    pub fn never() -> Self {
        Self::default()
    }

    /// Wraps text as-is. It need not be valid; [`Timestamp::epoch_ms`] says so.
    pub fn from_text(text: impl Into<String>) -> Self {
        Timestamp(text.into())
    }

    /// Formats `epoch_ms` as `YYYY-MM-DDTHH:MM:SS[.fff]Z`.
    pub fn from_epoch_ms(epoch_ms: i64) -> Self {
        let days = epoch_ms.div_euclid(86_400_000);
        let ms_of_day = epoch_ms.rem_euclid(86_400_000);
        let (y, m, d) = civil_from_days(days);
        let secs = ms_of_day / 1000;
        let ms = ms_of_day % 1000;
        let (hh, mm, ss) = (secs / 3600, (secs / 60) % 60, secs % 60);
        let frac = if ms == 0 { String::new() } else { format!(".{ms:03}") };
        Timestamp(format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}{frac}Z"))
    }

    /// The stored text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True for `default(DateTime)` (year 1) and for text that is not a timestamp at all.
    pub fn is_never(&self) -> bool {
        self.epoch_ms().is_none_or(|ms| ms <= NEVER_EPOCH_MS)
    }

    /// Milliseconds since the Unix epoch, or `None` if the text is not a timestamp. Text
    /// without an offset is read as UTC (this is how `default(DateTime)` is ordered).
    pub fn epoch_ms(&self) -> Option<i64> {
        parse_iso(&self.0)
    }
}

// Howard Hinnant's civil-date algorithms (public domain).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn parse_iso(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let num = |from: usize, len: usize| -> Option<i64> {
        let part = b.get(from..from.checked_add(len)?)?;
        if !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(part).ok()?.parse().ok()
    };
    let y = num(0, 4)?;
    let date_ok = b.get(4) == Some(&b'-') && b.get(7) == Some(&b'-');
    let sep_ok = matches!(b.get(10), Some(b'T' | b't' | b' '));
    if !date_ok || !sep_ok || b.get(13) != Some(&b':') || b.get(16) != Some(&b':') {
        return None;
    }
    let (m, d) = (num(5, 2)?, num(8, 2)?);
    let (hh, mi, ss) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mi > 59 || ss > 60 {
        return None;
    }
    let mut i = 19;
    let mut ms = 0i64;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        let mut scale = 100i64;
        while let Some(c) = b.get(i).filter(|c| c.is_ascii_digit()) {
            // Only the first three fractional digits matter at millisecond resolution.
            ms += i64::from(c - b'0') * scale;
            scale /= 10;
            i += 1;
        }
        if i == start {
            return None;
        }
    }
    let mut offset_ms = 0i64;
    match b.get(i) {
        None | Some(b'Z' | b'z') => {}
        Some(&sign @ (b'+' | b'-')) => {
            let oh = num(i + 1, 2)?;
            let om = if b.get(i + 3) == Some(&b':') { num(i + 4, 2)? } else { num(i + 3, 2).unwrap_or(0) };
            let total = (oh * 60 + om) * 60_000;
            offset_ms = if sign == b'+' { total } else { -total };
        }
        Some(_) => return None,
    }
    let days = days_from_civil(y, m, d);
    Some(days * 86_400_000 + ((hh * 60 + mi) * 60 + ss) * 1000 + ms - offset_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_epoch_milliseconds() {
        for ms in [0, 1_762_430_400_000, 1_762_430_400_123, -1, 951_782_400_000] {
            assert_eq!(Timestamp::from_epoch_ms(ms).epoch_ms(), Some(ms), "{ms}");
        }
        assert_eq!(Timestamp::from_epoch_ms(1_762_430_400_000).as_str(), "2025-11-06T12:00:00Z");
    }

    #[test]
    fn default_is_the_never_sentinel_and_orders_first() {
        let never = Timestamp::never();
        assert!(never.is_never());
        assert!(never.epoch_ms() < Timestamp::from_epoch_ms(0).epoch_ms());
        assert!(!Timestamp::from_epoch_ms(0).is_never());
    }

    #[test]
    fn parses_csharp_shapes() {
        // 7-digit fractional seconds and an explicit offset, as DateTime.Now serialises.
        let utc = Timestamp::from_text("2026-11-06T12:00:00.1234567Z").epoch_ms();
        let plus1 = Timestamp::from_text("2026-11-06T13:00:00.1234567+01:00").epoch_ms();
        assert_eq!(utc, plus1);
        assert_eq!(utc.map(|v| v % 1000), Some(123));
        assert_eq!(Timestamp::from_text("garbage").epoch_ms(), None);
        assert!(Timestamp::from_text("garbage").is_never());
    }

    #[test]
    fn hostile_text_does_not_panic() {
        for s in ["", "2026", "2026-13-40T99:99:99", "9999-12-31T23:59:59.9999999+99:99", "2026-01-01T00:00:00.", "é"] {
            let _ = Timestamp::from_text(s).epoch_ms();
        }
    }
}
