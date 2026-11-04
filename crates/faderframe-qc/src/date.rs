//! Dates in UTC for the report (no time zone database: a report says
//! UTC).

/// Year, month (1–12) and day of the days since 1970-01-01 (the civil
/// calendar, proleptic Gregorian; Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// A moment as seconds since the Unix epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Utc(pub u64);

impl Utc {
    pub fn now() -> Self {
        Self(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        )
    }

    fn parts(self) -> (i64, u32, u32, u64, u64, u64) {
        let days = (self.0 / 86_400) as i64;
        let rest = self.0 % 86_400;
        let (y, m, d) = civil(days);
        (y, m, d, rest / 3600, rest / 60 % 60, rest % 60)
    }

    /// "2026-10-29T14:03:12Z" (ISO 8601).
    pub fn iso(self) -> String {
        let (y, m, d, h, min, s) = self.parts();
        format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}Z")
    }

    /// "29 October 2026, 14:03 UTC".
    pub fn text(self) -> String {
        let (y, m, d, h, min, _) = self.parts();
        format!(
            "{d} {} {y}, {h:02}:{min:02} UTC",
            MONTHS[(m as usize).clamp(1, 12) - 1]
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_moments() {
        assert_eq!(Utc(0).iso(), "1970-01-01T00:00:00Z");
        // `date -u -d @1793282592`.
        assert_eq!(Utc(1_793_282_592).iso(), "2026-10-29T14:03:12Z");
        assert_eq!(Utc(1_793_282_592).text(), "29 October 2026, 14:03 UTC");
        // A leap day.
        assert_eq!(Utc(951_782_400).iso(), "2000-02-29T00:00:00Z");
        assert_eq!(Utc(4_107_542_399).iso(), "2100-02-28T23:59:59Z");
    }
}
