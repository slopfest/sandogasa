// SPDX-License-Identifier: Apache-2.0 OR MIT

//! How urgent a tracking issue's update is, as the issue records it
//! (#98): the CVEs it fixes, each with its severity and a link to the
//! page listing its advisories, and a `priority::` label from the
//! priority Red Hat set on the RHEL issues. `file-issue` records both,
//! and `status --refresh` fills them in on issues filed without them.

use sandogasa_cve::severity::Rating;

/// What a CVE line starts with.
pub const CVE_LINE: &str = "- **CVE**:";

/// What the line recording CVEs the update does not fix starts with:
/// those named only in the merge request's description that the person
/// filing or refreshing the issue declined, so they are not asked about
/// again.
pub const NOT_FIXED_LINE: &str = "- **Not fixed here**:";

/// The scope of the priority labels: `priority::major`.
pub const PRIORITY_SCOPE: &str = "priority::";

/// Jira's priorities, most urgent first. `Undefined` is not one.
const PRIORITIES: [&str; 5] = ["Blocker", "Critical", "Major", "Normal", "Minor"];

/// The CVEs named in `texts`, each once, in the order first named.
pub fn cves_in<'a>(texts: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in texts.into_iter().flat_map(crate::sync_updates::cve_ids) {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// The CVEs a RHEL issue's description sets out, as `cpu-sig-tracker
/// jira` drafts it: a section per CVE, headed `CVE-…:` when there are
/// several, each ending in the CVE's NVD link. Only those lines count,
/// so a CVE the text mentions in passing, such as an older one a fix
/// regressed, is not taken as one the update fixes.
pub fn described_cves(description: &str) -> Vec<String> {
    cves_in(description.lines().map(str::trim).filter(|l| {
        let id = l
            .strip_prefix("https://nvd.nist.gov/vuln/detail/")
            .or_else(|| l.strip_suffix(':'));
        id.is_some_and(|id| crate::sync_updates::cve_ids(id) == [id.to_ascii_uppercase()])
    }))
}

/// `cves` in order of year, then number, compared as numbers:
/// CVE-2026-9999 before CVE-2026-10000. An id that is not a CVE's
/// sorts last, as it is.
pub fn sort_cves(cves: &mut [String]) {
    let key = |id: &str| {
        let mut parts = id.strip_prefix("CVE-")?.splitn(2, '-');
        let year: u32 = parts.next()?.parse().ok()?;
        let number: u64 = parts.next()?.parse().ok()?;
        Some((year, number))
    };
    cves.sort_by_key(|id| (key(id).is_none(), key(id), id.clone()));
}

/// CVE lines in the order [`sort_cves`] gives the CVEs they name.
pub fn sort_cve_lines(lines: &mut [&str]) {
    let mut keyed: Vec<(String, &str)> = lines
        .iter()
        .map(|l| {
            (
                parse_cve_lines(l).into_iter().next().unwrap_or_default(),
                *l,
            )
        })
        .collect();
    let mut ids: Vec<String> = keyed.iter().map(|(id, _)| id.clone()).collect();
    sort_cves(&mut ids);
    keyed.sort_by_key(|(id, _)| ids.iter().position(|i| i == id));
    for (slot, (_, line)) in lines.iter_mut().zip(keyed) {
        *slot = line;
    }
}

/// The CVEs [`update_cves`] found, by how sure the evidence is.
#[derive(Debug, Default, PartialEq)]
pub struct FoundCves {
    /// Named by the merge request's title, the tracking issue, or the
    /// RHEL issues' summaries or descriptions, sorted ([`sort_cves`]).
    pub certain: Vec<String>,
    /// Named only in the merge request's description, sorted, each with
    /// the description's line that names it. Prose there can mention a
    /// CVE the update does not fix, such as one whose fix it regressed,
    /// so these are asked about.
    pub unsure: Vec<(String, String)>,
}

/// How much of a description's line a question about it shows.
const CONTEXT_CHARS: usize = 100;

