// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Sourcehut (sr.ht) activity reporting. sr.ht has no unified PR model,
//! so this pulls three independent services and presents them together:
//!
//! - **lists.sr.ht** — patchsets the user submitted (the PR analog);
//!   `status == APPLIED` is the merged analog.
//! - **todo.sr.ht** — tickets the user opened / closed, derived from the
//!   authenticated user's event feed (so it only populates when reporting
//!   on the token owner — see `sandogasa_sourcehut`'s notes).
//! - **git.sr.ht** — commits landed in the user's own repos, split into
//!   yours vs third-party by author email (see `collect_commits`).
//!
//! Each service is independent: a failure in one is warned about and its
//! section left empty, rather than sinking the whole domain.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use sandogasa_sourcehut::{Actor, Client, Event, Patchset};
use serde::Serialize;

use crate::config::SourcehutConfig;

/// A user's Sourcehut activity for a single domain.
#[derive(Debug, Default, Serialize)]
pub struct SourcehutReport {
    /// Instance host the report was fetched from.
    pub instance: String,
    /// sr.ht login reported on.
    pub user: String,

    /// Patchsets the user submitted in the window.
    pub patches: Vec<PatchItem>,
    /// Tickets the user opened in the window.
    pub opened_tickets: Vec<TicketItem>,
    /// Tickets the user resolved in the window.
    pub closed_tickets: Vec<TicketItem>,
    /// Commits landed in the user's own repos in the window.
    pub commits: Vec<CommitItem>,
}

/// A submitted patchset for the report's lists.
#[derive(Debug, Clone, Serialize)]
pub struct PatchItem {
    pub subject: String,
    /// Mailing list it was sent to.
    pub list: String,
    /// Patchset status (`PROPOSED` … `APPLIED`).
    pub status: String,
}

impl PatchItem {
    /// Whether the patchset was applied (the merged analog).
    fn applied(&self) -> bool {
        self.status == "APPLIED"
    }
}

/// A ticket reference for the report's lists.
#[derive(Debug, Clone, Serialize)]
pub struct TicketItem {
    /// Canonical cross-tracker reference, e.g. `~user/tracker#3`.
    pub reference: String,
    pub subject: String,
}

/// A commit in one of the user's repos, for the report's lists.
#[derive(Debug, Clone, Serialize)]
pub struct CommitItem {
    /// Repository name it landed in.
    pub repo: String,
    /// Commit hash.
    pub id: String,
    /// Commit subject (first line of the message).
    pub subject: String,
    /// Commit author's display name.
    pub author: String,
    /// Whether it's attributed to the reported user (author email is one
    /// of theirs, or the `*` wildcard is set) vs a third party.
    pub owner: bool,
}

/// Build the Sourcehut activity report for one domain. Each of the three
/// services is queried independently; a service error is logged (under
/// `--verbose`) and its section left empty rather than failing the run.
pub fn sourcehut_report(
    cfg: &SourcehutConfig,
    user: &str,
    since: NaiveDate,
    until: NaiveDate,
    tokens: &BTreeMap<String, String>,
    owner_emails: &[String],
    verbose: bool,
) -> Result<SourcehutReport, String> {
    let token = find_token(&cfg.instance, tokens)?;
    let client = Client::new(&cfg.instance, &token)
        .map_err(|e| format!("Sourcehut client setup for {}: {e}", cfg.instance))?;
    // RFC3339 lower bound for the paginated-until-older service calls;
    // the exact window is applied by `date_in_range` below.
    let since_ts = format!("{since}T00:00:00Z");

    let mut report = SourcehutReport {
        instance: cfg.instance.clone(),
        user: user.to_string(),
        ..Default::default()
    };

    // lists.sr.ht — patches.
    if verbose {
        eprintln!("[sourcehut] {}: patches for ~{user}", cfg.instance);
    }
    match client.patches(user) {
        Ok(patches) => report.patches = filter_patches(&patches, since, until),
        Err(e) => warn(
            verbose,
            &format!("Sourcehut patches ({}): {e}", cfg.instance),
        ),
    }

    // todo.sr.ht — tickets (from the token owner's event feed).
    if verbose {
        eprintln!("[sourcehut] {}: ticket events for ~{user}", cfg.instance);
    }
    match client.ticket_events(&since_ts) {
        Ok(events) => {
            let (opened, closed) = classify_tickets(&events, user, since, until);
            report.opened_tickets = opened;
            report.closed_tickets = closed;
        }
        Err(e) => warn(
            verbose,
            &format!("Sourcehut tickets ({}): {e}", cfg.instance),
        ),
    }

    // git.sr.ht — commits in the user's own repos (tagged owner vs
    // third-party by author email).
    if verbose {
        eprintln!("[sourcehut] {}: commits for ~{user}", cfg.instance);
    }
    report.commits = match collect_commits(
        &client,
        user,
        since,
        until,
        &since_ts,
        owner_emails,
        verbose,
    ) {
        Ok(commits) => commits,
        Err(e) => {
            warn(
                verbose,
                &format!("Sourcehut commits ({}): {e}", cfg.instance),
            );
            Vec::new()
        }
    };

    Ok(report)
}

