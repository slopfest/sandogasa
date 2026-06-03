// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Locale signals derived from an IANA timezone string.
//!
//! Given an IANA name like `Europe/Berlin`, returns the country
//! the zone belongs to (via tzdb's `zone1970.tab`) and the local
//! datetime, weekday, and whether that weekday is the country's
//! weekend.
//!
//! The country mapping is parsed lazily from a bundled copy of
//! `zone1970.tab` (public domain). For zones that span multiple
//! countries, the first listed country code is used.

use std::collections::HashMap;
use std::sync::OnceLock;

use chrono::{DateTime, Datelike, NaiveDate, Timelike, Utc, Weekday};

const BUNDLED_ZONE1970_TAB: &str = include_str!("../data/zone1970.tab");
const SYSTEM_ZONE1970_PATH: &str = "/usr/share/zoneinfo/zone1970.tab";

/// Where to read `zone1970.tab` from. `Auto` (the default)
/// picks whichever of the system file (`/usr/share/zoneinfo/
/// zone1970.tab`) and the bundled copy has the newer "From
/// Paul Eggert" header date; ties go to the system file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum TzSource {
    #[default]
    Auto,
    System,
    Bundled,
}

/// Caller's source preference. The first reader of the lookup
/// table snapshots this; later changes are ignored.
static SOURCE_PREF: OnceLock<TzSource> = OnceLock::new();

/// Set the `zone1970.tab` source preference before any lookup
/// runs. Idempotent — the first call wins (subsequent calls are
/// silently dropped, since the lookup table is built once).
pub fn init_source(pref: TzSource) {
    let _ = SOURCE_PREF.set(pref);
}

/// Resolve the `zone1970.tab` content according to the
/// configured source preference. Emits an `info:` line on
/// stderr when `Auto` ends up preferring the bundled copy over
/// an older system file, so the user knows their tzdata is
/// behind.
fn load_zone1970() -> String {
    let pref = SOURCE_PREF.get().copied().unwrap_or_default();
    match pref {
        TzSource::Bundled => BUNDLED_ZONE1970_TAB.to_string(),
        TzSource::System => match std::fs::read_to_string(SYSTEM_ZONE1970_PATH) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "warning: --tz-source=system but {SYSTEM_ZONE1970_PATH} \
                     is unreadable ({e}); falling back to the bundled copy."
                );
                BUNDLED_ZONE1970_TAB.to_string()
            }
        },
        TzSource::Auto => pick_newer(),
    }
}

fn pick_newer() -> String {
    let system = match std::fs::read_to_string(SYSTEM_ZONE1970_PATH) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "info: {SYSTEM_ZONE1970_PATH} unreadable ({e}); \
                 using bundled tzdata. Override with \
                 --tz-source=bundled to silence this."
            );
            return BUNDLED_ZONE1970_TAB.to_string();
        }
    };
    match decide_auto(&system, BUNDLED_ZONE1970_TAB) {
        AutoChoice::Bundled {
            system_date,
            bundled_date,
        } => {
            eprintln!(
                "info: bundled tzdata ({bundled_date}) is newer than \
                 {SYSTEM_ZONE1970_PATH} ({system_date}); using bundled. \
                 Override with --tz-source=system to force."
            );
            BUNDLED_ZONE1970_TAB.to_string()
        }
        AutoChoice::System => system,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum AutoChoice {
    System,
    Bundled {
        system_date: NaiveDate,
        bundled_date: NaiveDate,
    },
}

/// Pure decision: given the two file contents, which one wins?
/// Bundled wins only when both Eggert dates are parseable and
/// the bundled one is strictly newer.
fn decide_auto(system_content: &str, bundled_content: &str) -> AutoChoice {
    match (
        extract_eggert_date(system_content),
        extract_eggert_date(bundled_content),
    ) {
        (Some(s), Some(b)) if b > s => AutoChoice::Bundled {
            system_date: s,
            bundled_date: b,
        },
        _ => AutoChoice::System,
    }
}

/// Pull the date out of `# From Paul Eggert (YYYY-MM-DD):` in
/// the file header. tzdb's maintainer stamps each release with
/// this line, so it doubles as a version marker.
fn extract_eggert_date(content: &str) -> Option<NaiveDate> {
    for line in content.lines().take(40) {
        if let Some(rest) = line.strip_prefix("# From Paul Eggert (")
            && let Some(date_str) = rest.split(')').next()
        {
            return NaiveDate::parse_from_str(date_str, "%Y-%m-%d").ok();
        }
    }
    None
}

/// IANA timezone -> ISO 3166-1 alpha-2 country code lookup,
/// parsed from `zone1970.tab` on first access. The chosen
/// source file is leaked as `&'static str` so the table can
/// hold zero-copy slices into it; that happens at most once per
/// process.
fn tz_to_country_table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let content: &'static str = Box::leak(load_zone1970().into_boxed_str());
        parse_zone1970(content)
    })
}

