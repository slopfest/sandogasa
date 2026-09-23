// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What is known about a CVE's fix — from NVD's CPE data, from
//! GitHub's advisory database, or from a range closed at the top with
//! no fix recorded — and how a build's version is judged against it.
//!
//! A fix version alone cannot say which versions it speaks for: a CVE
//! fixed in two series has two, and a build in the other series is
//! above one fix and still vulnerable. So every judgment takes the
//! vulnerable ranges alongside the fixed versions, and a build is a
//! fix only when it is at or past a fixed version *and* outside every
//! range still marked vulnerable.

/// A distribution name as [PEP 503](https://peps.python.org/pep-0503/)
/// normalises it and as Fedora's `python3dist()` provides carry it:
/// lowercase, with any run of `-`, `_` and `.` as a single `-`
/// (`jaraco.context` → `jaraco-context`).
pub fn pep503(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            if !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// Whether a build at `version` fixes the CVE: at or past one of the
/// `fixed` versions and outside every one of the `ranges` still marked
/// vulnerable. The first test alone would accept one series' fix for
/// another series' build.
pub fn is_fix(
    version: &str,
    fixed: &[sandogasa_nvd::FixedVersion],
    ranges: &[sandogasa_nvd::VulnerableRange],
) -> bool {
    fixed
        .iter()
        .any(|fv| crate::version::version_gte(version, &fv.version))
        && !ranges.iter().any(|r| in_vulnerable_range(r, version))
}

/// Check a single bug against Bodhi updates and NVD fixed versions.
/// Check whether an NVD product name matches a Fedora component.
///
/// First tries an exact match, then checks if `(<product>)` appears in
/// the component's RPM provides (obtained via fedrq).  This handles
/// cases like NVD product "django" matching Fedora component
/// "python-django3" (whose subpackages provide `python3dist(django)`).
/// A Python distribution is also tried under its PEP 503 name, which is
/// what the provide carries: `jaraco.context` is
/// `python3dist(jaraco-context)`.
pub fn product_matches_component(product: &str, component: &str, provides: Option<&str>) -> bool {
    if product == component {
        return true;
    }
    if let Some(provides) = provides {
        let provides = provides.to_lowercase();
        let needles = [
            format!("({})", product.to_lowercase()),
            format!("({})", pep503(product)),
        ];
        if needles.iter().any(|n| provides.contains(n)) {
            return true;
        }
    }
    false
}

/// What one NVD lookup tells us, kept whole so a CVE with no
/// `configurations` can still be chased through its references.
#[derive(Debug, Clone, Default)]
pub struct CveFacts {
    pub fixed: Vec<sandogasa_nvd::FixedVersion>,
    /// Which versions each fix speaks for. A fix version on its own
    /// cannot say, and a CVE fixed in two series has two.
    pub ranges: Vec<sandogasa_nvd::VulnerableRange>,
    pub vuln_status: String,
    pub references: Vec<String>,
    /// The `vendor:product` pairs NVD's CPEs name, for a component
    /// named after both (`uutils-coreutils` for `uutils:coreutils`).
    pub affected: Vec<sandogasa_nvd::models::AffectedProduct>,
}

impl CveFacts {
    pub fn from_response(resp: &sandogasa_nvd::models::CveResponse) -> Self {
        Self {
            fixed: resp.fixed_versions(),
            ranges: resp.vulnerable_ranges(),
            affected: resp.affected_upstream(),
            vuln_status: resp.vuln_status().to_string(),
            references: resp
                .reference_urls()
                .into_iter()
                .map(str::to_string)
                .collect(),
        }
    }
}

/// Where a fixed version came from, so the run can say so.
/// A fixed version found outside NVD: the versions (one per affected
/// series) and the vulnerable ranges those series span, judged the way
/// NVD's are, plus where they came from.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub fixed: Vec<sandogasa_nvd::FixedVersion>,
    pub ranges: Vec<sandogasa_nvd::VulnerableRange>,
    pub source: VersionSource,
}

impl Resolved {
    pub fn one(product: &str, version: &str, source: VersionSource) -> Self {
        Resolved {
            fixed: vec![sandogasa_nvd::FixedVersion {
                product: product.to_string(),
                version: version.to_string(),
            }],
            ranges: Vec::new(),
            source,
        }
    }

