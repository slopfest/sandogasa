// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Helpers shared by the forge backends (`gitlab`, `github`,
//! `forgejo`, `sourcehut`). Each backend keeps its own thin
//! `find_token` / `instance_token_env` wrappers naming its
//! [`TokenSpec`], so the env-var names stay byte-identical per
//! service; only the boilerplate lives here.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use sandogasa_config::secret::SecretRef;

/// Per-service parameters for the token lookup: everything that
/// differs between the four forges.
pub(crate) struct TokenSpec {
    /// Human-readable service name, used in the error message
    /// ("no GitLab token for …").
    pub service: &'static str,
    /// The keyring entry's label, shared with every sandogasa tool
    /// reading the same service's token (`sandogasa: GitLab token`).
    pub label: &'static str,
    /// The generic env var, which is also the prefix of the
    /// instance-specific one (`<generic>_<HOST>`).
    pub generic_env: &'static str,
    /// Characters replaced by `_` when turning a hostname into an
    /// env-var suffix. Kept per-service because the names are a
    /// documented interface: Sourcehut also folds `-`, the others
    /// only `.`.
    pub host_separators: &'static [char],
    /// Extra text appended to the error message (e.g. where to
    /// generate a token). Empty for most services.
    pub hint: &'static str,
}

/// The level-1 rendering of an item list: one line per group — repo,
/// project, component, list — with a count, largest first and ties by
/// name. Level 2 lists the items themselves; see DEVELOPMENT.md,
/// "Detail levels".
pub(crate) fn write_counts_by<'a, T>(
    out: &mut String,
    items: &'a [T],
    group: impl Fn(&'a T) -> &'a str,
) {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for item in items {
        *counts.entry(group(item)).or_default() += 1;
    }
    let mut rows: Vec<(&str, usize)> = counts.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    for (group, n) in rows {
        out.push_str(&format!("- `{group}`: {n}\n"));
    }
    out.push('\n');
}

/// A commit as the rebased-landing check sees it: its title and author,
/// whichever forge it came from.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Landed {
    pub title: String,
    pub author_name: String,
    pub author_email: String,
    /// When it was authored (RFC 3339), where the forge says.
    pub authored: Option<String>,
}

/// How far before a PR was opened its own commits' dates may move the
/// branch listing ([`listing_since`]): enough for a commit written and
/// applied around the time the PR was, without listing months of a
/// branch, which comes one page at a time.
const LISTING_LEAD_DAYS: i64 = 30;

/// When an RFC 3339 timestamp is, in UTC.
fn instant(ts: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|d| d.with_timezone(&chrono::Utc))
}