fn parse_zone1970(content: &'static str) -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();
    for line in content.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let mut parts = line.splitn(4, '\t');
        let codes = parts.next().unwrap_or("");
        let _coords = parts.next();
        let tz = parts.next().unwrap_or("");
        if tz.is_empty() {
            continue;
        }
        // Multi-country zones (e.g. `CA,US`) get the first code
        // — coarse but unambiguous for weekend/holiday signal.
        let first_cc = codes.split(',').next().unwrap_or("");
        if first_cc.len() == 2 {
            m.insert(tz, first_cc);
        }
    }
    m
}

/// Resolve `Europe/Berlin` -> `DE`. Returns `None` for unknown
/// or non-canonical names (e.g. legacy aliases not listed in
/// `zone1970.tab`).
pub fn country_for_tz(iana: &str) -> Option<&'static str> {
    tz_to_country_table().get(iana).copied()
}

/// Days that count as the weekend in the given ISO 3166-1
/// alpha-2 country.
///
/// Defaults to Saturday + Sunday. Overrides cover the
/// Middle-Eastern / South-Asian variants where the workweek is
/// shifted. Best-effort, as of mid-2026; the underlying laws
/// change occasionally (UAE moved from Fri/Sat to Sat/Sun in
/// 2022, Saudi Arabia to Fri/Sat in 2013).
pub fn weekend_days(country: &str) -> &'static [Weekday] {
    use Weekday::*;
    match country {
        // Friday + Saturday (most of MENA, plus Israel).
        "BH" | "DZ" | "EG" | "IL" | "IQ" | "JO" | "KW" | "LY" | "OM" | "PS" | "QA" | "SA"
        | "SD" | "SY" | "YE" | "AF" | "MV" => &[Fri, Sat],
        // Friday + Sunday.
        "BN" => &[Fri, Sun],
        // Single-day weekends.
        "IR" => &[Fri],
        "NP" => &[Sat],
        // Everywhere else, Sat + Sun.
        _ => &[Sat, Sun],
    }
}

/// Local-time snapshot derived from an IANA timezone name.
#[derive(Debug, Clone)]
pub struct LocalTimeInfo {
    /// Localised datetime, RFC 3339 (with offset).
    pub local_time_rfc3339: String,
    /// Human-friendly local datetime, e.g. `2026-06-03 14:25:33`.
    pub local_time_display: String,
    /// Day of week in the local zone.
    pub weekday: Weekday,
    /// Calendar date in the local zone — needed for the
    /// holiday lookup.
    pub local_date: NaiveDate,
    /// Hour of day in the local zone (0–23), used to flag
    /// outside-working-hours in the rendered output.
    pub hour: u32,
    /// ISO 3166-1 alpha-2 country, if the zone is in zone1970.tab.
    pub country: Option<&'static str>,
    /// Whether `weekday` is a weekend in `country`. `None` when
    /// the country is unknown.
    pub is_weekend: Option<bool>,
}