    /// `protobuf 6.33.5, protobuf 5.29.6`; `coreutils > 0.8.0` when the
    /// version is an affected range's upper bound rather than a fix.
    pub fn describe(&self) -> String {
        let inferred = matches!(
            self.source,
            VersionSource::Advisory { inferred: true, .. } | VersionSource::Ranges
        );
        self.fixed
            .iter()
            .map(|f| {
                if inferred {
                    format!("{} > {}", f.product, f.version)
                } else {
                    format!("{} {}", f.product, f.version)
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[derive(Debug, Clone)]
pub enum VersionSource {
    /// From the config's `[fixed_versions]` table.
    Config,
    /// GitHub's advisory database records the fix as structured data
    /// (`first_patched_version`), as authoritative as NVD's ranges —
    /// or, `inferred`, only an affected range closed at the top, whose
    /// bound stands in for the fix (see [`inferred_from_ranges`]).
    Advisory { id: String, inferred: bool },
    /// NVD marks a range affected up to and including a version and
    /// records no fix; the bound stands in for it.
    Ranges,
    /// Scraped from a reference and confirmed at the prompt, and
    /// whether that answer was written to the config — which decides
    /// whether there is anything left to advise.
    Confirmed { version: String, recorded: bool },
}

/// What GitHub's advisories for a CVE settle: the first advisory with a
/// patched version — or, failing one, an affected range closed at the
/// top (see [`inferred_from_ranges`]) — gives a fixed version and a
/// range per affected series, judged as NVD's would be. When none does,
/// the note says what the database had instead, for the report.
pub fn resolved_from_ghsa(
    records: &[(String, Vec<crate::advisory::GhsaVuln>)],
) -> (Option<Resolved>, Option<String>) {
    let mut note = None;
    for (id, vulns) in records {
        let fixed: Vec<sandogasa_nvd::FixedVersion> = vulns
            .iter()
            .filter_map(|v| {
                Some(sandogasa_nvd::FixedVersion {
                    product: v.package.clone(),
                    version: v.patched.clone()?,
                })
            })
            .collect();
        // A range open at the top (`>= 1.19.0`) beside a patched
        // version means "from 1.19.0 until the fix": close it there,
        // or every build past the fix would still read as vulnerable.
        let ranges: Vec<sandogasa_nvd::VulnerableRange> = vulns
            .iter()
            .filter_map(|v| {
                let mut r = crate::advisory::ghsa_range(&v.package, v.range.as_deref()?)?;
                if r.end_excluding.is_none() && r.end_including.is_none() {
                    r.end_excluding = v.patched.clone();
                }
                Some(r)
            })
            .collect();
        // No patched version, but a range closed at the top says what
        // is not affected: everything above it.
        let (fixed, inferred) = if fixed.is_empty() {
            (inferred_from_ranges(&ranges), true)
        } else {
            (fixed, false)
        };
        if fixed.is_empty() {
            if note.is_none()
                && let Some(v) = vulns.first()
            {
                note = Some(format!(
                    "{id} records no patched version{}",
                    v.range
                        .as_deref()
                        .map(|r| format!(" (affected: {} {r})", v.package))
                        .unwrap_or_default()
                ));
            }
            continue;
        }
        return (
            Some(Resolved {
                fixed,
                ranges,
                source: VersionSource::Advisory {
                    id: id.clone(),
                    inferred,
                },
            }),
            note,
        );
    }
    (None, note)
}

/// The fixed versions an affected-range list implies when it names no
/// fix: a range closed at the top (`<= 0.8.0`, NVD's
/// `versionEndIncluding`) says everything above its bound is not
/// affected, so the bound stands in as the fixed version — with the
/// range kept alongside, so the bound itself stays inside it and only
/// builds above clear. Ranges with a fix or no upper bound give none.
pub fn inferred_from_ranges(
    ranges: &[sandogasa_nvd::VulnerableRange],
) -> Vec<sandogasa_nvd::FixedVersion> {
    ranges
        .iter()
        .filter(|r| r.end_excluding.is_none())
        .filter_map(|r| {
            Some(sandogasa_nvd::FixedVersion {
                product: r.product.clone(),
                version: r.end_including.clone()?,
            })
        })
        .collect()
}

/// Whether `version` is inside a range NVD marks vulnerable.
///
/// A fix from one series must not vouch for a build in another: Django
/// 6.0.5 is above CVE-2026-15337's 5.2.17 fix and still inside the same
/// CVE's `6.0 <= v < 6.0.8` range.
pub fn in_vulnerable_range(range: &sandogasa_nvd::VulnerableRange, version: &str) -> bool {
    if let Some(start) = &range.start_including
        && !crate::version::version_gte(version, start)
    {
        return false;
    }
    if let Some(end) = &range.end_excluding {
        return !crate::version::version_gte(version, end);
    }
    if let Some(end) = &range.end_including {
        return crate::version::version_gte(end, version);
    }
    // A wildcard CPE with no bounds: every version from the start on.
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ghsa(
        id: &str,
        vulns: &[(&str, &str, Option<&str>, Option<&str>)],
    ) -> (String, Vec<crate::advisory::GhsaVuln>) {
        (
            id.to_string(),
            vulns
                .iter()
                .map(|(eco, pkg, range, patched)| crate::advisory::GhsaVuln {
                    ecosystem: eco.to_string(),
                    package: pkg.to_string(),
                    range: range.map(str::to_string),
                    patched: patched.map(str::to_string),
                })
                .collect(),
        )
    }

    #[test]
    fn resolved_from_ghsa_takes_fixes_then_inferred_bounds_then_notes() {
        let protobuf = ghsa(
            "GHSA-7gcm-g887-7qv7",
            &[
                (
                    "pip",
                    "protobuf",
                    Some(">= 6.30.0rc1, <= 6.33.4"),
                    Some("6.33.5"),
                ),
                ("pip", "protobuf", Some("< 5.29.6"), Some("5.29.6")),
            ],
        );
        let (found, note) = resolved_from_ghsa(std::slice::from_ref(&protobuf));
        let found = found.unwrap();
        assert_eq!(found.fixed.len(), 2);
        assert_eq!(found.ranges.len(), 2);
        assert!(matches!(
            found.source,
            VersionSource::Advisory {
                inferred: false,
                ..
            }
        ));
        assert_eq!(note, None);

        let coreutils = ghsa(
            "GHSA-x4mc-mqm7-gg39",
            &[("rust", "coreutils", Some("<= 0.8.0"), None)],
        );
        let (found, _) = resolved_from_ghsa(std::slice::from_ref(&coreutils));
        let found = found.unwrap();
        assert_eq!(found.describe(), "coreutils > 0.8.0");
        assert!(matches!(
            found.source,
            VersionSource::Advisory { inferred: true, .. }
        ));

        let open = ghsa("GHSA-open", &[("rust", "x", Some(">= 1.0"), None)]);
        let (found, note) = resolved_from_ghsa(std::slice::from_ref(&open));
        assert!(found.is_none());
        assert_eq!(
            note.as_deref(),
            Some("GHSA-open records no patched version (affected: x >= 1.0)")
        );
    }

    #[test]
    fn inferred_from_ranges_takes_a_closed_top_with_no_fix() {
        let r = |end_excluding: Option<&str>, end_including: Option<&str>| {
            sandogasa_nvd::VulnerableRange {
                product: "coreutils".into(),
                start_including: None,
                end_excluding: end_excluding.map(str::to_string),
                end_including: end_including.map(str::to_string),
            }
        };
        let inferred = inferred_from_ranges(&[r(None, Some("0.8.0"))]);
        assert_eq!(inferred.len(), 1);
        assert_eq!(inferred[0].version, "0.8.0");
        // The bound itself stays inside the range; only builds above clear.
        assert!(in_vulnerable_range(&r(None, Some("0.8.0")), "0.8.0"));
        assert!(!in_vulnerable_range(&r(None, Some("0.8.0")), "0.9.0"));
        // A recorded fix or an open top infers nothing.
        assert!(inferred_from_ranges(&[r(Some("0.9.0"), Some("0.8.0"))]).is_empty());
        assert!(inferred_from_ranges(&[r(None, None)]).is_empty());
    }

    #[test]
    fn a_range_open_at_the_top_is_closed_by_the_patched_version() {
        // GHSA-73p7-m7gg-w2jv as strukturag/libheif publishes it:
        // affected `>= 1.19.0`, patched 1.23.3.
        let (found, _) = resolved_from_ghsa(&[ghsa(
            "GHSA-73p7-m7gg-w2jv",
            &[("", "libheif", Some(">= 1.19.0"), Some("1.23.3"))],
        )]);
        let found = found.unwrap();
        assert_eq!(found.ranges[0].end_excluding.as_deref(), Some("1.23.3"));
        assert!(is_fix("1.23.5", &found.fixed, &found.ranges));
        assert!(!is_fix("1.22.0", &found.fixed, &found.ranges));
        assert!(
            !is_fix("1.18.0", &found.fixed, &found.ranges),
            "below the range: not affected, but not a fix either"
        );
    }

    #[test]
    fn a_fix_must_clear_every_vulnerable_range() {
        let fv = |v: &str| sandogasa_nvd::FixedVersion {
            product: "django".into(),
            version: v.into(),
        };
        let range = |start: &str, end: &str| sandogasa_nvd::VulnerableRange {
            product: "django".into(),
            start_including: Some(start.into()),
            end_excluding: Some(end.into()),
            end_including: None,
        };
        let fixed = vec![fv("5.2.17"), fv("6.0.8")];
        let ranges = vec![range("5.2", "5.2.17"), range("6.0", "6.0.8")];
        assert!(is_fix("5.2.17", &fixed, &ranges));
        assert!(is_fix("6.0.8", &fixed, &ranges));
        assert!(
            !is_fix("6.0.5", &fixed, &ranges),
            "above 5.2.17 but inside the 6.0 range"
        );
        assert!(!is_fix("5.2.16", &fixed, &ranges));
        assert!(
            !is_fix("1.0", &[], &[]),
            "no fixed version: nothing is a fix"
        );
    }
}
