// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Launchpad reporting: the source packages a person uploaded to PPAs
//! in the window, by PPA, read anonymously from Launchpad's REST API.
//!
//! A person's own PPAs come from `~<login>/ppas`; team PPAs are named in
//! the domain's `launchpad_ppas`, since Launchpad has no call listing
//! every archive a person uploaded to. Each PPA's uploads come from its
//! `getPublishedSources`, newest first from the start of the window. A
//! PPA can hold other people's uploads (a team's members, or those an
//! owner gave upload rights to), so an upload counts as the person's
//! when they signed it or created it (a copy); in their own PPAs, one
//! with neither recorded counts too. Private PPAs cannot be read
//! anonymously, and a PPA that does not answer cannot be read at all:
//! both are left out, by name, and the rest are reported.
//!
//! The API takes a start date but no end, so a report on a past window
//! pages through every upload since its start.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::forge;

/// Where Launchpad's API answers.
pub const DEFAULT_INSTANCE: &str = "https://api.launchpad.net/devel";

/// How many PPAs are read at once.
const PARALLEL: usize = 4;

/// How many entries one page asks for (Launchpad's largest is 300).
const PAGE_SIZE: usize = 300;

/// A person's PPA uploads in a window.
#[derive(Debug, Default, Serialize)]
pub struct LaunchpadReport {
    /// The Launchpad login reported on.
    pub user: String,
    /// Uploads in the window, by PPA (`~owner/distribution/name`) and
    /// newest first within each.
    pub uploads: BTreeMap<String, Vec<Upload>>,
    /// PPAs left out, each with why: private, or not readable (a team
    /// PPA that is private, gone, or failed to answer).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub left_out: Vec<String>,
}

/// One source package uploaded to a PPA.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Upload {
    pub package: String,
    pub version: String,
    /// The distribution series it was built for, e.g. `noble`.
    pub series: String,
    pub date: NaiveDate,
    /// Launchpad's publishing status: `Published`, `Superseded`, ...
    pub status: String,
    /// The PPA's page listing the package.
    pub url: String,
}

/// A PPA as `~<login>/ppas` or the archive itself answers.
#[derive(Debug, Clone, Deserialize)]
struct Archive {
    self_link: String,
    web_link: String,
    #[serde(default)]
    private: bool,
}

/// One page of a collection.
#[derive(Debug, Deserialize)]
struct Page<T> {
    entries: Vec<T>,
    #[serde(default)]
    next_collection_link: Option<String>,
}

/// One published source, as `getPublishedSources` answers.
#[derive(Debug, Deserialize)]
struct Source {
    source_package_name: String,
    source_package_version: String,
    #[serde(default)]
    status: String,
    date_created: DateTime<chrono::FixedOffset>,
    #[serde(default)]
    distro_series_link: String,
    #[serde(default)]
    package_signer_link: Option<String>,
    #[serde(default)]
    package_creator_link: Option<String>,
}