/// The CVEs an update fixes: those named in `texts` (the merge request's
/// title, the tracking issue's body, the RHEL issues' summaries), then
/// those its RHEL issues' `descriptions` set out ([`described_cves`]),
/// each once, sorted ([`sort_cves`]). An update fixing several CVEs
/// names them in its RHEL issue's description, not in a title. A CVE
/// only `mr_description` names is unsure.
pub fn update_cves<'a>(
    texts: impl IntoIterator<Item = &'a str>,
    mr_description: &str,
    descriptions: impl IntoIterator<Item = &'a str>,
) -> FoundCves {
    let mut certain = cves_in(texts);
    for id in descriptions.into_iter().flat_map(described_cves) {
        if !certain.contains(&id) {
            certain.push(id);
        }
    }
    sort_cves(&mut certain);
    let mut ids: Vec<String> = cves_in([mr_description])
        .into_iter()
        .filter(|id| !certain.contains(id))
        .collect();
    sort_cves(&mut ids);
    let unsure = ids
        .into_iter()
        .map(|id| {
            let line = mr_description
                .lines()
                .map(str::trim)
                .find(|l| crate::sync_updates::cve_ids(l).contains(&id))
                .unwrap_or_default();
            let line = match line.char_indices().nth(CONTEXT_CHARS) {
                Some((at, _)) => format!("{}…", &line[..at]),
                None => line.to_string(),
            };
            (id, line)
        })
        .collect();
    FoundCves { certain, unsure }
}

/// What [`settle_cves`] decided.
#[derive(Debug, Default, PartialEq)]
pub struct Settled {
    /// The CVEs the update fixes, sorted.
    pub cves: Vec<String>,
    /// The unsure CVEs asked about and not taken, sorted, for a
    /// [`not_fixed_line`].
    pub declined: Vec<String>,
    /// The unsure CVEs left out without an answer, because there was no
    /// terminal to ask at or the question could not be read: neither
    /// fixed nor declined.
    pub left_out: Vec<String>,
}

impl Settled {
    /// Warn that [`Settled::left_out`] were left out, if any were.
    pub fn warn_left_out(&self) {
        if !self.left_out.is_empty() {
            eprintln!(
                "warning: left out {}: named only in the merge request's \
                 description, and not asked about in this run",
                self.left_out.join(", ")
            );
        }
    }
}

/// The CVEs to record: the certain ones, and the unsure ones `ask`
/// takes. Unsure ones in `declined_before` (an earlier answer, from the
/// issue's [`NOT_FIXED_LINE`]) are left out without asking. `ask` gets
/// the question, which lists the rest, and one entry per CVE, and
/// returns the positions of those it takes, or `None` when it could
/// not ask ([`ask_at_terminal`]). Without `interactive`, or when `ask`
/// could not ask, they are [`Settled::left_out`].
pub fn settle_cves(
    found: FoundCves,
    declined_before: &[String],
    interactive: bool,
    ask: impl FnOnce(&str, &[String]) -> Option<Vec<usize>>,
) -> Settled {
    let cves = found.certain;
    let unsure: Vec<(String, String)> = found
        .unsure
        .into_iter()
        .filter(|(id, _)| !declined_before.contains(id))
        .collect();
    let left_out = |cves, unsure: Vec<(String, String)>| Settled {
        cves,
        declined: Vec::new(),
        left_out: unsure.into_iter().map(|(id, _)| id).collect(),
    };
    if unsure.is_empty() || !interactive {
        return left_out(cves, unsure);
    }
    let items: Vec<String> = unsure
        .iter()
        .map(|(id, line)| format!("  {id}: {line}"))
        .collect();
    let question = format!(
        "The merge request's description names CVEs found nowhere else:\n{}\n\
         Does the update fix them?",
        items.join("\n")
    );
    let Some(taken) = ask(&question, &items) else {
        return left_out(cves, unsure);
    };
    let mut settled = Settled {
        cves,
        ..Default::default()
    };
    for (i, (id, _)) in unsure.into_iter().enumerate() {
        if taken.contains(&i) {
            settled.cves.push(id);
        } else {
            settled.declined.push(id);
        }
    }
    sort_cves(&mut settled.cves);
    settled
}

/// The `ask` for [`settle_cves`] at a terminal: all, none (the default)
/// or each in turn, after `heading` when there is one. `None` when the
/// answer could not be read.
pub fn ask_at_terminal(
    heading: Option<String>,
) -> impl FnOnce(&str, &[String]) -> Option<Vec<usize>> {
    move |question, items| {
        let question = match heading {
            Some(h) => format!("{h}:\n{question}"),
            None => question.to_string(),
        };
        sandogasa_cli::confirm_each(&question, "Does it fix this one?", items, false).ok()
    }
}