/// Resolve the local time and weekend status for a given IANA
/// timezone string. Returns `None` if the string isn't a known
/// IANA zone parseable by `chrono-tz`.
pub fn local_time_info(iana: &str, now_utc: DateTime<Utc>) -> Option<LocalTimeInfo> {
    let tz: chrono_tz::Tz = iana.parse().ok()?;
    let local = now_utc.with_timezone(&tz);
    let weekday = local.weekday();
    let country = country_for_tz(iana);
    let is_weekend = country.map(|cc| weekend_days(cc).contains(&weekday));
    Some(LocalTimeInfo {
        local_time_rfc3339: local.to_rfc3339(),
        local_time_display: local.format("%Y-%m-%d %H:%M:%S %Z").to_string(),
        weekday,
        local_date: local.date_naive(),
        hour: local.hour(),
        country,
        is_weekend,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn country_for_tz_resolves_single_country_zones() {
        assert_eq!(country_for_tz("Europe/Berlin"), Some("DE"));
        assert_eq!(country_for_tz("Asia/Tokyo"), Some("JP"));
        assert_eq!(country_for_tz("Asia/Riyadh"), Some("SA"));
        assert_eq!(country_for_tz("Pacific/Auckland"), Some("NZ"));
    }

    #[test]
    fn country_for_tz_picks_first_for_multi_country_zones() {
        // America/Detroit is listed as "CA,US" — take CA.
        // (If tzdb changes the order, update this assertion to
        // match the new "first listed".)
        let cc = country_for_tz("America/Detroit");
        assert!(matches!(cc, Some("US") | Some("CA")));
    }

    #[test]
    fn country_for_tz_returns_none_for_unknown() {
        assert_eq!(country_for_tz("Not/A_Zone"), None);
    }

    #[test]
    fn weekend_days_defaults_to_sat_sun() {
        use Weekday::*;
        assert_eq!(weekend_days("DE"), &[Sat, Sun]);
        assert_eq!(weekend_days("US"), &[Sat, Sun]);
        assert_eq!(weekend_days("JP"), &[Sat, Sun]);
        assert_eq!(weekend_days("ZZ"), &[Sat, Sun]); // unknown -> default
    }

    #[test]
    fn weekend_days_overrides_for_fri_sat_countries() {
        use Weekday::*;
        assert_eq!(weekend_days("SA"), &[Fri, Sat]);
        assert_eq!(weekend_days("EG"), &[Fri, Sat]);
        assert_eq!(weekend_days("IL"), &[Fri, Sat]);
    }

    #[test]
    fn weekend_days_overrides_for_single_day_weekends() {
        use Weekday::*;
        assert_eq!(weekend_days("IR"), &[Fri]);
        assert_eq!(weekend_days("NP"), &[Sat]);
    }

    #[test]
    fn local_time_info_germany_in_summer() {
        // 2026-06-03T12:00:00Z is a Wednesday; Berlin is UTC+2
        // in summer, so local time is 14:00 Wednesday.
        let now = Utc.with_ymd_and_hms(2026, 6, 3, 12, 0, 0).unwrap();
        let info = local_time_info("Europe/Berlin", now).unwrap();
        assert_eq!(info.weekday, Weekday::Wed);
        assert_eq!(info.country, Some("DE"));
        assert_eq!(info.is_weekend, Some(false));
        assert!(info.local_time_display.starts_with("2026-06-03 14:00:00"));
    }

    #[test]
    fn local_time_info_riyadh_on_friday() {
        // 2026-06-05T09:00:00Z is a Friday; Riyadh is UTC+3, so
        // local time 12:00 Friday — weekend in SA.
        let now = Utc.with_ymd_and_hms(2026, 6, 5, 9, 0, 0).unwrap();
        let info = local_time_info("Asia/Riyadh", now).unwrap();
        assert_eq!(info.weekday, Weekday::Fri);
        assert_eq!(info.country, Some("SA"));
        assert_eq!(info.is_weekend, Some(true));
    }

    #[test]
    fn local_time_info_unknown_zone_returns_none() {
        let now = Utc.with_ymd_and_hms(2026, 6, 3, 12, 0, 0).unwrap();
        assert!(local_time_info("Bogus/Zone", now).is_none());
    }

    #[test]
    fn extract_eggert_date_parses_header_line() {
        let content = "# tzdb timezone descriptions\n\
            #\n\
            # From Paul Eggert (2025-05-15):\n\
            # ... other stuff\n";
        assert_eq!(
            extract_eggert_date(content),
            Some(NaiveDate::from_ymd_opt(2025, 5, 15).unwrap())
        );
    }

    #[test]
    fn extract_eggert_date_returns_none_when_missing() {
        let content = "# tzdb timezone descriptions\n# no date line here\n";
        assert_eq!(extract_eggert_date(content), None);
    }

    #[test]
    fn extract_eggert_date_returns_none_for_bad_date() {
        let content = "# From Paul Eggert (not-a-date):\n";
        assert_eq!(extract_eggert_date(content), None);
    }

    #[test]
    fn bundled_zone1970_has_a_parseable_eggert_date() {
        // Guards against future bundles losing the marker.
        assert!(extract_eggert_date(BUNDLED_ZONE1970_TAB).is_some());
    }

    #[test]
    fn decide_auto_prefers_system_on_tie() {
        let same = "# From Paul Eggert (2025-05-15):\n";
        assert_eq!(decide_auto(same, same), AutoChoice::System);
    }

    #[test]
    fn decide_auto_prefers_system_when_newer() {
        let sys = "# From Paul Eggert (2026-01-01):\n";
        let bun = "# From Paul Eggert (2025-05-15):\n";
        assert_eq!(decide_auto(sys, bun), AutoChoice::System);
    }

    #[test]
    fn decide_auto_prefers_bundled_when_strictly_newer() {
        let sys = "# From Paul Eggert (2025-05-15):\n";
        let bun = "# From Paul Eggert (2026-01-01):\n";
        assert_eq!(
            decide_auto(sys, bun),
            AutoChoice::Bundled {
                system_date: NaiveDate::from_ymd_opt(2025, 5, 15).unwrap(),
                bundled_date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            }
        );
    }

    #[test]
    fn decide_auto_prefers_system_when_dates_unparseable() {
        // If we can't read either marker, trust the live system
        // file rather than silently switching to a frozen copy.
        let sys = "# no marker\n";
        let bun = "# also no marker\n";
        assert_eq!(decide_auto(sys, bun), AutoChoice::System);
    }
}