/// Enumerate the user's own repos and collect commits landed there within
/// the window, tagging each as the user's own or a third party's.
///
/// sr.ht exposes only the account's *primary* email (meta.sr.ht has no
/// secondary-emails list), so ownership is decided against a set of the
/// user's git emails: the account primary plus any configured in
/// `owner_emails`, or everything when `owner_emails` contains `"*"`.
/// A commit whose author email isn't in that set (e.g. an applied patch
/// from someone else) is kept but marked third-party.
fn collect_commits(
    client: &Client,
    user: &str,
    since: NaiveDate,
    until: NaiveDate,
    since_ts: &str,
    owner_emails: &[String],
    verbose: bool,
) -> Result<Vec<CommitItem>, String> {
    let wildcard = owner_emails.iter().any(|e| e.trim() == "*");
    let mut owned = owned_email_set(owner_emails);
    if !wildcard {
        // Seed with the account primary email (best-effort — a failure
        // just means fewer commits are attributed as the user's).
        match client.user_email(user) {
            Ok(Some(email)) if !email.is_empty() => {
                owned.insert(email.to_ascii_lowercase());
            }
            Ok(_) => {}
            Err(e) => warn(verbose, &format!("Sourcehut account email: {e}")),
        }
    }

    let repos = client
        .repositories(user)
        .map_err(|e| format!("repositories: {e}"))?;
    let mut out = Vec::new();
    for repo in repos {
        let commits = match client.commits_since(user, &repo.name, since_ts) {
            Ok(c) => c,
            Err(e) => {
                // e.g. an empty repo with no default branch ("reference
                // not found") — skip it, don't fail the section.
                warn(verbose, &format!("Sourcehut log for {}: {e}", repo.name));
                continue;
            }
        };
        for c in commits {
            if date_in_range(&c.author.time, since, until) {
                let subject = c.message.lines().next().unwrap_or_default().to_string();
                out.push(CommitItem {
                    repo: repo.name.clone(),
                    id: c.id,
                    subject,
                    author: c.author.name.clone(),
                    owner: wildcard || owned.contains(&c.author.email.to_ascii_lowercase()),
                });
            }
        }
    }
    Ok(out)
}

/// Lower-cased set of configured owner emails (excluding the `*`
/// wildcard, which the caller handles separately).
fn owned_email_set(owner_emails: &[String]) -> std::collections::HashSet<String> {
    owner_emails
        .iter()
        .map(|e| e.trim())
        .filter(|e| !e.is_empty() && *e != "*")
        .map(|e| e.to_ascii_lowercase())
        .collect()
}

/// Keep the patchsets submitted within `[since, until]`.
fn filter_patches(patches: &[Patchset], since: NaiveDate, until: NaiveDate) -> Vec<PatchItem> {
    patches
        .iter()
        .filter(|p| date_in_range(&p.created, since, until))
        .map(|p| PatchItem {
            subject: p.subject.clone(),
            list: p.list.name.clone(),
            status: p.status.clone(),
        })
        .collect()
}