/// The line recording that the update does not fix `cves`.
pub fn not_fixed_line(cves: &[String]) -> String {
    format!("{NOT_FIXED_LINE} {}", cves.join(", "))
}

/// The CVEs a body's [`NOT_FIXED_LINE`] names.
pub fn parse_not_fixed(body: &str) -> Vec<String> {
    cves_in(
        body.lines()
            .filter(|l| l.trim_start().starts_with(NOT_FIXED_LINE)),
    )
}

/// `body` without its [`NOT_FIXED_LINE`], for reading the CVEs the
/// update fixes from the rest.
pub fn without_not_fixed(body: &str) -> String {
    body.lines()
        .filter(|l| !l.trim_start().starts_with(NOT_FIXED_LINE))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The label for the most urgent of `priorities` (Jira's names):
/// `priority::critical`. `None` when none is a priority Jira ranks.
pub fn priority_label<'a>(priorities: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let rank = |p: &str| {
        PRIORITIES
            .iter()
            .position(|k| k.eq_ignore_ascii_case(p.trim()))
    };
    priorities
        .into_iter()
        .filter_map(rank)
        .min()
        .map(|i| format!("{PRIORITY_SCOPE}{}", PRIORITIES[i].to_ascii_lowercase()))
}

/// A body's line for `cve`: its severity by the first of `ratings` (Red
/// Hat's, when it rated it), linking Red Hat's page for it, which lists
/// the advisories that fix it, and NVD's.
pub fn cve_line(cve: &str, ratings: &[Rating]) -> String {
    let rated = match ratings.first() {
        Some(r) => format!("{} ({r})", r.level.as_str()),
        None => "not rated yet".to_string(),
    };
    format!(
        "{CVE_LINE} [{cve}](https://access.redhat.com/security/cve/{cve}) — {rated}; \
         [NVD](https://nvd.nist.gov/vuln/detail/{cve})"
    )
}

/// The CVEs a body's CVE lines name.
pub fn parse_cve_lines(body: &str) -> Vec<String> {
    cves_in(
        body.lines()
            .filter(|l| l.trim_start().starts_with(CVE_LINE)),
    )
}

/// The CVE lines for `cves`, each rated by Red Hat, NVD and GitHub
/// (what `jira` rates a CVE by).
pub fn rated_cve_lines(cves: &[String], verbose: bool) -> Vec<String> {
    let Ok(runtime) = crate::jira::runtime() else {
        return cves.iter().map(|c| cve_line(c, &[])).collect();
    };
    cves.iter()
        .map(|cve| {
            let ratings = runtime.block_on(crate::jira_issue::cve_ratings(cve, verbose));
            cve_line(cve, &ratings)
        })
        .collect()
}