/// Where to start listing a target branch's commits to find a PR's work
/// on it: the earliest of when the PR was opened (`opened`) and when
/// its own commits were made (`commit_dates`, as the forge gives them),
/// but no more than [`LISTING_LEAD_DAYS`] before the opening. A commit
/// applied as it is keeps its own date, which can be before the PR was
/// opened: rasdaemon's ffd1392 is dated 15 minutes before its PR #272
/// (#230). Dates that do not parse are left out; with none, `opened` as
/// it is.
pub(crate) fn listing_since(opened: &str, commit_dates: &[Option<String>]) -> String {
    let Some(open) = instant(opened) else {
        return opened.to_string();
    };
    let floor = open - chrono::Duration::days(LISTING_LEAD_DAYS);
    commit_dates
        .iter()
        .flatten()
        .filter_map(|d| instant(d))
        .chain([open])
        .min()
        .unwrap_or(open)
        .max(floor)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Whether a PR's work landed as a rebased or reworded commit: one of
/// the target branch's commits (already limited by the caller, from the
/// earlier of the PR's opening and its own commits, [`listing_since`])
/// carries the same title as one of the PR's commits **and the same
/// author**. Authorship is the decisive check, not content: the same
/// change redone by the maintainer — an identical `gbp import-orig` two
/// days later, say — is theirs, and the PR it superseded stays closed.
///
/// With `opened`, the listing reaches back before the PR was opened, so
/// a branch commit authored before then counts only when authored at
/// the same moment as the PR's commit: applied or rebased, a commit
/// keeps its author date, while an earlier commit of the same title by
/// the same author ("Update translations") is another one (#230).
pub(crate) fn rebased_onto(
    pr_commits: &[Landed],
    branch_commits: &[Landed],
    opened: Option<&str>,
) -> bool {
    let same_author = |a: &Landed, b: &Landed| {
        (!a.author_email.is_empty() && a.author_email.eq_ignore_ascii_case(&b.author_email))
            || (!a.author_name.is_empty() && a.author_name == b.author_name)
    };
    let opened = opened.and_then(instant);
    let in_time = |pc: &Landed, bc: &Landed| {
        let Some(opened) = opened else {
            return true;
        };
        match bc.authored.as_deref().and_then(instant) {
            Some(at) if at < opened => pc.authored.as_deref().and_then(instant) == Some(at),
            _ => true,
        }
    };
    pr_commits.iter().any(|pc| {
        branch_commits
            .iter()
            .any(|bc| bc.title.trim() == pc.title.trim() && same_author(pc, bc) && in_time(pc, bc))
    })
}

/// Whether an RFC 3339 timestamp's date falls within
/// `[since, until]` (inclusive). Only the date part is considered.
pub(crate) fn date_in_range(ts: &str, since: NaiveDate, until: NaiveDate) -> bool {
    let Some(day) = ts.split('T').next() else {
        return false;
    };
    NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map(|d| d >= since && d <= until)
        .unwrap_or(false)
}

/// Strip scheme + trailing slash to get the bare hostname — the
/// token-keying host. Note that an API base such as
/// `api.github.com` keeps its `api.` prefix, so a user with both
/// github.com and a GHES `api.example.com` ends up with two
/// distinct keys.
pub(crate) fn instance_host(instance: &str) -> String {
    instance
        .trim_end_matches('/')
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .to_string()
}

/// Name of the instance-specific token env var for `instance`,
/// e.g. `GITLAB_TOKEN_SALSA_DEBIAN_ORG`.
pub(crate) fn instance_token_env(instance: &str, spec: &TokenSpec) -> String {
    format!(
        "{}_{}",
        spec.generic_env,
        instance_host(instance)
            .to_uppercase()
            .replace(spec.host_separators, "_")
    )
}

/// The keyring entry for `instance`'s token: one per instance, named
/// for its host, so two instances of a forge keep separate tokens and
/// every tool talking to the same one shares it (#79).
pub(crate) fn secret(instance: &str, spec: &TokenSpec) -> SecretRef {
    SecretRef::for_url(instance, spec.label)
}

/// Look up a forge API token for an instance.
///
/// Order: instance-specific env var → generic env var → the
/// keyring's entry for the instance → `<service>_tokens.<hostname>`
/// from the user overlay → error. Env vars win over the rest so a
/// shell override works even with a persisted token; an empty one
/// counts as unset.
pub(crate) fn find_token(
    instance: &str,
    tokens: &BTreeMap<String, String>,
    spec: &TokenSpec,
) -> Result<String, String> {
    let var = instance_token_env(instance, spec);
    if let Ok(t) = std::env::var(&var)
        && !t.is_empty()
    {
        return Ok(t);
    }
    let host = instance_host(instance);
    sandogasa_config::secret::resolve(
        crate::secrets::keyring(),
        Some(spec.generic_env),
        &secret(instance, spec),
        tokens.get(&host).map(String::as_str),
    )
    .ok_or_else(|| {
        format!(
            "no {} token for {host}: set {var} (instance-specific), \
             {} (generic), or run `sandogasa-report config` to \
             store one{}",
            spec.service, spec.generic_env, spec.hint
        )
    })
}

/// Append a `- **<label>:** <count>` summary bullet, unless there
/// is nothing to count.
///
/// A summary block exists to say what happened, so a line saying
/// something did not happen is noise — and there are enough of
/// them that a quiet period rendered as a wall of zeroes. The
/// backends already suppressed their `applied` counts this way;
/// this applies the same rule to every line.
pub fn stat(out: &mut String, label: &str, count: usize) {
    if count > 0 {
        out.push_str(&format!("- **{label}:** {count}\n"));
    }
}

/// Append a `- **<label>:** <total> across <n> <unit>(s)` bullet,
/// unless the total is zero. `unit` is the bare noun (`project`,
/// `repo`); the `(s)` is added here.
pub fn stat_across(out: &mut String, label: &str, total: u64, n: usize, unit: &str) {
    if total > 0 {
        out.push_str(&format!("- **{label}:** {total} across {n} {unit}(s)\n"));
    }
}

/// `f` over `items`, at most `limit` at a time, the results in the
/// items' order: for remote calls with no batch form, which shared
/// services should not get all at once.
pub fn in_parallel<T: Sync, R: Send>(
    items: &[T],
    limit: usize,
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let f = &f;
    let mut out = Vec::with_capacity(items.len());
    for chunk in items.chunks(limit.max(1)) {
        std::thread::scope(|scope| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|item| scope.spawn(move || f(item)))
                .collect();
            out.extend(
                handles
                    .into_iter()
                    .map(|h| h.join().expect("a parallel task panicked")),
            );
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Landed, in_parallel, rebased_onto};

    #[test]
    fn in_parallel_keeps_the_order_and_the_bound() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let running = AtomicUsize::new(0);
        let most = AtomicUsize::new(0);
        let items: Vec<u32> = (0..20).collect();
        let doubled = in_parallel(&items, 3, |n| {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            most.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(2));
            running.fetch_sub(1, Ordering::SeqCst);
            n * 2
        });
        assert_eq!(doubled, items.iter().map(|n| n * 2).collect::<Vec<_>>());
        assert!(most.load(Ordering::SeqCst) <= 3);
        assert!(in_parallel(&[] as &[u32], 3, |n| *n).is_empty());
    }

    fn landed(title: &str, name: &str, email: &str) -> Landed {
        Landed {
            authored: None,
            title: title.into(),
            author_name: name.into(),
            author_email: email.into(),
        }
    }

    #[test]
    fn write_counts_by_groups_largest_first_then_by_name() {
        let items = [("b", 1), ("a", 2), ("c", 2), ("a", 3)];
        let mut out = String::new();
        super::write_counts_by(&mut out, &items, |i| i.0);
        assert_eq!(out, "- `a`: 2\n- `b`: 1\n- `c`: 1\n\n");
    }

    #[test]
    fn before_the_pr_only_a_commit_of_the_prs_own_moment_counts() {
        let at = |title: &str, date: &str| Landed {
            authored: Some(date.into()),
            ..landed(title, "Michel Lind", "michel@example.org")
        };
        let opened = Some("2026-11-20T11:17:51Z");
        let pr = [at(
            "meson.build: build it everywhere",
            "2026-11-18T21:53:57Z",
        )];
        // Applied as it is: authored at the PR commit's own moment.
        assert!(rebased_onto(
            &pr,
            &[at(
                "meson.build: build it everywhere",
                "2026-11-18T21:53:57Z"
            )],
            opened
        ));
        // Same title and author, but another, earlier commit.
        assert!(!rebased_onto(
            &pr,
            &[at(
                "meson.build: build it everywhere",
                "2026-11-10T09:00:00Z"
            )],
            opened
        ));
        // Authored after the PR was opened: a rebase that reset the
        // date, which counts as before.
        assert!(rebased_onto(
            &pr,
            &[at(
                "meson.build: build it everywhere",
                "2026-11-21T08:00:00Z"
            )],
            opened
        ));
    }

    #[test]
    fn the_listing_reaches_back_at_most_a_month_before_the_pr() {
        assert_eq!(
            listing_since(
                "2026-11-20T11:17:51Z",
                &[Some("2026-06-01T00:00:00Z".into())]
            ),
            "2026-10-21T11:17:51Z"
        );
    }

    #[test]
    fn rebased_onto_needs_the_same_title_and_the_same_author() {
        let pr = [landed(
            "New upstream version 3.2.16",
            "Michel Lind",
            "michel@example.org",
        )];
        // Rebased by the maintainer: same title, author preserved.
        assert!(rebased_onto(
            &pr,
            &[landed(
                "New upstream version 3.2.16",
                "Michel Lind",
                "michel@example.org"
            )],
            None
        ));
        // Reworded on a different email but the same name still counts.
        assert!(rebased_onto(
            &pr,
            &[landed(
                "New upstream version 3.2.16 ",
                "Michel Lind",
                "michel@fedoraproject.org"
            )],
            None
        ));
        // The maintainer redid the identical import themselves: superseded, not applied.
        assert!(!rebased_onto(
            &pr,
            &[landed(
                "New upstream version 3.2.16",
                "Simon Quigley",
                "simon@example.org"
            )],
            None
        ));
        // The author's own unrelated commit does not count either.
        assert!(!rebased_onto(
            &pr,
            &[landed("Team upload.", "Michel Lind", "michel@example.org")],
            None
        ));
        assert!(!rebased_onto(&pr, &[], None));
    }

    #[test]
    fn stat_omits_a_zero_and_keeps_a_count() {
        let mut out = String::new();
        stat(&mut out, "PRs opened", 0);
        assert!(out.is_empty());
        stat(&mut out, "PRs merged", 3);
        assert_eq!(out, "- **PRs merged:** 3\n");
    }

    #[test]
    fn stat_across_omits_a_zero_total_and_pluralizes_the_unit() {
        let mut out = String::new();
        stat_across(&mut out, "Releases published", 0, 0, "repo");
        assert!(out.is_empty());
        stat_across(&mut out, "Commits authored", 12, 2, "repo");
        assert_eq!(out, "- **Commits authored:** 12 across 2 repo(s)\n");
    }
    use super::*;

    #[test]
    fn a_branch_is_listed_from_the_prs_earliest_commit() {
        let some = |d: &str| Some(d.to_string());
        // rasdaemon#272: its commit, applied as it is, is dated before
        // the PR was opened.
        assert_eq!(
            listing_since(
                "2026-11-20T11:17:51Z",
                &[
                    some("2026-11-18T21:53:57Z"),
                    some("2026-11-20T09:00:00+02:00")
                ]
            ),
            "2026-11-18T21:53:57Z"
        );
        // An offset is compared as the instant it is.
        assert_eq!(
            listing_since("2026-11-20T11:17:51Z", &[some("2026-11-20T12:00:00+02:00")]),
            "2026-11-20T10:00:00Z"
        );
        // Nothing earlier, or nothing that parses: the PR's opening.
        assert_eq!(
            listing_since("2026-11-20T11:17:51Z", &[None, some("not a date")]),
            "2026-11-20T11:17:51Z"
        );
        assert_eq!(listing_since("garbled", &[]), "garbled");
    }

    const GITLAB: TokenSpec = TokenSpec {
        service: "GitLab",
        label: "sandogasa: GitLab token",
        generic_env: "GITLAB_TOKEN",
        host_separators: &['.'],
        hint: "",
    };

    #[test]
    fn date_in_range_is_inclusive_and_date_only() {
        let s = NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
        let u = NaiveDate::from_ymd_opt(2026, 6, 30).unwrap();
        assert!(date_in_range("2026-06-01T00:00:00Z", s, u));
        assert!(date_in_range("2026-06-30T23:59:59+02:00", s, u));
        assert!(!date_in_range("2026-07-01T00:00:00Z", s, u));
        assert!(!date_in_range("garbage", s, u));
    }

    #[test]
    fn instance_host_strips_scheme_and_slash() {
        assert_eq!(instance_host("https://gitlab.com/"), "gitlab.com");
        assert_eq!(instance_host("http://localhost:8080"), "localhost:8080");
    }

    #[test]
    fn instance_token_env_uses_spec_separators() {
        assert_eq!(
            instance_token_env("https://salsa.debian.org/", &GITLAB),
            "GITLAB_TOKEN_SALSA_DEBIAN_ORG"
        );
        let sourcehut = TokenSpec {
            host_separators: &['.', '-'],
            generic_env: "SOURCEHUT_TOKEN",
            ..GITLAB
        };
        assert_eq!(
            instance_token_env("git.sr-ht.test", &sourcehut),
            "SOURCEHUT_TOKEN_GIT_SR_HT_TEST"
        );
    }

    #[test]
    #[serial_test::serial]
    fn find_token_error_names_both_vars_and_hint() {
        let spec = TokenSpec {
            hint: " (generate at example.test)",
            ..GITLAB
        };
        // Skip if the env vars happen to be set — the fake
        // hostname keeps that unlikely, but a shell that exported
        // GITLAB_TOKEN would otherwise short-circuit the lookup.
        if std::env::var(instance_token_env(
            "https://nonexistent.example.test",
            &spec,
        ))
        .is_ok()
            || std::env::var("GITLAB_TOKEN").is_ok()
        {
            return;
        }
        let err =
            find_token("https://nonexistent.example.test", &BTreeMap::new(), &spec).unwrap_err();
        assert!(err.contains("no GitLab token for nonexistent.example.test"));
        assert!(err.contains("GITLAB_TOKEN_NONEXISTENT_EXAMPLE_TEST (instance-specific)"));
        assert!(err.contains("GITLAB_TOKEN (generic)"));
        assert!(err.ends_with("(generate at example.test)"));
    }
}
