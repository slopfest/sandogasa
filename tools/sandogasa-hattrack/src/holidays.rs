// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Public-holiday lookup via the Nager.Date public API
//! (<https://date.nager.at>) with a per-year, per-country
//! on-disk cache.
//!
//! The cache lives under `$XDG_CACHE_HOME/sandogasa-hattrack/
//! holidays/` (or the platform equivalent — `dirs::cache_dir`
//! handles that). Cached files are tiny JSON blobs keyed by
//! `{CC}-{YEAR}.json`, e.g. `IE-2026.json`. They live forever:
//! holidays for a given year are stable once published, and
//! `--refresh-holidays` lets the user force-refetch if they
//! ever need to.
//!
//! Only nationwide holidays (`global: true` in the Nager.Date
//! response) are surfaced — we only know the country, not the
//! subdivision, so a regional holiday could mislead.

use std::path::PathBuf;

use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};

const NAGER_BASE_URL: &str = "https://date.nager.at/api/v3";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NagerHoliday {
    /// `YYYY-MM-DD` in the country's local calendar.
    pub date: String,
    #[serde(rename = "localName")]
    pub local_name: String,
    /// English / canonical name. We render this rather than
    /// `local_name` because callers may not read the local
    /// script.
    pub name: String,
    #[serde(rename = "countryCode")]
    pub country_code: String,
    /// True for nationwide holidays, false for regional ones.
    pub global: bool,
}

/// One nationwide holiday match.
///
/// `name` is the English / canonical name; `local_name` is the
/// holiday's name in the country's own language. They're often
/// the same string (e.g. "New Year's Day" / "New Year's Day"),
/// in which case the renderer collapses the duplicate.
#[derive(Debug, Clone, Serialize)]
pub struct HolidayEntry {
    pub name: String,
    pub local_name: String,
}

/// Return the nationwide holidays that fall on `date` in the
/// given ISO 3166-1 alpha-2 country. Empty on any error
/// (unknown country, network failure, parse failure) — holidays
/// are an enrichment, never load-bearing.
pub async fn holidays_for(cc: &str, date: NaiveDate, refresh: bool) -> Vec<HolidayEntry> {
    let cc = cc.to_uppercase();
    let year = date.year();
    let all = match load_or_fetch(&cc, year, refresh).await {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let date_str = date.format("%Y-%m-%d").to_string();
    all.into_iter()
        .filter(|h| h.global && h.date == date_str)
        .map(|h| HolidayEntry {
            name: h.name,
            local_name: h.local_name,
        })
        .collect()
}

/// Format a holiday for the human-readable output: collapses
/// the local name when it duplicates the canonical name.
pub fn format_holiday(entry: &HolidayEntry) -> String {
    if entry.local_name.is_empty() || entry.local_name == entry.name {
        entry.name.clone()
    } else {
        format!("{} ({})", entry.name, entry.local_name)
    }
}

/// Read the year's holiday list from disk, falling back to a
/// Nager.Date fetch when the cache is missing or `refresh` is
/// set.
async fn load_or_fetch(cc: &str, year: i32, refresh: bool) -> Result<Vec<NagerHoliday>, String> {
    let path = cache_path(cc, year);
    if !refresh
        && let Some(ref p) = path
        && p.exists()
        && let Ok(content) = std::fs::read_to_string(p)
        && let Ok(parsed) = serde_json::from_str::<Vec<NagerHoliday>>(&content)
    {
        return Ok(parsed);
    }
    let fetched = fetch_from_nager(cc, year).await?;
    if let Some(p) = path {
        let _ = save_cache(&p, &fetched);
    }
    Ok(fetched)
}

async fn fetch_from_nager(cc: &str, year: i32) -> Result<Vec<NagerHoliday>, String> {
    let url = format!("{NAGER_BASE_URL}/PublicHolidays/{year}/{cc}");
    let resp = reqwest::get(&url).await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("Nager.Date returned HTTP {}", resp.status()));
    }
    resp.json::<Vec<NagerHoliday>>()
        .await
        .map_err(|e| e.to_string())
}

/// Resolve `$XDG_CACHE_HOME/sandogasa-hattrack/holidays/
/// {CC}-{YEAR}.json`. Returns `None` if no cache directory can
/// be determined (e.g. no `HOME`, weird headless env) — the
/// lookup still works, just without caching.
fn cache_path(cc: &str, year: i32) -> Option<PathBuf> {
    let mut p = dirs::cache_dir()?;
    p.push("sandogasa-hattrack");
    p.push("holidays");
    p.push(format!("{cc}-{year}.json"));
    Some(p)
}

fn save_cache(path: &std::path::Path, holidays: &[NagerHoliday]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string(holidays)?;
    std::fs::write(path, content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_path_uppercases_country_code() {
        // Caller is responsible for uppercasing, so this just
        // confirms the filename uses what it got.
        let p = cache_path("IE", 2026).unwrap();
        assert!(p.to_string_lossy().ends_with("IE-2026.json"));
    }

    #[test]
    fn nager_holiday_parses_canonical_shape() {
        // Sample from
        // https://date.nager.at/api/v3/PublicHolidays/2026/IE
        let json = r#"[{
            "date": "2026-03-17",
            "localName": "Saint Patrick's Day",
            "name": "Saint Patrick's Day",
            "countryCode": "IE",
            "fixed": true,
            "global": true,
            "counties": null,
            "launchYear": null,
            "types": ["Public"]
        }]"#;
        let parsed: Vec<NagerHoliday> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].date, "2026-03-17");
        assert_eq!(parsed[0].name, "Saint Patrick's Day");
        assert_eq!(parsed[0].country_code, "IE");
        assert!(parsed[0].global);
    }

    #[test]
    fn format_holiday_collapses_duplicate_names() {
        let e = HolidayEntry {
            name: "New Year's Day".into(),
            local_name: "New Year's Day".into(),
        };
        assert_eq!(format_holiday(&e), "New Year's Day");
    }

    #[test]
    fn format_holiday_shows_local_when_different() {
        let e = HolidayEntry {
            name: "Saint Patrick's Day".into(),
            local_name: "Lá Fhéile Pádraig".into(),
        };
        assert_eq!(
            format_holiday(&e),
            "Saint Patrick's Day (Lá Fhéile Pádraig)"
        );
    }

    #[test]
    fn format_holiday_handles_empty_local() {
        let e = HolidayEntry {
            name: "Foo".into(),
            local_name: String::new(),
        };
        assert_eq!(format_holiday(&e), "Foo");
    }

    #[test]
    fn nager_holiday_serializes_back() {
        // Round-trip so the on-disk cache stays readable.
        let h = NagerHoliday {
            date: "2026-01-01".to_string(),
            local_name: "Lá Caille".to_string(),
            name: "New Year's Day".to_string(),
            country_code: "IE".to_string(),
            global: true,
        };
        let s = serde_json::to_string(&[&h]).unwrap();
        assert!(s.contains("\"date\":\"2026-01-01\""));
        assert!(s.contains("\"countryCode\":\"IE\""));
    }
}