/// From the event feed, the tickets `user` opened and resolved within the
/// window. A ticket is *opened* by a `CREATED` change whose author is the
/// user, and *closed* by a `STATUS_CHANGE` to `RESOLVED` whose editor is
/// the user. Deduplicated by ticket reference.
fn classify_tickets(
    events: &[Event],
    user: &str,
    since: NaiveDate,
    until: NaiveDate,
) -> (Vec<TicketItem>, Vec<TicketItem>) {
    let mut opened: BTreeMap<String, TicketItem> = BTreeMap::new();
    let mut closed: BTreeMap<String, TicketItem> = BTreeMap::new();
    for ev in events {
        if !date_in_range(&ev.created, since, until) {
            continue;
        }
        let item = || TicketItem {
            reference: ev.ticket.reference.clone(),
            subject: ev.ticket.subject.clone(),
        };
        for ch in &ev.changes {
            match ch.event_type.as_str() {
                "CREATED" if actor_matches(&ch.author, user) => {
                    opened
                        .entry(ev.ticket.reference.clone())
                        .or_insert_with(item);
                }
                "STATUS_CHANGE"
                    if actor_matches(&ch.editor, user)
                        && ch.new_status.as_deref() == Some("RESOLVED") =>
                {
                    closed
                        .entry(ev.ticket.reference.clone())
                        .or_insert_with(item);
                }
                _ => {}
            }
        }
    }
    (
        opened.into_values().collect(),
        closed.into_values().collect(),
    )
}

/// Whether an actor is the reported user (matching the sr.ht canonical
/// name `~username`, ignoring a leading `~` and case).
fn actor_matches(actor: &Option<Actor>, user: &str) -> bool {
    actor.as_ref().is_some_and(|a| {
        a.canonical_name
            .trim_start_matches('~')
            .eq_ignore_ascii_case(user.trim_start_matches('~'))
    })
}