/// The uploads `login` made in the window to their own PPAs and to the
/// PPAs named in `extra` (`~team/ubuntu/name`), from the API at `base`.
pub fn launchpad_report(
    base: &str,
    login: &str,
    extra: &[String],
    since: NaiveDate,
    until: NaiveDate,
    verbose: bool,
) -> Result<LaunchpadReport, String> {
    let base = base.trim_end_matches('/');
    let login = login.trim().trim_start_matches('~');
    let http = sandogasa_cli::http::blocking_builder("sandogasa-report")
        .build()
        .map_err(|e| format!("HTTP client: {e}"))?;
    let get = |url: &str| -> Result<serde_json::Value, String> {
        if verbose {
            eprintln!("[launchpad] GET {url}");
        }
        let resp = http
            .get(url)
            .header("Accept", "application/json")
            .send()
            .map_err(|e| format!("GET {url}: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("GET {url}: HTTP {status}"));
        }
        resp.json().map_err(|e| format!("GET {url}: {e}"))
    };

    eprintln!("Reading the Launchpad PPAs of ~{login}...");
    let mut archives: Vec<(String, Archive, bool)> = Vec::new();
    for archive in pages::<Archive>(&get, &format!("{base}/~{login}/ppas"))? {
        let name = reference(&archive.self_link);
        archives.push((name, archive, true));
    }
    // Team PPAs, each checked as a name and read a few at a time.
    let mut wanted: Vec<(String, String)> = Vec::new();
    for ppa in extra {
        let name = ppa.trim().trim_start_matches('~').to_string();
        let parts: Vec<&str> = name.split('/').collect();
        if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
            return Err(format!(
                "`{ppa}` in launchpad_ppas is not a PPA: give ~owner/distribution/name"
            ));
        }
        let reference = format!("~{name}");
        if archives.iter().any(|(n, _, _)| *n == reference)
            || wanted.iter().any(|(n, _)| *n == reference)
        {
            continue;
        }
        let url = format!("{base}/~{}/+archive/{}/{}", parts[0], parts[1], parts[2]);
        wanted.push((reference, url));
    }
    let mut left_out: Vec<String> = Vec::new();
    let fetched = forge::in_parallel(&wanted, PARALLEL, |(_, url)| {
        get(url).and_then(|v| {
            serde_json::from_value::<Archive>(v).map_err(|e| format!("GET {url}: {e}"))
        })
    });
    for ((reference, _), archive) in wanted.into_iter().zip(fetched) {
        match archive {
            Ok(a) => archives.push((reference, a, false)),
            Err(e) => left_out.push(format!("{reference} (not readable: {})", reason(&e))),
        }
    }

    let (private, readable): (Vec<_>, Vec<_>) =
        archives.into_iter().partition(|(_, a, _)| a.private);
    left_out.extend(
        private
            .into_iter()
            .map(|(n, _, _)| format!("{n} (private)")),
    );
    eprintln!(
        "Reading the uploads to {} PPA(s) since {since}...",
        readable.len()
    );
    let me = format!("/~{login}");
    let read = forge::in_parallel(&readable, PARALLEL, |(_, archive, own)| {
        let url = format!(
            "{}?ws.op=getPublishedSources&created_since_date={since}&order_by_date=true&ws.size={PAGE_SIZE}",
            archive.self_link
        );
        let sources = pages::<Source>(&get, &url)?;
        let uploads: Vec<Upload> = sources
            .into_iter()
            .filter(|s| {
                let links = [&s.package_signer_link, &s.package_creator_link];
                let mine = links
                    .iter()
                    .any(|l| l.as_deref().is_some_and(|l| l.ends_with(&me)));
                let nobodys = links.iter().all(|l| l.is_none());
                mine || (*own && nobodys)
            })
            .filter_map(|s| {
                let date = s.date_created.date_naive();
                (since..=until).contains(&date).then(|| Upload {
                    // A `+` in a query is a space.
                    url: format!(
                        "{}/+packages?field.name_filter={}",
                        archive.web_link.trim_end_matches('/'),
                        s.source_package_name.replace('+', "%2B")
                    ),
                    package: s.source_package_name,
                    version: s.source_package_version,
                    series: s
                        .distro_series_link
                        .rsplit('/')
                        .next()
                        .unwrap_or_default()
                        .to_string(),
                    date,
                    status: s.status,
                })
            })
            .collect();
        Ok::<_, String>(uploads)
    });
    let mut uploads = BTreeMap::new();
    for ((name, _, _), r) in readable.into_iter().zip(read) {
        match r {
            Ok(list) if list.is_empty() => {}
            Ok(list) => {
                uploads.insert(name, list);
            }
            Err(e) => left_out.push(format!("{name} (not readable: {})", reason(&e))),
        }
    }
    left_out.sort();
    Ok(LaunchpadReport {
        user: login.to_string(),
        uploads,
        left_out,
    })
}

/// Format the section as Markdown.
pub fn format_markdown(report: &LaunchpadReport, detail: u8) -> String {
    let heading = format!("### Launchpad PPAs (~{})\n\n", report.user);
    let total: usize = report.uploads.values().map(Vec::len).sum();
    let mut out = heading.clone();
    if total == 0 {
        out.push_str("No PPA uploads.\n");
    } else {
        forge::stat_across(
            &mut out,
            "Uploads",
            total as u64,
            report.uploads.len(),
            "PPA",
        );
    }
    if !report.left_out.is_empty() {
        out.push_str(&format!("- Left out: {}\n", report.left_out.join(", ")));
    }
    out.push('\n');
    if detail == 0 || total == 0 {
        return out;
    }
    for (ppa, list) in &report.uploads {
        out.push_str(&format!("#### {ppa}\n\n"));
        if detail >= 2 {
            for u in list {
                let status = if u.status == "Published" || u.status.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", u.status.to_lowercase())
                };
                out.push_str(&format!(
                    "- [{} {}]({}) for {}, {}{status}\n",
                    u.package, u.version, u.url, u.series, u.date
                ));
            }
            out.push('\n');
        } else {
            write_versions_by_package(&mut out, list);
        }
    }
    out
}

