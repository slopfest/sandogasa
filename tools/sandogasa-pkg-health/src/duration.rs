// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Small parser for relative-duration strings like "7d", "24h".

use chrono::Duration;

/// Parse a duration string of the form "<n><unit>" where unit is one
/// of `s` (seconds), `m` (minutes), `h` (hours), `d` (days), or
/// `w` (weeks).
///
/// ```
/// use chrono::Duration;
/// use sandogasa_pkg_health::duration::parse;
///
/// assert_eq!(parse("7d").unwrap(), Duration::days(7));
/// assert_eq!(parse("24h").unwrap(), Duration::hours(24));
/// assert_eq!(parse("4w").unwrap(), Duration::weeks(4));
/// assert!(parse("").is_err());
/// assert!(parse("abc").is_err());
/// assert!(parse("7x").is_err());
/// ```
pub fn parse(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty duration".to_string());
    }
    let (num_part, unit) = s.split_at(s.len() - 1);
    let n: i64 = num_part
        .parse()
        .map_err(|_| format!("invalid duration '{s}': expected <number><unit>"))?;
    match unit {
        "s" => Ok(Duration::seconds(n)),
        "m" => Ok(Duration::minutes(n)),
        "h" => Ok(Duration::hours(n)),
        "d" => Ok(Duration::days(n)),
        "w" => Ok(Duration::weeks(n)),
        _ => Err(format!(
            "invalid unit '{unit}' in '{s}': expected s/m/h/d/w"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_days() {
        assert_eq!(parse("7d").unwrap(), Duration::days(7));
    }

    #[test]
    fn parse_hours() {
        assert_eq!(parse("24h").unwrap(), Duration::hours(24));
    }

    #[test]
    fn parse_minutes() {
        assert_eq!(parse("30m").unwrap(), Duration::minutes(30));
    }

    #[test]
    fn parse_seconds() {
        assert_eq!(parse("60s").unwrap(), Duration::seconds(60));
    }

    #[test]
    fn parse_weeks() {
        assert_eq!(parse("2w").unwrap(), Duration::weeks(2));
    }

    #[test]
    fn parse_trims_whitespace() {
        assert_eq!(parse("  7d  ").unwrap(), Duration::days(7));
    }

    #[test]
    fn parse_empty_errors() {
        assert!(parse("").is_err());
        assert!(parse("   ").is_err());
    }

    #[test]
    fn parse_no_unit_errors() {
        assert!(parse("7").is_err());
    }

    #[test]
    fn parse_bad_unit_errors() {
        let err = parse("7x").unwrap_err();
        assert!(err.contains("invalid unit"));
    }

    #[test]
    fn parse_bad_number_errors() {
        assert!(parse("abc").is_err());
    }

    #[test]
    fn parse_zero() {
        assert_eq!(parse("0d").unwrap(), Duration::zero());
    }
}