/// Format the Sourcehut section as Markdown.
pub fn format_markdown(report: &SourcehutReport, detail: u8) -> String {
    let heading = "### Sourcehut\n\n".to_string();
    if report.patches.is_empty()
        && report.opened_tickets.is_empty()
        && report.closed_tickets.is_empty()
        && report.commits.is_empty()
    {
        return format!("{heading}No Sourcehut activity.\n\n");
    }

    let applied = report.patches.iter().filter(|p| p.applied()).count();
    let repos = report
        .commits
        .iter()
        .map(|c| c.repo.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len();

    let mut out = heading;
    out.push_str(&format!("- **Patches sent:** {}\n", report.patches.len()));
    if applied > 0 {
        out.push_str(&format!("- **Patches applied:** {applied}\n"));
    }
    out.push_str(&format!(
        "- **Tickets opened:** {}\n",
        report.opened_tickets.len()
    ));
    out.push_str(&format!(
        "- **Tickets closed:** {}\n",
        report.closed_tickets.len()
    ));
    let own: Vec<&CommitItem> = report.commits.iter().filter(|c| c.owner).collect();
    let others: Vec<&CommitItem> = report.commits.iter().filter(|c| !c.owner).collect();
    out.push_str(&format!(
        "- **Commits by you:** {} across {repos} repo(s)\n",
        own.len()
    ));
    // Third-party commits landed in your repos (e.g. patches you applied
    // that preserve the submitter as author) — shown only when present.
    if !others.is_empty() {
        out.push_str(&format!(
            "- **Commits by others (in your repos):** {}\n",
            others.len()
        ));
    }
    out.push('\n');

    if detail < 1 {
        return out;
    }

    if !report.patches.is_empty() {
        out.push_str("#### Patches sent\n\n");
        for p in &report.patches {
            out.push_str(&format!("- {} → {} ({})\n", p.subject, p.list, p.status));
        }
        out.push('\n');
    }
    if !report.opened_tickets.is_empty() {
        out.push_str("#### Tickets opened\n\n");
        write_tickets(&mut out, &report.opened_tickets);
    }
    if !report.closed_tickets.is_empty() {
        out.push_str("#### Tickets closed\n\n");
        write_tickets(&mut out, &report.closed_tickets);
    }
    // Commits: at `--detailed`, per-repo counts (like the other forges);
    // at `--detailed --detailed`, the individual commits with subjects.
    write_commits(&mut out, &own, &others, detail >= 2);
    out
}

/// Render the commit section for the detail levels. `deep` (level ≥ 2)
/// lists individual commits with their subject; otherwise it's a per-repo
/// count breakdown, matching the github/gitlab presentation.
fn write_commits(out: &mut String, own: &[&CommitItem], others: &[&CommitItem], deep: bool) {
    if own.is_empty() && others.is_empty() {
        return;
    }
    if deep {
        if !own.is_empty() {
            out.push_str("#### Commits\n\n");
            for c in own {
                out.push_str(&format!(
                    "- `{}` {} {}\n",
                    c.repo,
                    short_id(&c.id),
                    c.subject
                ));
            }
            out.push('\n');
        }
        if !others.is_empty() {
            out.push_str("#### Commits by others (in your repos)\n\n");
            for c in others {
                out.push_str(&format!(
                    "- `{}` {} {} — {}\n",
                    c.repo,
                    short_id(&c.id),
                    c.subject,
                    c.author
                ));
            }
            out.push('\n');
        }
        return;
    }
    // Per-repo counts (own, with a "+N by others" annotation when any).
    out.push_str("#### Commits by repo\n\n");
    let mut own_by_repo: BTreeMap<&str, usize> = BTreeMap::new();
    let mut other_by_repo: BTreeMap<&str, usize> = BTreeMap::new();
    for c in own {
        *own_by_repo.entry(c.repo.as_str()).or_default() += 1;
    }
    for c in others {
        *other_by_repo.entry(c.repo.as_str()).or_default() += 1;
    }
    let repos: std::collections::BTreeSet<&str> = own_by_repo
        .keys()
        .chain(other_by_repo.keys())
        .copied()
        .collect();
    for repo in repos {
        let mine = own_by_repo.get(repo).copied().unwrap_or(0);
        let theirs = other_by_repo.get(repo).copied().unwrap_or(0);
        if theirs > 0 {
            out.push_str(&format!("- `{repo}`: {mine} (+{theirs} by others)\n"));
        } else {
            out.push_str(&format!("- `{repo}`: {mine}\n"));
        }
    }
    out.push('\n');
}

/// First 8 chars of a commit hash.
fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

fn write_tickets(out: &mut String, tickets: &[TicketItem]) {
    for t in tickets {
        out.push_str(&format!("- {} {}\n", t.reference, t.subject));
    }
    out.push('\n');
}

/// Log a warning to stderr (only under `--verbose`; a quiet run silently
/// omits the failed section, matching the "one service down ≠ fatal"
/// policy).
fn warn(verbose: bool, msg: &str) {
    if verbose {
        eprintln!("warning: {msg}");
    }
}

/// Whether an RFC3339 timestamp's date falls within `[since, until]`
/// (inclusive). Only the date part is considered.
fn date_in_range(ts: &str, since: NaiveDate, until: NaiveDate) -> bool {
    let Some(day) = ts.split('T').next() else {
        return false;
    };
    NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map(|d| d >= since && d <= until)
        .unwrap_or(false)
}

/// Look up the Sourcehut token for an instance.
///
/// Order: instance-specific env var → generic env var →
/// `sourcehut_tokens.<host>` from the user overlay → error.
fn find_token(instance: &str, tokens: &BTreeMap<String, String>) -> Result<String, String> {
    let var = instance_token_env(instance);
    if let Ok(t) = std::env::var(&var) {
        return Ok(t);
    }
    if let Ok(t) = std::env::var("SOURCEHUT_TOKEN") {
        return Ok(t);
    }
    let host = instance_host(instance);
    if let Some(t) = tokens.get(&host) {
        return Ok(t.clone());
    }
    Err(format!(
        "no Sourcehut token for {host}: set {var} (instance-specific), \
         SOURCEHUT_TOKEN (generic), or run `sandogasa-report config` to \
         store one in the overlay (generate at meta.sr.ht/oauth2/personal-token)"
    ))
}

fn instance_token_env(instance: &str) -> String {
    format!(
        "SOURCEHUT_TOKEN_{}",
        instance_host(instance)
            .to_uppercase()
            .replace(['.', '-'], "_")
    )
}

/// Strip scheme + trailing slash to get the bare host — the token-keying
/// host.
pub(crate) fn instance_host(instance: &str) -> String {
    instance
        .trim_end_matches('/')
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patchset(json: &str) -> Patchset {
        serde_json::from_str(json).unwrap()
    }

    fn event(json: &str) -> Event {
        serde_json::from_str(json).unwrap()
    }

    fn win() -> (NaiveDate, NaiveDate) {
        (
            NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 6, 30).unwrap(),
        )
    }

    #[test]
    fn date_in_range_inclusive() {
        let (s, u) = win();
        assert!(date_in_range("2026-06-01T00:00:00Z", s, u));
        assert!(date_in_range("2026-06-30T23:59:59Z", s, u));
        assert!(!date_in_range("2026-05-31T00:00:00Z", s, u));
        assert!(!date_in_range("garbage", s, u));
    }

    #[test]
    fn filter_patches_windows_and_flags_applied() {
        let (s, u) = win();
        let patches = vec![
            patchset(
                r#"{"created":"2026-06-10T00:00:00Z","subject":"a","status":"APPLIED","list":{"name":"devel"}}"#,
            ),
            patchset(
                r#"{"created":"2026-05-01T00:00:00Z","subject":"old","status":"APPLIED","list":{"name":"devel"}}"#,
            ),
            patchset(
                r#"{"created":"2026-06-20T00:00:00Z","subject":"b","status":"PROPOSED","list":{"name":"devel"}}"#,
            ),
        ];
        let items = filter_patches(&patches, s, u);
        assert_eq!(items.len(), 2);
        assert_eq!(items.iter().filter(|p| p.applied()).count(), 1);
    }

    #[test]
    fn classify_tickets_opened_closed_by_user_only() {
        let (s, u) = win();
        let events = vec![
            // Opened by the user.
            event(
                r#"{"created":"2026-06-05T00:00:00Z","ticket":{"ref":"~m/p#1","subject":"Bug"},
                    "changes":[{"eventType":"CREATED","author":{"canonicalName":"~michel"}}]}"#,
            ),
            // Resolved by the user.
            event(
                r#"{"created":"2026-06-10T00:00:00Z","ticket":{"ref":"~m/p#2","subject":"Fix"},
                    "changes":[{"eventType":"STATUS_CHANGE","editor":{"canonicalName":"~michel"},
                                "newStatus":"RESOLVED","newResolution":"FIXED"}]}"#,
            ),
            // Created by someone else (subscribed feed) → ignored.
            event(
                r#"{"created":"2026-06-12T00:00:00Z","ticket":{"ref":"~m/p#3","subject":"Other"},
                    "changes":[{"eventType":"CREATED","author":{"canonicalName":"~someone"}}]}"#,
            ),
            // In-feed but out of window → ignored.
            event(
                r#"{"created":"2026-05-01T00:00:00Z","ticket":{"ref":"~m/p#4","subject":"Old"},
                    "changes":[{"eventType":"CREATED","author":{"canonicalName":"~michel"}}]}"#,
            ),
        ];
        let (opened, closed) = classify_tickets(&events, "michel", s, u);
        assert_eq!(opened.len(), 1);
        assert_eq!(opened[0].reference, "~m/p#1");
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].reference, "~m/p#2");
    }

    #[test]
    fn actor_matches_ignores_tilde_and_case() {
        let a: Option<Actor> = serde_json::from_str(r#"{"canonicalName":"~Michel"}"#).ok();
        assert!(actor_matches(&a, "michel"));
        assert!(actor_matches(&a, "~michel"));
        assert!(!actor_matches(&a, "other"));
        assert!(!actor_matches(&None, "michel"));
    }

    #[test]
    fn format_empty_and_populated() {
        let empty = SourcehutReport {
            instance: "sr.ht".into(),
            user: "michel".into(),
            ..Default::default()
        };
        let md = format_markdown(&empty, 1);
        assert!(md.contains("### Sourcehut\n"));
        assert!(md.contains("No Sourcehut activity"));

        let report = SourcehutReport {
            instance: "sr.ht".into(),
            user: "michel".into(),
            patches: vec![PatchItem {
                subject: "[PATCH] x".into(),
                list: "devel".into(),
                status: "APPLIED".into(),
            }],
            opened_tickets: vec![TicketItem {
                reference: "~m/p#1".into(),
                subject: "Bug".into(),
            }],
            commits: vec![
                CommitItem {
                    repo: "dotfiles".into(),
                    id: "abcdef1234".into(),
                    subject: "Add foo".into(),
                    author: "Michel".into(),
                    owner: true,
                },
                CommitItem {
                    repo: "dotfiles".into(),
                    id: "99887766aa".into(),
                    subject: "Fix bar".into(),
                    author: "Contributor".into(),
                    owner: false,
                },
            ],
            ..Default::default()
        };

        // --detailed (level 1): counts + per-repo commit breakdown.
        let md = format_markdown(&report, 1);
        assert!(md.contains("- **Patches sent:** 1"));
        assert!(md.contains("- **Patches applied:** 1"));
        assert!(md.contains("- **Tickets opened:** 1"));
        assert!(md.contains("- **Commits by you:** 1 across 1 repo(s)"));
        assert!(md.contains("- **Commits by others (in your repos):** 1"));
        assert!(md.contains("#### Patches sent"));
        assert!(md.contains("[PATCH] x → devel (APPLIED)"));
        // Per-repo counts, not individual hashes, at level 1.
        assert!(md.contains("#### Commits by repo"));
        assert!(md.contains("`dotfiles`: 1 (+1 by others)"));
        assert!(!md.contains("abcdef12"));

        // --detailed --detailed (level 2): individual commits + subjects.
        let deep = format_markdown(&report, 2);
        assert!(deep.contains("#### Commits\n"));
        assert!(deep.contains("`dotfiles` abcdef12 Add foo"));
        assert!(deep.contains("#### Commits by others (in your repos)"));
        assert!(deep.contains("`dotfiles` 99887766 Fix bar — Contributor"));
    }

    #[test]
    fn owned_email_set_normalizes_and_drops_wildcard() {
        let set = owned_email_set(&[
            "  Salimma@Fedoraproject.org ".to_string(),
            "*".to_string(),
            "".to_string(),
        ]);
        assert!(set.contains("salimma@fedoraproject.org"));
        assert!(!set.contains("*"));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn instance_token_env_name() {
        assert_eq!(instance_token_env("sr.ht"), "SOURCEHUT_TOKEN_SR_HT");
    }

    #[test]
    fn find_token_errors_when_unset() {
        let var = instance_token_env("nonexistent.example.test");
        if std::env::var(&var).is_ok() || std::env::var("SOURCEHUT_TOKEN").is_ok() {
            return;
        }
        let err = find_token("nonexistent.example.test", &BTreeMap::new()).unwrap_err();
        assert!(err.contains("no Sourcehut token"));
    }

    #[test]
    fn find_token_falls_back_to_config() {
        let var = instance_token_env("nonexistent.example.test");
        if std::env::var(&var).is_ok() || std::env::var("SOURCEHUT_TOKEN").is_ok() {
            return;
        }
        let mut tokens = BTreeMap::new();
        tokens.insert(
            "nonexistent.example.test".to_string(),
            "from-config".to_string(),
        );
        let tok = find_token("nonexistent.example.test", &tokens).unwrap();
        assert_eq!(tok, "from-config");
    }

    #[test]
    fn instance_host_strips_scheme() {
        assert_eq!(instance_host("https://sr.ht/"), "sr.ht");
        assert_eq!(instance_host("sr.ht"), "sr.ht");
    }
}