/// Whether `login` reads as a Launchpad name: lowercase letters and
/// digits first, then those, `+`, `.` and `-`.
pub fn valid_login(login: &str) -> bool {
    let mut chars = login.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "+.-".contains(c))
}

/// Every entry of the collection at `url`, following its pages.
fn pages<T: serde::de::DeserializeOwned>(
    get: &impl Fn(&str) -> Result<serde_json::Value, String>,
    url: &str,
) -> Result<Vec<T>, String> {
    let mut out = Vec::new();
    let mut next = Some(url.to_string());
    while let Some(url) = next {
        let page: Page<T> =
            serde_json::from_value(get(&url)?).map_err(|e| format!("GET {url}: {e}"))?;
        out.extend(page.entries);
        next = page.next_collection_link;
    }
    Ok(out)
}

/// Why a request failed, for the report: its HTTP status when it has
/// one, the whole error otherwise.
fn reason(error: &str) -> String {
    match error.rsplit_once(": HTTP ") {
        Some((_, status)) => format!("HTTP {status}"),
        None => error.to_string(),
    }
}

/// A PPA's name, `~owner/distribution/name`, from its API link
/// (`.../~owner/+archive/distribution/name`).
fn reference(self_link: &str) -> String {
    let Some(at) = self_link.find("/~") else {
        return self_link.to_string();
    };
    self_link[at + 1..].replacen("/+archive/", "/", 1)
}

/// `version` without the `~<series>` suffix a PPA upload usually
/// carries to build one source for several series
/// (`3.4.6-1~noble+1` is `3.4.6-1`); other versions are kept whole.
fn base_version<'a>(version: &'a str, series: &str) -> &'a str {
    if series.is_empty() {
        return version;
    }
    match version.find(&format!("~{series}")) {
        Some(at) => &version[..at],
        None => version,
    }
}