/// The label changes that make `labels` carry `want` as their only
/// priority: `(add, remove)`, both empty when they already do.
pub fn priority_change(labels: &[String], want: &str) -> (Vec<String>, Vec<String>) {
    let remove: Vec<String> = labels
        .iter()
        .filter(|l| l.starts_with(PRIORITY_SCOPE) && l.as_str() != want)
        .cloned()
        .collect();
    let add = if labels.iter().any(|l| l == want) {
        Vec::new()
    } else {
        vec![want.to_string()]
    };
    (add, remove)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sandogasa_cve::severity::Level;

    #[test]
    fn the_most_urgent_jira_priority_names_the_label() {
        assert_eq!(
            priority_label(["Normal", "Critical", "Major"]).as_deref(),
            Some("priority::critical")
        );
        assert_eq!(
            priority_label(["major"]).as_deref(),
            Some("priority::major")
        );
        assert_eq!(priority_label(["Undefined"]), None);
        assert_eq!(priority_label([]), None);
    }

    #[test]
    fn a_cve_line_gives_the_severity_and_links_the_advisories() {
        let ratings = [Rating {
            level: Level::High,
            source: "Red Hat".into(),
            detail: "Important (CVSS 3: 7.5)".into(),
        }];
        let line = cve_line("CVE-2026-119741", &ratings);
        assert_eq!(
            line,
            "- **CVE**: [CVE-2026-119741](https://access.redhat.com/security/cve/CVE-2026-119741) \
             — high (Red Hat: Important (CVSS 3: 7.5)); \
             [NVD](https://nvd.nist.gov/vuln/detail/CVE-2026-119741)"
        );
        assert!(cve_line("CVE-2026-1", &[]).contains("— not rated yet;"));
        let body = format!("- **MR**: x\n{line}\n- **Release**: c10s\n");
        assert_eq!(parse_cve_lines(&body), ["CVE-2026-119741"]);
        // A CVE named elsewhere in the body is not a CVE line.
        assert!(parse_cve_lines("Fixes CVE-2026-2.\n").is_empty());
    }

    #[test]
    fn a_rhel_issues_description_sets_out_the_cves_it_fixes() {
        // As `jira` drafts it for git's 2.52.1 update (RHEL-306261).
        let several = "Update git to 2.52.1 to address CVEs announced in https://github.blog/x/\n\
                       \n\
                       CVE-2026-111760:\n\
                       Severity: high (Red Hat: Important (CVSS 3: 8.8))\n\
                       \n\
                       https://nvd.nist.gov/vuln/detail/CVE-2026-111760\n\
                       \n\
                       CVE-2026-113408:\n\
                       Severity: medium\n\
                       \n\
                       A regression of the fix for CVE-2020-0001.\n\
                       \n\
                       https://nvd.nist.gov/vuln/detail/CVE-2026-113408\n";
        assert_eq!(
            described_cves(several),
            ["CVE-2026-111760", "CVE-2026-113408"]
        );
        // One CVE: no heading, only its NVD link.
        assert_eq!(
            described_cves("Severity: high\n\nhttps://nvd.nist.gov/vuln/detail/CVE-2026-1234\n"),
            ["CVE-2026-1234"]
        );
        // A heading that is not a CVE alone, or prose, is not one.
        assert!(described_cves("See CVE-2026-1234:\nCVE-2026-1234 is bad").is_empty());
        assert_eq!(
            update_cves(["Fix CVE-2026-113408"], "", [several]).certain,
            ["CVE-2026-111760", "CVE-2026-113408"]
        );
        assert_eq!(
            update_cves(["Fix CVE-2026-111760"], "", [several]).certain,
            ["CVE-2026-111760", "CVE-2026-113408"]
        );
    }

    #[test]
    fn a_cve_only_the_mr_description_names_is_unsure() {
        let long = "x".repeat(120);
        let found = update_cves(
            ["Fix CVE-2026-2002"],
            &format!(
                "Fixes CVE-2026-2002.\n\n  A regression of the fix for CVE-2026-1001.\n\
                 Also CVE-2025-9009 {long}"
            ),
            [],
        );
        assert_eq!(found.certain, ["CVE-2026-2002"]);
        assert_eq!(
            found.unsure,
            [
                (
                    "CVE-2025-9009".to_string(),
                    format!("Also CVE-2025-9009 {}…", &long[..81])
                ),
                (
                    "CVE-2026-1001".to_string(),
                    "A regression of the fix for CVE-2026-1001.".to_string()
                ),
            ]
        );
        // Named elsewhere too, it is certain.
        let found = update_cves(["Fix CVE-2026-2002", "CVE-2026-1001"], "CVE-2026-1001", []);
        assert!(found.unsure.is_empty());
    }

    #[test]
    fn an_unsure_cve_is_kept_only_when_taken() {
        let found = || FoundCves {
            certain: vec!["CVE-2026-5005".into()],
            unsure: vec![
                ("CVE-2026-1001".into(), "regressed by CVE-2026-1001".into()),
                ("CVE-2026-9009".into(), "see CVE-2026-9009".into()),
            ],
        };
        let mut asked = None;
        let settled = settle_cves(found(), &[], true, |q, items| {
            asked = Some((q.to_string(), items.to_vec()));
            Some(vec![0])
        });
        assert_eq!(settled.cves, ["CVE-2026-1001", "CVE-2026-5005"]);
        assert_eq!(settled.declined, ["CVE-2026-9009"]);
        assert!(settled.left_out.is_empty());
        let (q, items) = asked.unwrap();
        assert!(q.contains("  CVE-2026-1001: regressed by CVE-2026-1001\n"));
        assert!(q.ends_with("Does the update fix them?"));
        assert_eq!(items[1], "  CVE-2026-9009: see CVE-2026-9009");
        // One declined before is not asked about again.
        let settled = settle_cves(found(), &["CVE-2026-9009".into()], true, |_, items| {
            assert_eq!(items, ["  CVE-2026-1001: regressed by CVE-2026-1001"]);
            Some(vec![])
        });
        assert_eq!(settled.cves, ["CVE-2026-5005"]);
        assert_eq!(settled.declined, ["CVE-2026-1001"]);
        // Every one declined before: nothing asked.
        let before = ["CVE-2026-1001".to_string(), "CVE-2026-9009".to_string()];
        let settled = settle_cves(found(), &before, true, |_, _| panic!("asked"));
        assert_eq!(
            settled,
            Settled {
                cves: vec!["CVE-2026-5005".into()],
                ..Default::default()
            }
        );
        // Without a terminal nothing is asked, and the unsure ones are
        // left out, not declined.
        let settled = settle_cves(found(), &[], false, |_, _| panic!("asked"));
        assert_eq!(settled.cves, ["CVE-2026-5005"]);
        assert!(settled.declined.is_empty());
        assert_eq!(settled.left_out, ["CVE-2026-1001", "CVE-2026-9009"]);
        // Nor is a question that could not be answered a "no".
        let settled = settle_cves(found(), &[], true, |_, _| None);
        assert!(settled.declined.is_empty());
        assert_eq!(settled.left_out, ["CVE-2026-1001", "CVE-2026-9009"]);
    }

    #[test]
    fn the_not_fixed_line_round_trips_and_hides_its_cves() {
        let line = not_fixed_line(&["CVE-2025-9009".into(), "CVE-2026-1001".into()]);
        assert_eq!(line, "- **Not fixed here**: CVE-2025-9009, CVE-2026-1001");
        let body = format!("- **CVE**: CVE-2026-5005\n{line}\n- **Release**: c10s\n");
        assert_eq!(parse_not_fixed(&body), ["CVE-2025-9009", "CVE-2026-1001"]);
        assert_eq!(
            cves_in([without_not_fixed(&body).as_str()]),
            ["CVE-2026-5005"]
        );
        // Its CVEs are not CVE lines.
        assert_eq!(parse_cve_lines(&body), ["CVE-2026-5005"]);
    }

    #[test]
    fn cves_sort_by_year_then_number() {
        let mut cves: Vec<String> = [
            "CVE-2026-114466",
            "CVE-2026-111760",
            "CVE-2025-99999",
            "CVE-2026-9999",
            "not-a-cve",
            "CVE-2026-105437",
        ]
        .map(String::from)
        .to_vec();
        sort_cves(&mut cves);
        assert_eq!(
            cves,
            [
                "CVE-2025-99999",
                "CVE-2026-9999",
                "CVE-2026-105437",
                "CVE-2026-111760",
                "CVE-2026-114466",
                "not-a-cve"
            ]
        );
    }

    #[test]
    fn cve_lines_sort_by_the_cve_they_name() {
        let a = cve_line("CVE-2026-114466", &[]);
        let b = cve_line("CVE-2026-105437", &[]);
        let c = cve_line("CVE-2025-1234", &[]);
        let mut lines = vec![a.as_str(), b.as_str(), c.as_str()];
        sort_cve_lines(&mut lines);
        assert_eq!(lines, [c.as_str(), b.as_str(), a.as_str()]);
    }

    #[test]
    fn cves_are_gathered_once_in_order() {
        assert_eq!(
            cves_in([
                "Fix sum2 overflow and CVE-2026-119741",
                "Resolves: RHEL-1\ncve-2026-119741, CVE-2026-2000",
            ]),
            ["CVE-2026-119741", "CVE-2026-2000"]
        );
    }

    #[test]
    fn a_priority_label_replaces_another_and_is_left_alone_when_right() {
        let labels: Vec<String> = ["c10s", "priority::normal", "security"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            priority_change(&labels, "priority::major"),
            (
                vec!["priority::major".to_string()],
                vec!["priority::normal".to_string()]
            )
        );
        let right: Vec<String> = ["priority::major"].map(String::from).to_vec();
        assert_eq!(priority_change(&right, "priority::major"), (vec![], vec![]));
    }
}