/// One line per package: its versions, oldest first, and the series
/// they were built for, so one release built for three series reads
/// as one version rather than three uploads.
fn write_versions_by_package(out: &mut String, uploads: &[Upload]) {
    let mut by_package: BTreeMap<&str, (Vec<&str>, Vec<&str>)> = BTreeMap::new();
    // Uploads come newest first; walk them oldest first.
    for u in uploads.iter().rev() {
        let (versions, series) = by_package.entry(u.package.as_str()).or_default();
        let version = base_version(&u.version, &u.series);
        if !versions.contains(&version) {
            versions.push(version);
        }
        if !u.series.is_empty() && !series.contains(&u.series.as_str()) {
            series.push(u.series.as_str());
        }
    }
    for (package, (versions, mut series)) in by_package {
        series.sort_unstable();
        let built_for = if series.is_empty() {
            String::new()
        } else {
            format!(" for {}", series.join(", "))
        };
        out.push_str(&format!(
            "- `{package}`: {}{built_for}\n",
            versions.join(", ")
        ));
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn source(name: &str, version: &str, created: &str, signer: Option<&str>) -> serde_json::Value {
        json!({
            "source_package_name": name,
            "source_package_version": version,
            "status": "Published",
            "date_created": created,
            "distro_series_link": "https://api.launchpad.net/devel/ubuntu/noble",
            "package_signer_link": signer.map(|s| format!("https://api.launchpad.net/devel/~{s}")),
            "package_creator_link": null,
        })
    }

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn a_login_reads_as_launchpad_names_do() {
        for good in ["me", "michel-slm", "a1", "x.y+z"] {
            assert!(valid_login(good), "{good}");
        }
        for bad in ["", "Me", "-me", "me lp", "https://launchpad.net/~me", "~me"] {
            assert!(!valid_login(bad), "{bad}");
        }
        assert_eq!(reason("GET u: HTTP 404 Not Found"), "HTTP 404 Not Found");
        assert_eq!(reason("GET u: timed out"), "GET u: timed out");
    }

    #[test]
    fn a_ppa_is_named_by_its_owner_distribution_and_name() {
        assert_eq!(
            reference("https://api.launchpad.net/devel/~me/+archive/ubuntu/ppa"),
            "~me/ubuntu/ppa"
        );
        assert_eq!(reference("odd"), "odd");
    }

    #[test]
    fn uploads_in_the_window_are_listed_by_ppa() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let server = rt.block_on(MockServer::start());
        let uri = server.uri();
        let archive = |owner: &str, name: &str, private: bool| {
            json!({
                "self_link": format!("{uri}/~{owner}/+archive/ubuntu/{name}"),
                "web_link": format!("https://launchpad.net/~{owner}/+archive/ubuntu/{name}"),
                "private": private,
            })
        };
        let answer = |p: &str, body: serde_json::Value| {
            Mock::given(method("GET"))
                .and(path(p.to_string()))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
        };
        rt.block_on(async {
            answer(
                "/~me/ppas",
                json!({"entries": [archive("me", "ppa", false), archive("me", "secret", true)]}),
            )
            .mount(&server)
            .await;
            // Own PPA, over two pages: mine, and those with no signer or
            // creator, count; a co-uploader's do not.
            Mock::given(method("GET"))
                .and(path("/~me/+archive/ubuntu/ppa"))
                .and(query_param("ws.op", "getPublishedSources"))
                .and(query_param("created_since_date", "2026-11-01"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "entries": [
                        source("late", "2", "2026-12-02T10:00:00+00:00", Some("me")),
                        source("tool", "1.1-1", "2026-11-20T10:00:00+00:00", None),
                    ],
                    "next_collection_link": format!("{uri}/page2"),
                })))
                .mount(&server)
                .await;
            answer(
                "/page2",
                json!({"entries": [
                    source("tool", "1.0-1", "2026-11-03T10:00:00+00:00", Some("other")),
                    source("libsigc++-2.0", "2.12-1", "2026-11-02T10:00:00+00:00", Some("me")),
                ]}),
            )
            .mount(&server)
            .await;
            // A team PPA: only the uploads signed or copied by me.
            // A private team PPA, and one whose uploads do not answer.
            Mock::given(method("GET"))
                .and(path("/~team/+archive/ubuntu/hidden"))
                .respond_with(ResponseTemplate::new(401))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/~team/+archive/ubuntu/flaky"))
                .and(query_param_is_missing("ws.op"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(archive("team", "flaky", false)),
                )
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/~team/+archive/ubuntu/flaky"))
                .and(query_param("ws.op", "getPublishedSources"))
                .respond_with(ResponseTemplate::new(500))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/~team/+archive/ubuntu/tools"))
                .and(query_param_is_missing("ws.op"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(archive("team", "tools", false)),
                )
                .mount(&server)
                .await;
            let mut copied = source("copied", "3", "2026-11-10T10:00:00+00:00", Some("other"));
            copied["package_creator_link"] = json!("https://api.launchpad.net/devel/~me");
            let mut superseded = source("mine", "1", "2026-11-05T10:00:00+00:00", Some("me"));
            superseded["status"] = json!("Superseded");
            Mock::given(method("GET"))
                .and(path("/~team/+archive/ubuntu/tools"))
                .and(query_param("ws.op", "getPublishedSources"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "entries": [
                        copied,
                        source("theirs", "9", "2026-11-08T10:00:00+00:00", Some("other")),
                        superseded,
                    ],
                })))
                .mount(&server)
                .await;
        });
        let r = launchpad_report(
            &uri,
            "~me",
            // The own PPA named again is read once.
            &[
                "~team/ubuntu/tools".into(),
                "me/ubuntu/ppa".into(),
                "~team/ubuntu/hidden".into(),
                "~team/ubuntu/flaky".into(),
                "~team/ubuntu/tools".into(),
            ],
            day("2026-11-01"),
            day("2026-11-30"),
            true,
        )
        .unwrap();
        assert_eq!(r.user, "me");
        assert_eq!(
            r.left_out,
            [
                "~me/ubuntu/secret (private)",
                "~team/ubuntu/flaky (not readable: HTTP 500 Internal Server Error)",
                "~team/ubuntu/hidden (not readable: HTTP 401 Unauthorized)",
            ]
        );
        let names = |ppa: &str| -> Vec<String> {
            r.uploads[ppa]
                .iter()
                .map(|u| format!("{} {}", u.package, u.version))
                .collect()
        };
        assert_eq!(
            names("~me/ubuntu/ppa"),
            ["tool 1.1-1", "libsigc++-2.0 2.12-1"]
        );
        assert_eq!(names("~team/ubuntu/tools"), ["copied 3", "mine 1"]);
        let first = &r.uploads["~me/ubuntu/ppa"][0];
        assert_eq!(first.series, "noble");
        assert_eq!(first.date, day("2026-11-20"));
        assert_eq!(
            first.url,
            "https://launchpad.net/~me/+archive/ubuntu/ppa/+packages?field.name_filter=tool"
        );

        let md = format_markdown(&r, 2);
        assert!(md.starts_with("### Launchpad PPAs (~me)\n\n"), "{md}");
        assert!(md.contains("- **Uploads:** 4 across 2 PPA(s)\n"), "{md}");
        assert!(
            md.contains("- Left out: ~me/ubuntu/secret (private), ~team/ubuntu/flaky"),
            "{md}"
        );
        // A `+` in the name is escaped in the link.
        assert!(md.contains("field.name_filter=libsigc%2B%2B-2.0)"), "{md}");
        assert!(md.contains("#### ~team/ubuntu/tools\n\n"), "{md}");
        assert!(
            md.contains(
                "- [tool 1.1-1](https://launchpad.net/~me/+archive/ubuntu/ppa/+packages?field.name_filter=tool) for noble, 2026-11-20\n"
            ),
            "{md}"
        );
        assert!(md.contains("for noble, 2026-11-05 (superseded)\n"), "{md}");
        let counts = format_markdown(&r, 1);
        assert!(
            counts.contains("- `libsigc++-2.0`: 2.12-1 for noble\n"),
            "{counts}"
        );
        let summary = format_markdown(&r, 0);
        assert!(!summary.contains("####"), "{summary}");
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["uploads"]["~me/ubuntu/ppa"][0]["package"], "tool");
    }

    #[test]
    fn a_bad_ppa_name_or_an_unreadable_answer_is_an_error() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let server = rt.block_on(MockServer::start());
        rt.block_on(async {
            Mock::given(method("GET"))
                .and(path("/~me/ppas"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"entries": []})))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/~gone/ppas"))
                .respond_with(ResponseTemplate::new(404))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/~junk/ppas"))
                .respond_with(ResponseTemplate::new(200).set_body_string("{"))
                .mount(&server)
                .await;
        });
        let (since, until) = (day("2026-11-01"), day("2026-11-30"));
        for bad in ["nope", "team//x", "team/ubuntu/a/b"] {
            let err = launchpad_report(&server.uri(), "me", &[bad.into()], since, until, false)
                .unwrap_err();
            assert!(err.contains("~owner/distribution/name"), "{err}");
        }
        let err = launchpad_report(&server.uri(), "gone", &[], since, until, false).unwrap_err();
        assert!(err.contains("HTTP 404"), "{err}");
        assert!(launchpad_report(&server.uri(), "junk", &[], since, until, false).is_err());
        // Nothing uploaded: said so.
        let r = launchpad_report(&server.uri(), "me", &[], since, until, false).unwrap();
        assert!(r.uploads.is_empty());
        assert!(format_markdown(&r, 2).contains("No PPA uploads.\n"));
    }

    #[test]
    fn one_release_built_for_several_series_reads_as_one_version() {
        let up = |version: &str, series: &str| Upload {
            package: "damo".into(),
            version: version.into(),
            series: series.into(),
            date: NaiveDate::from_ymd_opt(2026, 11, 21).unwrap(),
            status: "Published".into(),
            url: String::new(),
        };
        // Newest first, as Launchpad answers.
        let list = vec![
            up("3.4.6-1~stonking+1", "stonking"),
            up("3.4.6-1~noble+1", "noble"),
            up("3.4.4-1~resolute+1", "resolute"),
            up("3.4.4-1~noble+1", "noble"),
            up("0.9", ""),
        ];
        let mut out = String::new();
        write_versions_by_package(&mut out, &list);
        assert_eq!(
            out,
            "- `damo`: 0.9, 3.4.4-1, 3.4.6-1 for noble, resolute, stonking\n\n"
        );
        // A version without the series suffix is kept whole.
        assert_eq!(base_version("1.0-1ppa1", "noble"), "1.0-1ppa1");
    }
}
