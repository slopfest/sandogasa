// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Branch-request filing and escalation.
//!
//! Ports the EPEL branch-request workflow from the old Python
//! `ebranch`: file a Bugzilla bug asking a maintainer to branch
//! and build a package for an EPEL release, then escalate
//! (`needinfo?`) requests that sit untouched.
//!
//! Three entry points back the CLI subcommands:
//!
//! - [`file_one`] — file a single request.
//! - [`file_batch`] — file requests for every missing package in
//!   a `resolve --report` or `check-crate --toml` report and link
//!   them together along the dependency graph (a package's request
//!   depends on the requests for the packages it needs).
//! - [`escalate`] — ping requests that have been NEW for at least
//!   a week.

use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal;

use sandogasa_bugzilla::BzClient;

use crate::check_crate::{self, CheckCrateReport};
use crate::resolve::{self, BranchRequest, ResolveReport};

/// Minimum age (days) before a NEW request is escalated.
pub const PING_MIN_DAYS: i64 = 7;

/// Options shared by the branch-request subcommands.
pub struct Options {
    pub bugzilla_url: String,
    pub api_key: String,
    pub branch: String,
    /// FAS to volunteer as co-maintainer in the bug text, when the
    /// asker is volunteering. Distinct from [`Options::identity`]:
    /// this one is a offer, not a statement of who is running the
    /// tool.
    pub fas: Option<String>,
    /// Who is running the tool, for asking dist-git what access they
    /// hold and for naming them in a releng ticket. True whether or
    /// not they are volunteering, so it comes from `packager.fas`
    /// even when `--fas` is absent.
    pub identity: Option<String>,
    pub sig: Option<String>,
    pub dry_run: bool,
    pub verbose: bool,
    /// Base-distro branch behind `branch` (e.g. `c10s` for epel10).
    /// When set, packages present there are refused: EPEL must not
    /// replace base-distro packages, and a branch request for one is
    /// always CANTFIX (rhbz#2482250).
    pub base_branch: Option<String>,
    /// Bugzilla account to assign a request to when the asker files it
    /// about work they could have done themselves — bookkeeping they
    /// own rather than a question for a maintainer.
    pub email: Option<String>,
    /// Whether the asker holds provenpackager, from the config file.
    /// Only ever adds a note to a build request; see
    /// [`note_provenpackager`].
    pub provenpackager: bool,
    /// Fedora's Forgejo, where a stalled request's releng ticket goes.
    pub forge_url: String,
    /// Token for [`Options::forge_url`]. Empty means `escalate`
    /// reports that a request has earned its ticket rather than
    /// filing one -- the work is still shown, never discarded.
    pub forge_token: String,
    /// Skip the per-ticket confirmation (`--yes`).
    pub assume_yes: bool,
}

// ---- request / ping body templates (ported) ----

/// What a request is actually asking for.
///
/// A branch can exist for months with nothing built in it — `et` was
/// branched for epel10 long before any build — and such a package
/// still wants a bug: it anchors the `depends_on` edges of everything
/// waiting on it. What it must not say is "please branch", which is
/// done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    /// No branch yet: ask for the branch and the build.
    Branch,
    /// Branched already: ask for the build alone.
    Build,
    /// The branch was just asked for by the person filing: the bug is
    /// their record of it, and the anchor for what depends on it.
    Requested,
}

impl RequestKind {
    /// The summary line, which is also what [`adoptable`] matches and
    /// what `sandogasa_bugclass` parses.
    pub fn summary(self, pkg: &str, branch: &str) -> String {
        match self {
            // A self-served request keeps the asking summary: it is
            // what `adoptable` matches and what sandogasa-bugclass
            // parses, so a re-run finds this bug like any other.
            RequestKind::Branch | RequestKind::Requested => {
                format!("Please branch and build {pkg} in {branch}")
            }
            RequestKind::Build => format!("Please build {pkg} in {branch}"),
        }
    }

    /// What the request asks for, for a one-line report.
    pub fn asked_for(self) -> &'static str {
        match self {
            RequestKind::Branch => "branch",
            RequestKind::Build => "build",
            RequestKind::Requested => "record of a branch",
        }
    }

    /// The opening line of the description.
    fn asking(self, pkg: &str, branch: &str) -> String {
        match self {
            RequestKind::Branch => format!("Please branch and build {pkg} in {branch}.\n"),
            RequestKind::Build => format!(
                "{branch} already has a {pkg} branch in dist-git, but nothing \
                 built from it.\nPlease build {pkg} in {branch}.\n"
            ),
            RequestKind::Requested => format!(
                "I have asked for the {branch} branch of {pkg} myself, with\n\
                 fedpkg request-branch.\n\
                 \n\
                 This bug records that and anchors the requests that depend \
                 on it.\n"
            ),
        }
    }
}

/// Build the request description, choosing the co-maintainer
/// offer based on whether a FAS and/or SIG was given. Returns an
/// error for the invalid SIG-without-FAS combination (you can't
/// volunteer a SIG to co-maintain without volunteering yourself).
pub fn request_description(
    pkg: &str,
    branch: &str,
    fas: Option<&str>,
    sig: Option<&str>,
) -> Result<String, String> {
    request_description_for(RequestKind::Branch, pkg, branch, fas, sig)
}

/// [`request_description`] for a given kind of request.
pub fn request_description_for(
    kind: RequestKind,
    pkg: &str,
    branch: &str,
    fas: Option<&str>,
    sig: Option<&str>,
) -> Result<String, String> {
    let base = kind.asking(pkg, branch);
    // Nobody is being asked for anything on a bug you filed about
    // your own package, so the co-maintainer offer has no place on it.
    if kind == RequestKind::Requested {
        return Ok(base);
    }
    match (fas, sig) {
        (None, None) => Ok(base),
        // Each action URL sits alone on its own line so it linkifies
        // cleanly in Bugzilla — casual packagers often don't know
        // where users and groups are added, so give them a link to
        // click: /adduser for a person (--fas), /addgroup for a SIG
        // (--sig).
        (Some(fas), None) => Ok(format!(
            "{base}\n\
             If you do not wish to maintain {pkg} in {branch},\n\
             or do not think you will be able to do this in a timely manner,\n\
             I would be happy to be a co-maintainer of the package (FAS: {fas});\n\
             please add me through\n\
             https://src.fedoraproject.org/rpms/{pkg}/adduser\n"
        )),
        (Some(fas), Some(sig)) => Ok(format!(
            "{base}\n\
             If you do not wish to maintain {pkg} in {branch},\n\
             or do not think you will be able to do this in a timely manner,\n\
             the {sig} would be happy to be a co-maintainer of the package;\n\
             please add the {sig} group through\n\
             https://src.fedoraproject.org/rpms/{pkg}/addgroup\n\
             and grant it commit access, or collaborator access on epel* branches.\n\
             \n\
             Please also add me as a co-maintainer (FAS: {fas}) through\n\
             https://src.fedoraproject.org/rpms/{pkg}/adduser\n"
        )),
        (None, Some(_)) => Err("cannot request a SIG be given access to a package without \
             signing up to co-maintain yourself (pass --fas as well)"
            .to_string()),
    }
}

/// Build the escalation (`needinfo?`) comment body.
pub fn ping_body(pkg: &str, branch: &str, fas: Option<&str>, sig: Option<&str>) -> String {
    let mut body = format!("Will you be able to branch and build {pkg} in {branch}?\n");
    match (fas, sig) {
        (Some(fas), Some(sig)) => {
            body.push_str(&format!(
                "\nThe {sig} would be happy to be a co-maintainer of the package\n\
                 if you do not wish to build it on {branch}.\n\
                 \n\
                 I would also be happy to be a co-maintainer (FAS: {fas}).\n"
            ));
        }
        (Some(fas), None) => {
            body.push_str(&format!(
                "\nI would be happy to be a co-maintainer if you do not wish\n\
                 to build it on {branch} (FAS: {fas}).\n"
            ));
        }
        (None, Some(sig)) => {
            body.push_str(&format!(
                "\nThe {sig} would be happy to be a co-maintainer of the package\n\
                 if you do not wish to build it on {branch}.\n"
            ));
        }
        (None, None) => {}
    }
    body
}

/// Outcome of deciding whether a request should be escalated.
#[derive(Debug, PartialEq, Eq)]
pub enum PingDecision {
    /// Escalate now.
    Ping,
    /// Already escalated previously.
    AlreadyPinged,
    /// Bug is closed — skip without comment.
    Closed,
    /// Bug is in a non-NEW open state; surface for manual review.
    NotNew(String),
    /// Bug is too recent; wait longer.
    TooNew(i64),
    /// The asker can do whatever is left, so no one else is owed
    /// anything. `branched` says which follow-up applies: the branch
    /// exists and the bug wants closing, or it does not and the asker
    /// should make it.
    NothingOwed { branched: bool },
    /// The request is assigned to the person running ebranch, so the
    /// ball is already in their court.
    Yours,
}

/// Decide whether to escalate a request given its current state.
///
/// `may_branch` is the asker's standing from dist-git (see
/// [`BranchStanding::may_branch`]) and `branched` whether the branch
/// is already there. They are passed as plain booleans to keep this
/// pure: the lookup belongs to the caller, which already has a client.
///
/// A ping asks a package maintainer for something. So it is only
/// warranted when there is something they can do that the asker
/// cannot -- branching, or building. Commit-equivalent access carries
/// both, which is why `may_branch` alone settles it. An existing
/// branch does not: a branch the asker cannot commit to still needs
/// its maintainer to build there, and skipping on that would silence
/// exactly the request that still needs a person.
pub fn ping_decision(
    status: &str,
    days_since_created: i64,
    already_pinged: bool,
    may_branch: bool,
    branched: bool,
    assigned_to_asker: bool,
) -> PingDecision {
    // A closed bug is settled whoever can act on it.
    if status == "CLOSED" {
        return PingDecision::Closed;
    }
    // Handing the bug to the person chasing it is how a maintainer
    // says they are done with it -- it usually arrives with the branch
    // and the ACL. Pinging anyway would set `needinfo?` on
    // `assigned_to`, which is now that same person: ebranch asking its
    // own user for information about their own request.
    if assigned_to_asker {
        return PingDecision::Yours;
    }
    if may_branch {
        return PingDecision::NothingOwed { branched };
    }
    if already_pinged {
        return PingDecision::AlreadyPinged;
    }
    match status {
        "NEW" if days_since_created < PING_MIN_DAYS => PingDecision::TooNew(days_since_created),
        "NEW" => PingDecision::Ping,
        other => PingDecision::NotNew(other.to_string()),
    }
}

/// Days a `needinfo?` must go unanswered before the EPEL policy
/// allows a releng ticket. Counted from the ping, not from the bug.
pub const RELENG_MIN_DAYS: i64 = 14;

/// Labels Fedora's releng tracker expects on a stalled-EPEL ticket.
/// From its own issue template, which triage reads.
pub const RELENG_LABELS: [&str; 3] = ["ops", "points/01", "Priority/Low"];

/// The releng tracker: `releng/tickets` on Fedora's Forgejo.
pub const RELENG_REPO: (&str, &str) = ("releng", "tickets");

/// What to do about a request whose ping has already gone out.
#[derive(Debug, PartialEq, Eq)]
pub enum RelengDecision {
    /// The needinfo has gone unanswered long enough: file the ticket.
    File(i64),
    /// A ticket is already recorded for this request.
    Filed(u64),
    /// Inside the two weeks still.
    TooSoon(i64),
    /// No outstanding `needinfo?` to time from: answered, cleared by
    /// hand, or never set. Needs a person, not a rule -- see
    /// [`releng_decision`].
    NoNeedinfo,
    /// The bug moved out of NEW, which is action by the policy's own
    /// definition.
    Progressed(String),
}

/// Decide whether a pinged request has earned a releng ticket.
///
/// `days_since_needinfo` is how long an outstanding `needinfo?` has
/// waited ([`needinfo_age`]), `None` when there is none. Pure, like
/// [`ping_decision`], and only ever called for a request that reached
/// [`PingDecision::AlreadyPinged`] -- which already establishes that
/// the asker has no access and the bug is not theirs, because
/// `ping_decision` returns those cases first.
///
/// A cleared needinfo deliberately does **not** escalate, even though
/// the policy would often allow it. The policy counts a reply of "I do
/// not want to do epel" followed by nothing as *no action*, so a
/// response is not in itself a reason to stop -- but telling that
/// apart from a real answer means reading prose, and getting it wrong
/// files a public ticket about a maintainer who did reply. So it is
/// surfaced for a person to read instead.
pub fn releng_decision(
    status: &str,
    days_since_needinfo: Option<i64>,
    ticket: Option<u64>,
) -> RelengDecision {
    if let Some(n) = ticket {
        return RelengDecision::Filed(n);
    }
    if status != "NEW" {
        return RelengDecision::Progressed(status.to_string());
    }
    match days_since_needinfo {
        None => RelengDecision::NoNeedinfo,
        Some(d) if d < RELENG_MIN_DAYS => RelengDecision::TooSoon(d),
        Some(d) => RelengDecision::File(d),
    }
}

/// How many days the oldest outstanding `needinfo?` on `requestee`
/// has waited, or `None` when there is none.
///
/// The flag is the clock, not a timestamp of our own: it is right even
/// when the ping was set by hand or by somebody else, it survives a
/// report that was regenerated, and Bugzilla clears it when the
/// requestee answers -- which is the cheapest available reading of
/// "did they respond". Only the flag on the maintainer counts; a
/// `needinfo?` pointed at anyone else is a different conversation.
///
/// `requestee` is a Bugzilla login, so `assigned_to` must be one too.
/// It is, for an authenticated read -- which `escalate` always is,
/// since pinging needs the key. Read anonymously the same bug returns
/// `assigned_to` as a real name ("Felix Wang" against a requestee of
/// `topazus`) and nothing would match.
pub fn needinfo_age(
    flags: &[sandogasa_bugzilla::models::Flag],
    requestee: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<i64> {
    flags
        .iter()
        .filter(|f| f.name == "needinfo" && f.status == "?")
        .filter(|f| {
            f.requestee
                .as_deref()
                .is_some_and(|r| r.eq_ignore_ascii_case(requestee))
        })
        .filter_map(|f| f.creation_date)
        .map(|set| (now - set).num_days())
        .max()
}

/// The open ticket among `hits` that is this exact request, if any.
///
/// On the title exactly, not on whatever the search returned: Forgejo
/// matches `q` as a substring, so a search for `Stalled EPEL package:
/// et` also finds `Stalled EPEL package: etcd`, and adopting that one
/// would record somebody else's request as this one's -- and then
/// never file the ticket this request needs. A *closed* ticket of the
/// same name is not a duplicate either: that request was dealt with,
/// and this is a new stall.
fn open_duplicate(hits: &[sandogasa_forgejo::Issue], title: &str) -> Option<u64> {
    hits.iter()
        .find(|i| i.title == title && i.state == "open")
        .map(|i| i.number)
}

/// Title and body of the releng ticket for a stalled request.
///
/// Follows `releng/tickets`'s own issue template rather than the prose
/// form in the EPEL docs: triage reads the template's headings, and a
/// ticket that skips them lands as free text. The checklist is ticked
/// because the caller files nothing until it has done each of those
/// things -- notably the duplicate search.
pub fn releng_ticket(
    pkg: &str,
    branch: &str,
    bug_url: &str,
    fas: &str,
    maintainers: &[String],
) -> (String, String) {
    let cc = match maintainers.is_empty() {
        // The policy requires naming the maintainer's FAS. When
        // dist-git lists no owner there is nobody to name, and saying
        // so is better than omitting the line silently.
        true => "cc: (no owner listed in dist-git)".to_string(),
        false => format!(
            "cc: {}",
            maintainers
                .iter()
                .map(|m| format!("@{m}"))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    };
    let body = format!(
        "## Describe the issue\n\n\
         {pkg} has a stalled EPEL branch request.\n\n\
         {bug_url}\n\n\
         Since this has met the time requirements as outlined in the stalled EPEL\n\
         request policy, please grant my FAS (@{fas}) commit access to the package.\n\n\
         {cc}\n\n\
         ## When do you need this? (YYYY/MM/DD)\n\n\
         ASAP\n\n\
         ## When is this no longer needed or useful? (YYYY/MM/DD)\n\n\
         N/A\n\n\
         ## If we cannot complete your request, what is the impact?\n\n\
         {pkg} stays unavailable in {branch}, along with anything that depends on\n\
         it.\n\n\
         ---\n\n\
         ## Checklist\n\n\
         - [x] I have checked existing issues for duplicates\n\
         - [x] I have filled out all the fields above\n\
         - [x] I have provided relevant Bugzilla or reference links (if applicable)\n"
    );
    (format!("Stalled EPEL package: {pkg}"), body)
}

// ---- single file-request ----

/// File one branch request. Tries `Fedora EPEL`/`<branch>` first
/// and falls back to `Fedora`/`rawhide` when the component isn't
/// in EPEL. Returns the new bug ID.
pub async fn file_one(
    bz: &BzClient,
    pkg: &str,
    branch: &str,
    fas: Option<&str>,
    sig: Option<&str>,
    blocks: &[u64],
    depends_on: &[u64],
) -> Result<u64, String> {
    file_one_of_kind(
        bz,
        RequestKind::Branch,
        pkg,
        branch,
        fas,
        sig,
        blocks,
        depends_on,
        None,
    )
    .await
}

/// [`file_one`] for a given kind of request.
#[allow(clippy::too_many_arguments)]
pub async fn file_one_of_kind(
    bz: &BzClient,
    kind: RequestKind,
    pkg: &str,
    branch: &str,
    fas: Option<&str>,
    sig: Option<&str>,
    blocks: &[u64],
    depends_on: &[u64],
    claim: Option<&str>,
) -> Result<u64, String> {
    let summary = kind.summary(pkg, branch);
    let description = request_description_for(kind, pkg, branch, fas, sig)?;

    let mut epel = serde_json::json!({
        "product": "Fedora EPEL",
        "version": branch,
        "component": pkg,
        "summary": summary,
        "description": description,
        "blocks": blocks,
        "depends_on": depends_on,
    });
    // Claimed as it is filed rather than reassigned afterwards: the
    // bug is the asker's own record of work they are doing, and an
    // update would mail everybody watching the package a second time
    // to say so.
    sandogasa_bugzilla::claim::apply_claim(&mut epel, claim);
    let resp = bz
        .create(&epel)
        .await
        .map_err(|e| format!("Bugzilla create for {pkg} (EPEL) failed: {e}"))?;
    if let Some(id) = resp.id {
        return Ok(id);
    }

    // Component not in EPEL → request the Fedora branch instead.
    let mut fedora = serde_json::json!({
        "product": "Fedora",
        "version": "rawhide",
        "component": pkg,
        "summary": summary,
        "description": description,
        "blocks": blocks,
        "depends_on": depends_on,
    });
    sandogasa_bugzilla::claim::apply_claim(&mut fedora, claim);
    let resp2 = bz
        .create(&fedora)
        .await
        .map_err(|e| format!("Bugzilla create for {pkg} (Fedora) failed: {e}"))?;
    if let Some(id) = resp2.id {
        return Ok(id);
    }

    Err(format!(
        "could not file request for {pkg}: EPEL: {}; Fedora: {}",
        resp.message.unwrap_or_else(|| "unknown error".into()),
        resp2.message.unwrap_or_else(|| "unknown error".into()),
    ))
}

/// Resolve a list of blocker/dependency tokens (numeric IDs or
/// Bugzilla aliases) to bug IDs.
pub async fn resolve_refs(bz: &BzClient, tokens: &[String]) -> Result<Vec<u64>, String> {
    let mut ids = Vec::new();
    for tok in tokens {
        if let Ok(id) = tok.parse::<u64>() {
            ids.push(id);
        } else {
            let bug = bz
                .bug_by_alias(tok)
                .await
                .map_err(|e| format!("could not resolve bug alias '{tok}': {e}"))?;
            ids.push(bug.id);
        }
    }
    Ok(ids)
}

// ---- pre-flight probes ----

/// Map batch `src_nvrs` output (`name-version-release` strings) back to
/// the queried package names: package → `version-release`. Longest
/// package names are matched first so a package that is a prefix of
/// another (`python-foo` vs `python-foo-bar`) can't steal its NVR.
fn match_nvrs_to_packages(nvrs: &[String], packages: &[String]) -> BTreeMap<String, String> {
    let mut by_len: Vec<&String> = packages.iter().collect();
    by_len.sort_by_key(|p| std::cmp::Reverse(p.len()));
    let mut out = BTreeMap::new();
    for nvr in nvrs {
        for pkg in &by_len {
            if let Some(vr) = nvr.strip_prefix(&format!("{pkg}-")) {
                out.entry((*pkg).clone()).or_insert_with(|| vr.to_string());
                break;
            }
        }
    }
    out
}

/// Query which of `packages` exist as source packages on the base
/// branch, returning package → `version-release`. This is the filing
/// pre-flight: it re-checks the base itself rather than trusting the
/// report, so stale or pre-guard reports can't slip a base-distro
/// package through (the rhbz#2482250 failure mode).
/// Which of `packages` a branch carries as source packages, with
/// their version-release.
/// What a branch has, asked of something that can answer.
///
/// The pre-flights are the part of filing most worth testing — they
/// are what stops a request being opened for a package the base distro
/// owns, for one already branched, or for one nobody has packaged —
/// and they are the part bound to an external tool. Behind this trait
/// a test answers from a table and a run answers from fedrq.
pub(crate) trait SourceProbe: Sync {
    /// The source packages among `packages` present on `branch`, each
    /// with its `version-release`. `repo` names a repo class to ask
    /// within, such as `@testing`.
    fn present(
        &self,
        branch: &str,
        repo: Option<&str>,
        packages: &[String],
    ) -> Result<BTreeMap<String, String>, String>;
}

/// The probe a real run uses.
pub(crate) struct FedrqProbe;

impl SourceProbe for FedrqProbe {
    fn present(
        &self,
        branch: &str,
        repo: Option<&str>,
        packages: &[String],
    ) -> Result<BTreeMap<String, String>, String> {
        src_probe_repo(branch, repo, packages)
    }
}

fn src_probe(
    probe: &dyn SourceProbe,
    branch: &str,
    packages: &[String],
) -> Result<BTreeMap<String, String>, String> {
    probe.present(branch, None, packages)
}

/// [`src_probe`] against one repo class of a branch.
fn src_probe_repo(
    branch: &str,
    repo: Option<&str>,
    packages: &[String],
) -> Result<BTreeMap<String, String>, String> {
    let fedrq = sandogasa_fedrq::Fedrq {
        branch: Some(branch.to_string()),
        repo: repo.map(str::to_string),
    };
    let nvrs = fedrq
        .src_nvrs(packages)
        .map_err(|e| format!("pre-flight ({branch}): {e}"))?;
    Ok(match_nvrs_to_packages(&nvrs, packages))
}

/// What the target branch already has, counting updates-testing.
///
/// A package whose branch request was acted on days ago sits in
/// testing, not yet stable — cxxopts was there while a second request
/// for it was being filed. A branch that has no testing repo is not an
/// error: the stable answer stands and the reason is said once.
fn target_probe(
    probe: &dyn SourceProbe,
    branch: &str,
    packages: &[String],
) -> Result<BTreeMap<String, String>, String> {
    let mut present = src_probe(probe, branch, packages)?;
    match probe.present(branch, Some("@testing"), packages) {
        Ok(testing) => {
            for (pkg, vr) in testing {
                present.entry(pkg).or_insert(vr);
            }
        }
        Err(e) => eprintln!(
            "warning: could not read {branch} updates-testing ({e}); a package \
             branched but not yet stable will look missing"
        ),
    }
    Ok(present)
}

/// Split the not-yet-filed packages into ones to file and ones to skip
/// (with the reason to print). Skips: overrides from the report (an
/// alternate package needs a new package review, not a branch request),
/// anything the base-distro pre-flight found in the base, and anything
/// the source pre-flight did not find in the source branch (nothing to
/// branch from: a package new to Fedora needs a package review first).
fn partition_filable(
    report: &ResolveReport,
    candidates: Vec<String>,
    base_present: &BTreeMap<String, String>,
    base_label: &str,
    source_present: &BTreeMap<String, String>,
    target_present: &BTreeMap<String, String>,
    target_label: &str,
) -> (Vec<String>, Vec<String>) {
    let mut to_file = Vec::new();
    let mut skipped = Vec::new();
    for pkg in candidates {
        if !source_present.contains_key(&pkg) {
            skipped.push(format!(
                "skipping {pkg}: not in {} — nothing to branch from; a package \
                 new to Fedora needs a package review first (see check-pkg-reviews)",
                report.source_branch
            ));
        } else if report.overrides.contains(&pkg) {
            skipped.push(format!(
                "skipping {pkg}: marked as a base-distro override — an \
                 alternate package needs a NEW package review (see \
                 check-pkg-reviews), not a branch request"
            ));
        } else if let Some(vr) = base_present.get(&pkg) {
            skipped.push(format!(
                "skipping {pkg}: present in base distro {base_label} \
                 ({vr}) — EPEL must not replace it (a branch request \
                 would be CANTFIX)"
            ));
        } else if let Some(vr) = target_present.get(&pkg) {
            skipped.push(format!(
                "skipping {pkg}: already branched to {target_label} \
                 ({vr}) — nothing to request; an older version there is \
                 an update, not a branch"
            ));
        } else {
            to_file.push(pkg);
        }
    }
    (to_file, skipped)
}

/// The packages still to file for: those with no request recorded and
/// none adopted in this run.
///
/// A dry run adopts nothing for real, so without the second condition
/// the preview offers to file for a package it has just said it would
/// adopt.
fn still_to_file(
    candidates: Vec<String>,
    report: &ResolveReport,
    adopted: &BTreeSet<String>,
) -> Vec<String> {
    candidates
        .into_iter()
        .filter(|p| !report.branch_requests.contains_key(p) && !adopted.contains(p))
        .collect()
}

/// Bring a report's recorded requests up to date, replacing any that
/// were closed as duplicates with the bug they point at.
///
/// One query for the lot, then a lookup only for the closed ones.
async fn refresh_recorded(bz: &BzClient, report: &mut ResolveReport) -> Result<bool, String> {
    let ids: Vec<u64> = report.branch_requests.values().map(|r| r.rhbz).collect();
    if ids.is_empty() {
        return Ok(false);
    }
    let superseded: BTreeMap<u64, sandogasa_bugzilla::models::Bug> = bz
        .bugs(&ids)
        .await
        .map_err(|e| format!("could not read the recorded requests: {e}"))?
        .into_iter()
        .filter(|b| b.status == "CLOSED" && b.dupe_of.is_some())
        .map(|b| (b.id, b))
        .collect();
    let mut changed = false;
    for (pkg, req) in report.branch_requests.iter_mut() {
        let Some(bug) = superseded.get(&req.rhbz) else {
            continue;
        };
        let live = follow_duplicate(bz, pkg, bug.clone()).await?;
        if live.id != req.rhbz {
            req.rhbz = live.id;
            req.pinged = false;
            changed = true;
        }
    }
    Ok(changed)
}

/// How many duplicate pointers to follow before giving up: a
/// duplicate can be closed as a duplicate of another, but a chain this
/// long is a loop or a mess either way.
const DUPE_HOPS: usize = 4;

/// Follow a request closed as a duplicate to the bug it points at.
///
/// A recorded request can be superseded while nobody is looking: today
/// rhbz#2543941 was closed as a duplicate of the older rhbz#2531397,
/// leaving a report naming a dead bug that `escalate` reads as CLOSED
/// and skips, so the live request goes unpinged.
///
/// Returns the bug to act on, which is the one passed in when it is not
/// a duplicate. The `pinged` flag belongs to the bug it was set on, so
/// a caller that follows a pointer clears it.
async fn follow_duplicate(
    bz: &BzClient,
    pkg: &str,
    bug: sandogasa_bugzilla::models::Bug,
) -> Result<sandogasa_bugzilla::models::Bug, String> {
    let mut bug = bug;
    let mut seen = vec![bug.id];
    for _ in 0..DUPE_HOPS {
        let Some(next) = bug.dupe_of.filter(|_| bug.status == "CLOSED") else {
            return Ok(bug);
        };
        if seen.contains(&next) {
            eprintln!("warning: {pkg}: rhbz#{next} is part of a duplicate loop; leaving it alone");
            return Ok(bug);
        }
        println!(
            "{pkg}: rhbz#{} was closed as a duplicate of rhbz#{next}",
            bug.id
        );
        seen.push(next);
        bug = bz
            .bug(next)
            .await
            .map_err(|e| format!("failed to fetch rhbz#{next} for {pkg}: {e}"))?;
    }
    eprintln!(
        "warning: {pkg}: followed {DUPE_HOPS} duplicate pointers and there are more; \
         stopping at rhbz#{}",
        bug.id
    );
    Ok(bug)
}

/// What to do about a package the asker could branch themselves.
///
/// Asking somebody else to do work you have rights to do is the thing
/// worth catching; what happens next is the operator's call, since a
/// bug is still wanted when it anchors the requests waiting on it.
///
/// Only access dist-git records for the person counts. A
/// provenpackager has nothing to do with this: that right is to
/// *build* any package, not to branch one, so it can never answer the
/// question here.
///
/// Known gap: a group listed on the project — `rust-sig` holds commit
/// on rust-tokio, for instance — grants its members access that this
/// does not see, because knowing it needs the asker's group
/// membership. Such a package is offered no branch and gets a request
/// instead, which is the safe direction.
fn offer_to_branch(pkg: &str, branch: &str, standing: &BranchStanding, dry_run: bool) -> Offer {
    let Some(access) = standing.access else {
        return Offer::only_file();
    };
    if !standing.may_branch() {
        return Offer::only_file();
    }
    let how = match standing.scope {
        Some(true) => format!("{access:?} access covering {branch}"),
        _ => format!("{access:?} access"),
    };
    println!("{pkg}: you have {how} in dist-git, so you can branch it for {branch} yourself:");
    println!("  fedpkg request-branch --repo {pkg} {branch}");
    if dry_run || !std::io::stdin().is_terminal() {
        println!("{}", unattended_record_note(dry_run));
        return Offer::only_file();
    }
    // Default no: it writes to Fedora's infrastructure, and an
    // absent-minded Enter should not file a releng ticket.
    let requested = sandogasa_cli::confirm(&format!("  run it for {pkg} now?"), false)
        .unwrap_or(false)
        && match request_branch(pkg, branch) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("warning: {e}");
                false
            }
        };
    let file = sandogasa_cli::confirm(
        &format!("  file a request for {pkg} anyway, as a record?"),
        true,
    )
    .unwrap_or(true);
    Offer { requested, file }
}

/// What came of offering to branch a package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Offer {
    /// The branch was asked for from this run.
    requested: bool,
    /// File a bug as well.
    file: bool,
}

impl Offer {
    /// The answer where no offer arises: file, as before.
    fn only_file() -> Self {
        Offer {
            requested: false,
            file: true,
        }
    }

    /// What to ask Bugzilla for, given what dist-git said and what the
    /// asker just did about it.
    fn kind(self, standing: Option<&BranchStanding>) -> RequestKind {
        match self.requested {
            true => RequestKind::Requested,
            false => request_kind(standing),
        }
    }
}

/// Ask releng for the branch, the way a packager would by hand.
///
/// `--repo` is what makes this usable from anywhere: without it
/// `fedpkg` wants to be run inside a dist-git checkout of the package,
/// and this runs over a whole closure of them. Output is inherited so
/// the ticket URL fedpkg prints reaches the person who asked for it.
fn request_branch(pkg: &str, branch: &str) -> Result<(), String> {
    // `fedpkg --version` is not a thing — it exits 2 with a usage
    // message — so probe with `help`, as koji is probed.
    sandogasa_cli::require_tools(&[("fedpkg", "sudo dnf install fedpkg", Some("help"))])?;
    let status = std::process::Command::new("fedpkg")
        .args(["request-branch", "--repo", pkg, branch])
        .status()
        .map_err(|e| format!("could not run fedpkg: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("fedpkg request-branch for {pkg} {status}"))
    }
}

/// What a run that cannot put the question says about the record.
///
/// A dry run has a terminal and simply is not asking yet, so it says
/// the question is coming rather than answering it on the asker's
/// behalf; a real run with nothing on stdin has nobody to ask and
/// keeps the request, which is what the command was invoked to do.
fn unattended_record_note(dry_run: bool) -> &'static str {
    if dry_run {
        "  would ask whether to run it, and whether to file a request anyway, \
         as a record and an anchor for what depends on it"
    } else {
        "  stdin is not a terminal, so filing a request anyway, as a record \
         and an anchor for what depends on it"
    }
}

/// Note, on a request for a build of an already-branched package,
/// that a provenpackager could do that build themselves. Printed
/// after the line naming the request it annotates, since it is a
/// footnote on that package and reads as orphaned above it.
///
/// Provenpackager is the right to build any package, not to branch
/// one, so it never answers [`offer_to_branch`]; and Fedora's
/// convention is that it is for emergencies, not for getting on with
/// things. Hence a line of prose and nothing else: no prompt, no
/// command to paste, and the word "emergency" in it.
fn provenpackager_note(pkg: &str, kind: RequestKind, provenpackager: bool) -> Option<String> {
    (provenpackager && kind == RequestKind::Build).then(|| {
        format!(
            "  note: you are a provenpackager, so in an emergency you could \
             build {pkg} yourself — the exception, not the way to get this done"
        )
    })
}

/// Whether to adopt an existing request as this package's: a prompt
/// when there is somebody to answer it, and yes otherwise.
///
/// Adopting is the productive answer — the alternative is a duplicate
/// bug on a public tracker — but it is somebody else's bug, and
/// `escalate` will ping it once it is old enough, counting from the
/// day it was filed rather than the day it was adopted. So the prompt
/// says that, and a run with nobody watching takes the default.
/// What adopting a request is for, given whether its package has
/// already landed in the target branch.
///
/// A request whose package is there wants closing, not chasing -- and
/// adopting it is how `close` gets to see it at all, so the offer
/// stands and only the reason changes. Without this the two cases read
/// identically, which is how rhbz#2482238 was offered for adoption
/// months after the build it asked for had shipped.
pub fn adoption_reason(pkg: &str, branch: &str, landed: Option<&str>) -> String {
    match landed {
        Some(vr) => format!("{pkg}-{vr} is already in {branch}; ebranch close will retire it"),
        None => "escalate may ping it once it is old enough".to_string(),
    }
}

fn adopt_request(pkg: &str, rhbz: u64, branch: &str, landed: Option<&str>) -> bool {
    println!("{pkg} already has an open branch request: rhbz#{rhbz}");
    let why = adoption_reason(pkg, branch, landed);
    if !std::io::stdin().is_terminal() {
        println!("  adopting it; {why}");
        return true;
    }
    println!("  {why}");
    sandogasa_cli::confirm(&format!("  adopt rhbz#{rhbz}?"), true).unwrap_or(true)
}

/// What dist-git says about branching a package: whether the branch is
/// already there, and whether the person asking could make it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct BranchStanding {
    /// The branch exists in dist-git, built or not. `et` was branched
    /// for epel10 months before anything was built there, and a
    /// request for it is a request for work already done.
    pub branched: bool,
    /// The access the asker holds, when they hold any: owner, admin or
    /// commit means they can branch it themselves rather than ask.
    pub access: Option<sandogasa_distgit::AccessLevel>,
    /// For collaborator access, whether its branch scope reaches the
    /// branch being asked for. `None` when the question does not
    /// arise, or when dist-git could not be asked.
    pub scope: Option<bool>,
    /// The package's owners, as FAS names. A releng ticket has to name
    /// the maintainer's FAS, and dist-git is where that lives --
    /// Bugzilla only ever has an email address.
    pub owners: Vec<String>,
}

impl BranchStanding {
    /// Whether the asker can act rather than ask.
    ///
    /// Collaborator counts, but only once its branch scope has been
    /// read and covers the branch in question. That is how EPEL access
    /// is usually granted — as a collaborator scoped to `epel*` — and
    /// the scope is not always set up to reach the branch being asked
    /// for. Anything unread or unreachable asks a person instead.
    pub fn may_branch(&self) -> bool {
        use sandogasa_distgit::AccessLevel::*;
        match self.access {
            Some(Owner | Admin | Commit) => true,
            Some(Collaborator) => self.scope == Some(true),
            _ => false,
        }
    }
}

/// Whether a collaborator's branch scope covers `branch`.
///
/// Pagure splits the scope on commas and matches each pattern against
/// the ref name with `fnmatch` (`is_repo_collaborator` in
/// `pagure/utils.py`), so `epel*` covers `epel10` and `epel9` covers
/// nothing else. Only `*` is honoured here; a pattern using fnmatch's
/// `?` or `[seq]` reads as not covering, which asks a person rather
/// than assuming the right to act.
fn scope_covers(patterns: &str, branch: &str) -> bool {
    patterns
        .split(',')
        .any(|p| crate::check_crate::glob_match(p.trim(), branch))
}

/// The branch scope of `user`'s collaborator access, as dist-git
/// publishes it: `Some(true)` when it reaches `branch`.
///
/// This is a second request, made only for a collaborator, because the
/// project endpoint names collaborators without saying what they may
/// touch — only `/contributors` carries the pattern.
async fn collaborator_scope(
    client: &sandogasa_distgit::DistGitClient,
    package: &str,
    user: &str,
    branch: &str,
) -> Option<bool> {
    match client.get_contributors(package).await {
        Ok(c) => c
            .users
            .collaborators
            .iter()
            .find(|c| c.name() == user)
            .map(|c| c.branches().is_none_or(|p| scope_covers(p, branch))),
        Err(e) => {
            eprintln!("warning: could not read {package}'s collaborator scope ({e})");
            None
        }
    }
}

/// What to ask for, given what dist-git says: a package already
/// branched needs building, not branching.
fn request_kind(standing: Option<&BranchStanding>) -> RequestKind {
    match standing {
        Some(s) if s.branched => RequestKind::Build,
        _ => RequestKind::Branch,
    }
}

/// Who a request should be assigned to as it is filed: the asker,
/// when they could have branched the package themselves and chose to
/// file anyway. Such a bug is their own record of work they are
/// doing, and leaving it on the maintainer asks them for something
/// nobody is waiting on. Everything else stays unassigned, which is
/// how Bugzilla says "the component's maintainer".
fn claim_for<'a>(standing: Option<&BranchStanding>, opts: &'a Options) -> Option<&'a str> {
    standing
        .filter(|s| s.may_branch())
        .and(opts.email.as_deref())
}

/// Ask dist-git where a package stands for a branch.
///
/// Failure is not fatal and not silent: without this the run behaves
/// as it did before, filing a request and possibly duplicating work.
pub(crate) async fn branch_standing(
    client: &sandogasa_distgit::DistGitClient,
    package: &str,
    branch: &str,
    fas: Option<&str>,
) -> BranchStanding {
    let branched = match client.project_branches(package).await {
        Ok(Some(branches)) => branches.iter().any(|b| b == branch),
        Ok(None) => false,
        Err(e) => {
            eprintln!("warning: could not read {package}'s branches from dist-git ({e})");
            false
        }
    };
    // One read answers both questions: what the asker may do, and who
    // owns the package -- the FAS a releng ticket has to name.
    let acls = match fas {
        Some(_) => match client.get_acls(package).await {
            Ok(acls) => Some(acls),
            Err(e) => {
                eprintln!("warning: could not read {package}'s access list ({e})");
                None
            }
        },
        None => None,
    };
    let access = fas.and_then(|user| acls.as_ref().and_then(|a| a.user_level(user)));
    let owners = acls.map(|a| a.access_users.owner).unwrap_or_default();
    let scope = match (access, fas) {
        (Some(sandogasa_distgit::AccessLevel::Collaborator), Some(user)) => {
            collaborator_scope(client, package, user, branch).await
        }
        _ => None,
    };
    BranchStanding {
        branched,
        access,
        scope,
        owners,
    }
}

/// The branch requests already open for `packages`, whoever filed
/// them, as package to bug id.
///
/// A request is matched by its summary rather than by who filed it, so
/// one opened by hand counts: the wording is the one this module
/// writes and the EPEL documentation suggests. The release is compared
/// by family, since a request says `epel10` where a report may target
/// `epel10.4`, and a bug is kept only when the package it names is the
/// component it was filed against — a request mentioned in another
/// package's bug is not that package's request.
pub(crate) fn adoptable(
    bugs: &[sandogasa_bugzilla::models::Bug],
    branch: &str,
) -> BTreeMap<String, u64> {
    let family =
        |b: &str| sandogasa_bugclass::bugzilla::product_version_for_branch(b).map(|(_, v)| v);
    let wanted = family(branch);
    let mut found: BTreeMap<String, u64> = BTreeMap::new();
    for bug in bugs {
        let Some((pkg, bug_branch)) =
            sandogasa_bugclass::bugzilla::branch_request_parts(&bug.summary)
        else {
            continue;
        };
        let Some(component) = bug
            .component
            .iter()
            .find(|c| sandogasa_bugclass::bugzilla::branch_request_names(&pkg, c))
        else {
            continue;
        };
        if family(&bug_branch) != wanted {
            continue;
        }
        // The component is what gets branched, whatever subpackage the
        // request happened to name.
        let pkg = component.clone();
        // The oldest one is the request; a later duplicate is not.
        found
            .entry(pkg)
            .and_modify(|id| *id = (*id).min(bug.id))
            .or_insert(bug.id);
    }
    found
}

/// Ask Bugzilla for the open branch requests among `packages`.
async fn search_existing(
    bz: &BzClient,
    packages: &[String],
    branch: &str,
) -> BTreeMap<String, u64> {
    if packages.is_empty() {
        return BTreeMap::new();
    }
    let mut query: Vec<String> = packages
        .iter()
        .map(|p| format!("component={}", crate::discover::urlencode(p)))
        .collect();
    query.push("bug_status=__open__".to_string());
    match bz.search(&query.join("&"), 0).await {
        Ok(bugs) => adoptable(&bugs, branch),
        Err(e) => {
            eprintln!(
                "warning: could not search Bugzilla for existing branch requests ({e}); \
                 a request filed outside this tool would be duplicated"
            );
            BTreeMap::new()
        }
    }
}

// ---- batch file-requests with linking ----

/// File requests for every package in `report` that doesn't
/// already have one, recording each in `report.branch_requests`,
/// then link them: a package's request `depends_on` the requests
/// for the packages it needs (following `report.edges`). Each new
/// request blocks the bugs/aliases in `blocked` (from `--blocked`);
/// nothing is blocked implicitly.
pub async fn file_batch(
    report: &mut ResolveReport,
    blocked: &[String],
    opts: &Options,
    probe: &dyn SourceProbe,
) -> Result<bool, String> {
    let bz = BzClient::new(&opts.bugzilla_url)
        .with_api_key(opts.api_key.clone())
        .map_err(|e| e.to_string())?;

    if report.packages.is_empty() {
        println!("No packages in report to file requests for.");
        return Ok(false);
    }

    // Packages without a recorded request yet.
    let candidates: Vec<String> = report
        .packages
        .iter()
        .filter(|p| !report.branch_requests.contains_key(*p))
        .cloned()
        .collect();

    // A recorded request can have been closed as a duplicate since the
    // report was written, which would leave linking and escalation
    // pointing at a dead bug.
    let mut changed = refresh_recorded(&bz, report).await?;

    // Target pre-flight, run before the adoption below rather than
    // after it: `still_to_file` drops an adopted package from the
    // candidates, so a request that had already been filed used to
    // skip this check entirely and be offered for adoption with no
    // hint that its package had shipped. It serves both purposes --
    // a package already branched needs no request, however old the
    // version there is. It reaches that point when the closure wanted
    // a capability that branch's older build does not provide -- a
    // soname from the source branch's build, say.
    let target_present = target_probe(probe, &opts.branch, &candidates)?;

    // A request filed by hand, from another report, or by another
    // maintainer is invisible to the report's own record. Adopt it
    // rather than opening a second bug for the same package.
    let mut adopted: BTreeSet<String> = BTreeSet::new();
    for (pkg, rhbz) in search_existing(&bz, &candidates, &opts.branch).await {
        let landed = target_present.get(&pkg).map(String::as_str);
        if opts.dry_run {
            println!(
                "would adopt existing request for {pkg}: rhbz#{rhbz} ({})",
                adoption_reason(&pkg, &opts.branch, landed)
            );
            adopted.insert(pkg);
            continue;
        }
        if !adopt_request(&pkg, rhbz, &opts.branch, landed) {
            continue;
        }
        report.branch_requests.insert(
            pkg.clone(),
            BranchRequest {
                rhbz,
                pinged: false,
                releng: None,
            },
        );
        adopted.insert(pkg);
        changed = true;
    }
    let candidates = still_to_file(candidates, report, &adopted);

    // Source pre-flight: a check-crate report lists crates nobody has
    // packaged yet beside ones that only want branching, and a resolve
    // report may be stale.
    let source_present = src_probe(probe, &report.source_branch, &candidates)?;
    // Base-distro pre-flight (defense in depth against stale reports).
    let base_present = match &opts.base_branch {
        Some(base) => src_probe(probe, base, &candidates)?,
        None => {
            eprintln!(
                "warning: no base-distro mapping for {}; base-distro \
                 pre-flight disabled (pass --base-branch to enable)",
                opts.branch
            );
            BTreeMap::new()
        }
    };
    let base_label = opts.base_branch.as_deref().unwrap_or("base");
    // What dist-git says: a branch that exists needs no request, and an
    // asker with access can make one rather than ask for it.
    let distgit = sandogasa_distgit::DistGitClient::new();
    let mut standing: BTreeMap<String, BranchStanding> = BTreeMap::new();
    for pkg in &candidates {
        standing.insert(
            pkg.clone(),
            branch_standing(&distgit, pkg, &opts.branch, opts.identity.as_deref()).await,
        );
    }
    let (to_file, skipped) = partition_filable(
        report,
        candidates,
        &base_present,
        base_label,
        &source_present,
        &target_present,
        &opts.branch,
    );
    for msg in &skipped {
        eprintln!("{msg}");
    }
    if !report.blocked_by_base.is_empty() {
        eprintln!(
            "note: {} package(s) blocked by the base distro in the \
             resolve report — see its blocked_by_base section",
            report.blocked_by_base.len()
        );
    }

    if opts.dry_run {
        for pkg in &to_file {
            let kind = request_kind(standing.get(pkg));
            if let Some(stands) = standing.get(pkg) {
                offer_to_branch(pkg, &opts.branch, stands, true);
            }
            println!(
                "would file {} request for {pkg} in {}{}{}",
                kind.asked_for(),
                opts.branch,
                // Where the offer above was made, the filing is the
                // asker's to decide, not something to state.
                match standing.get(pkg).is_some_and(|s| s.may_branch()) {
                    true => " if you say yes",
                    false => "",
                },
                match claim_for(standing.get(pkg), opts) {
                    Some(email) => format!(", assigned to {email}"),
                    None => String::new(),
                }
            );
            if let Some(note) = provenpackager_note(pkg, kind, opts.provenpackager) {
                println!("{note}");
            }
        }
        // Links among already-recorded requests (a re-run);
        // newly-filed ones don't have IDs to preview yet.
        preview_links(&report.edges, &report.branch_requests);
        return Ok(false);
    }

    let blocks = resolve_refs(&bz, blocked).await?;

    for pkg in &to_file {
        let offer = match standing.get(pkg) {
            Some(stands) => offer_to_branch(pkg, &opts.branch, stands, opts.dry_run),
            None => Offer::only_file(),
        };
        if !offer.file {
            continue;
        }
        let kind = offer.kind(standing.get(pkg));
        let rhbz = file_one_of_kind(
            &bz,
            kind,
            pkg,
            &opts.branch,
            opts.fas.as_deref(),
            opts.sig.as_deref(),
            &blocks,
            &[],
            claim_for(standing.get(pkg), opts),
        )
        .await?;
        println!("filed {pkg}: rhbz#{rhbz} ({})", kind.asked_for());
        if let Some(note) = provenpackager_note(pkg, kind, opts.provenpackager) {
            println!("{note}");
        }
        report.branch_requests.insert(
            pkg.clone(),
            BranchRequest {
                rhbz,
                pinged: false,
                releng: None,
            },
        );
        changed = true;
    }

    // Link: each package's request depends on its dependencies'
    // requests. Done after filing so every rhbz exists.
    link_requests(&bz, &report.edges, &report.branch_requests, opts.verbose).await;

    Ok(changed)
}

/// Add `depends_on` links between filed requests following the
/// package dependency edges. Only links packages that both have
/// a recorded request. Existing links aren't removed.
///
/// A bug that cannot be linked is reported and the rest are drawn
/// anyway. Bugzilla is unreliable enough under a batch of writes that
/// one refusal is not a reason to abandon the other thirty edges, and
/// the bug IDs just filed are only written back to the report when
/// this returns — stopping here would lose them. Linking is additive
/// and idempotent, so the fix is to run `file-requests` again.
async fn link_requests(
    bz: &BzClient,
    pkg_edges: &BTreeMap<String, BTreeSet<String>>,
    requests: &BTreeMap<String, BranchRequest>,
    verbose: bool,
) {
    let mut failed: Vec<u64> = Vec::new();
    for (pkg, deps) in pkg_edges {
        let Some(req) = requests.get(pkg) else {
            continue;
        };
        let dep_ids: Vec<u64> = deps
            .iter()
            .filter_map(|d| requests.get(d).map(|r| r.rhbz))
            .collect();
        if dep_ids.is_empty() {
            continue;
        }
        if verbose {
            eprintln!(
                "[file-requests] linking rhbz#{} depends_on {:?}",
                req.rhbz, dep_ids
            );
        }
        // Additive update so we never clobber unrelated links; a failed
        // request is read back before it counts as failed. Minor, so
        // drawing a graph edge does not mail everyone watching the
        // package: a 31-package batch is otherwise 31 notifications
        // about bookkeeping (issue #30).
        let body = serde_json::json!({
            "depends_on": { "add": dep_ids },
            "minor_update": true,
        });
        let out = bz.update_verified(req.rhbz, &body, 3).await;
        if let Some(note) = out.note() {
            eprintln!("note: rhbz#{}: {note}", req.rhbz);
        }
        if !out.complete() {
            eprintln!(
                "warning: could not link rhbz#{} ({pkg}): {}",
                req.rhbz,
                out.last_error.unwrap_or_default()
            );
            failed.push(req.rhbz);
        }
    }
    if !failed.is_empty() {
        eprintln!(
            "warning: {} request(s) left unlinked: {failed:?} — every bug was              filed and recorded; re-run file-requests to draw the rest",
            failed.len()
        );
    }
}

fn preview_links(
    pkg_edges: &BTreeMap<String, BTreeSet<String>>,
    requests: &BTreeMap<String, BranchRequest>,
) {
    for (pkg, deps) in pkg_edges {
        let Some(req) = requests.get(pkg) else {
            continue;
        };
        let dep_ids: Vec<u64> = deps
            .iter()
            .filter_map(|d| requests.get(d).map(|r| r.rhbz))
            .collect();
        if !dep_ids.is_empty() {
            println!("would link rhbz#{} depends_on {dep_ids:?}", req.rhbz);
        }
    }
}

// ---- closing a fulfilled request ----

/// What to do about a request that was filed before the work landed.
#[derive(Debug, PartialEq, Eq)]
pub enum CloseDecision {
    /// The package is in the branch at this `version-release`, so the
    /// request has been fulfilled and the bug can be closed.
    Landed(String),
    /// Nothing in the branch yet: the request still stands.
    Pending,
    /// Already closed; nothing to do.
    Closed,
}

/// Decide whether a filed request has been overtaken by the work.
///
/// `present` is the package's `version-release` in the target branch
/// when it is there ([`target_probe`]), `None` when it is not. Pure,
/// like [`ping_decision`]: the probe belongs to the caller.
///
/// Only a build counts. A branch with nothing built from it does not
/// fulfil a request -- the bug asked for the package to be *in* the
/// branch, and an empty branch is the halfway state `RequestKind::Build`
/// exists to describe.
pub fn close_decision(status: &str, present: Option<&str>) -> CloseDecision {
    if status == "CLOSED" {
        return CloseDecision::Closed;
    }
    match present {
        Some(vr) => CloseDecision::Landed(vr.to_string()),
        None => CloseDecision::Pending,
    }
}

/// The ids of the `needinfo?` flags still outstanding on a bug.
///
/// Closing answers the question for everyone, and Red Hat's Bugzilla
/// does not clear them on close by itself -- rhbz#2482238 was closed
/// with one still pointed at `epel-packagers-sig`, where it would have
/// sat in their request queue indefinitely.
pub fn outstanding_needinfo(flags: &[sandogasa_bugzilla::models::Flag]) -> Vec<u64> {
    flags
        .iter()
        .filter(|f| f.name == "needinfo" && f.status == "?")
        .map(|f| f.id)
        .filter(|id| *id != 0)
        .collect()
}

/// A Bugzilla `flags` update that clears each of `ids`.
fn clear_flags(ids: &[u64]) -> serde_json::Value {
    ids.iter()
        .map(|id| serde_json::json!({ "id": id, "status": "X" }))
        .collect()
}

/// The comment a close leaves behind, naming what was found.
///
/// The evidence goes in the bug rather than a bare "fixed": the next
/// reader should be able to check the claim without rerunning
/// anything, and a request closed months after it was satisfied is
/// exactly the case where "how do you know" is worth answering.
pub fn close_body(pkg: &str, branch: &str, vr: &str) -> String {
    format!(
        "{pkg}-{vr} is in {branch}, so this request has been fulfilled.\n\n\
         Closing; please reopen if the build is missing for a branch\n\
         you still need it in.\n"
    )
}

/// Close every request in `report` whose package has landed in the
/// target branch. Returns whether the report changed.
///
/// One probe for the whole report, not one per request: the presence
/// question is answered in bulk by the same call `file_batch` makes
/// before filing, and asking it per package would turn a report into
/// as many repository queries.
pub(crate) async fn close_batch(
    report: &mut ResolveReport,
    opts: &Options,
    claim: bool,
    probe: &dyn SourceProbe,
) -> Result<bool, String> {
    let bz = BzClient::new(&opts.bugzilla_url)
        .with_api_key(opts.api_key.clone())
        .map_err(|e| e.to_string())?;
    let packages: Vec<String> = report.branch_requests.keys().cloned().collect();
    let present = target_probe(probe, &opts.branch, &packages)?;

    // Decide everything first, so the claim question is asked once
    // about a known set rather than per bug as they go by.
    let mut landed: Vec<(String, u64, String, Vec<u64>)> = Vec::new();
    // Already-closed requests whose `needinfo?` was never cleared.
    let mut dangling: Vec<(String, u64, Vec<u64>)> = Vec::new();
    for (pkg, req) in report.branch_requests.iter() {
        let bug = bz
            .bug(req.rhbz)
            .await
            .map_err(|e| format!("failed to fetch rhbz#{} for {pkg}: {e}", req.rhbz))?;
        match close_decision(&bug.status, present.get(pkg).map(String::as_str)) {
            CloseDecision::Landed(vr) => {
                landed.push((pkg.clone(), req.rhbz, vr, outstanding_needinfo(&bug.flags)))
            }
            CloseDecision::Pending => {
                if opts.verbose {
                    eprintln!("[close] {pkg} rhbz#{} not in {} yet", req.rhbz, opts.branch);
                }
            }
            CloseDecision::Closed => {
                // A closed request can still carry the `needinfo?` our
                // own ping set, and nothing else will ever clear it.
                // Tidying it here costs one write and saves the
                // requestee a dead entry in their queue forever.
                let stale = outstanding_needinfo(&bug.flags);
                match stale.is_empty() {
                    true => {
                        if opts.verbose {
                            eprintln!("[close] {pkg} rhbz#{} already closed", req.rhbz);
                        }
                    }
                    false => dangling.push((pkg.clone(), req.rhbz, stale)),
                }
            }
        }
    }
    for (pkg, rhbz, stale) in &dangling {
        println!(
            "{} stale needinfo on {pkg} rhbz#{rhbz} (already closed)",
            if opts.dry_run {
                "would clear"
            } else {
                "clearing"
            }
        );
        if opts.dry_run {
            continue;
        }
        let update = serde_json::json!({ "flags": clear_flags(stale) });
        let out = bz.update_verified(*rhbz, &update, 3).await;
        if !out.complete() {
            eprintln!(
                "warning: could not clear needinfo on rhbz#{rhbz}: {}",
                out.last_error.unwrap_or_default()
            );
        }
    }
    if landed.is_empty() {
        if dangling.is_empty() {
            println!(
                "nothing to close: no request's package is in {} yet",
                opts.branch
            );
        }
        return Ok(false);
    }
    for (pkg, rhbz, vr, _) in &landed {
        println!(
            "{} {pkg} rhbz#{rhbz} — {pkg}-{vr} is in {}",
            if opts.dry_run {
                "would close"
            } else {
                "closing"
            },
            opts.branch
        );
    }
    if opts.dry_run {
        return Ok(false);
    }
    // Project rule: a tool that closes bugs offers to reassign them.
    let assign = sandogasa_bugzilla::claim::resolve_claim(
        claim,
        opts.assume_yes,
        opts.email.as_deref(),
        &sandogasa_bugzilla::claim::close_claim_prompt(
            landed.len(),
            opts.email.as_deref().unwrap_or_default(),
        ),
        |prompt| sandogasa_cli::confirm(prompt, false).map_err(|e| e.to_string()),
    )?;

    let mut changed = false;
    for (pkg, rhbz, vr, needinfo) in &landed {
        let mut update = serde_json::json!({
            "status": "CLOSED",
            "resolution": "CURRENTRELEASE",
            "comment": { "body": close_body(pkg, &opts.branch, vr) },
        });
        if !needinfo.is_empty() {
            // Cleared in the same write as the close: a separate one
            // could leave the flag behind if it failed, which is the
            // state being fixed.
            update["flags"] = clear_flags(needinfo);
        }
        sandogasa_bugzilla::claim::apply_claim(&mut update, assign.as_deref());
        let out = bz.update_verified(*rhbz, &update, 3).await;
        if let Some(note) = out.note() {
            eprintln!("note: rhbz#{rhbz}: {note}");
        }
        if !out.complete() {
            return Err(format!(
                "failed to close rhbz#{rhbz}: {}",
                out.last_error.unwrap_or_default()
            ));
        }
        println!("closed {pkg} rhbz#{rhbz}");
        changed = true;
    }
    Ok(changed)
}

/// `close` — retire the requests a report records that have landed.
pub fn run_close(report_path: &str, opts: &Options, claim: bool) -> Result<(), String> {
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| format!("failed to create async runtime: {e}"))?;
    rt.block_on(async {
        let mut file = ReportFile::load(report_path)?;
        let mut report = file.view();
        let changed = close_batch(&mut report, opts, claim, &FedrqProbe).await?;
        if changed && !opts.dry_run {
            file.save(&report, report_path)?;
        }
        Ok(())
    })
}

// ---- escalation ----

/// Escalate stale requests recorded in `report`. Pings each NEW
/// request older than [`PING_MIN_DAYS`] that hasn't been pinged,
/// marking it pinged. Returns whether the report changed.
pub async fn escalate(report: &mut ResolveReport, opts: &Options) -> Result<bool, String> {
    // A ping is a request for someone else's work, so ask dist-git
    // what is left for them before making one. The answer can change
    // after a bug is filed -- a branch gets made, access gets granted
    // -- and nothing else here re-reads it.
    //
    // Gathered here rather than inside the loop so the decision flow
    // below can be tested without a dist-git client, the way the rest
    // of this tool keeps an external query out of the flow around it.
    let distgit = sandogasa_distgit::DistGitClient::new();
    let mut standings: BTreeMap<String, BranchStanding> = BTreeMap::new();
    for pkg in report.branch_requests.keys() {
        standings.insert(
            pkg.clone(),
            branch_standing(&distgit, pkg, &opts.branch, opts.identity.as_deref()).await,
        );
    }
    // Whether anything was *built* there, which `branched` does not
    // answer: a branch can sit empty for months. Only used to word the
    // skip, so a probe failure is not fatal -- it degrades to the
    // weaker "branched" claim.
    let packages: Vec<String> = report.branch_requests.keys().cloned().collect();
    let present = target_probe(&FedrqProbe, &opts.branch, &packages).unwrap_or_else(|e| {
        eprintln!(
            "warning: could not read {} ({e}); reporting branches, not builds",
            opts.branch
        );
        BTreeMap::new()
    });
    escalate_with(report, opts, &standings, &present).await
}

/// File the releng ticket for a request whose ping went unanswered.
///
/// Returns the ticket number when one was filed, `None` when the run
/// could only report that the step is due -- no token, no FAS, or an
/// answer of no at the prompt. Reporting rather than failing is the
/// point: the rest of the run is still worth having.
///
/// The duplicate search is not optional. The tracker's checklist
/// claims it was done, so it is done before the body that claims it is
/// written, and a search that fails stops this package rather than
/// filing a ticket that asserts something untrue.
async fn file_releng(
    rhbz: u64,
    pkg: &str,
    waited: i64,
    standing: &BranchStanding,
    opts: &Options,
) -> Result<Option<u64>, String> {
    let due = format!("{pkg} rhbz#{rhbz}: needinfo unanswered for {waited} days");
    let Some(fas) = opts.identity.clone().filter(|f| !f.is_empty()) else {
        println!("{due} — set packager.fas to file the releng ticket");
        return Ok(None);
    };
    if opts.forge_token.is_empty() {
        println!("{due} — set [forgejo] token to file the releng ticket");
        return Ok(None);
    }
    let (title, body) = releng_ticket(
        pkg,
        &opts.branch,
        &format!("{}/show_bug.cgi?id={rhbz}", opts.bugzilla_url),
        &fas,
        &standing.owners,
    );
    if opts.dry_run {
        println!("would file releng ticket: {title}");
        return Ok(None);
    }
    let (url, token, title2) = (
        opts.forge_url.clone(),
        opts.forge_token.clone(),
        title.clone(),
    );
    let (owner, repo) = RELENG_REPO;
    // reqwest's blocking client off the async worker, the way
    // sandogasa-hattrack drives the same client.
    let open_dupe = tokio::task::spawn_blocking(move || -> Result<Option<u64>, String> {
        let c = sandogasa_forgejo::Client::new(&url, &token).map_err(|e| e.to_string())?;
        let hits = c
            .search_repo_issues(owner, repo, &title2, "all")
            .map_err(|e| e.to_string())?;
        Ok(open_duplicate(&hits, &title2))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("{pkg}: could not search {owner}/{repo} for duplicates: {e}"))?;
    if let Some(n) = open_dupe {
        println!("{due} — {owner}/{repo}#{n} is already open");
        return Ok(Some(n));
    }
    if !opts.assume_yes {
        if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            println!("{due} — rerun with --yes to file the releng ticket");
            return Ok(None);
        }
        println!("\n{due}.\n{title}\n\n{body}");
        if !sandogasa_cli::confirm(&format!("File this on {owner}/{repo}?"), true)
            .map_err(|e| e.to_string())?
        {
            return Ok(None);
        }
    }
    let (url, token) = (opts.forge_url.clone(), opts.forge_token.clone());
    let issue = tokio::task::spawn_blocking(move || -> Result<sandogasa_forgejo::Issue, String> {
        let c = sandogasa_forgejo::Client::new(&url, &token).map_err(|e| e.to_string())?;
        let labels = c
            .label_ids(owner, repo, &RELENG_LABELS)
            .map_err(|e| e.to_string())?;
        c.create_issue(owner, repo, &title, &body, &labels)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("{pkg}: could not file on {owner}/{repo}: {e}"))?;
    println!("filed {owner}/{repo}#{} for {pkg}", issue.number);
    Ok(Some(issue.number))
}

/// [`escalate`] with the dist-git answers already in hand.
pub(crate) async fn escalate_with(
    report: &mut ResolveReport,
    opts: &Options,
    standings: &BTreeMap<String, BranchStanding>,
    present: &BTreeMap<String, String>,
) -> Result<bool, String> {
    let bz = BzClient::new(&opts.bugzilla_url)
        .with_api_key(opts.api_key.clone())
        .map_err(|e| e.to_string())?;
    let now = chrono::Utc::now();
    let mut changed = false;
    // Requests no maintainer is holding up. Collected rather than
    // printed in place: scattered through the run they read as noise,
    // and the whole point of the skip is that these are the ones to
    // go and do something about -- once their dependencies are in.
    let mut unblocked_by_people: Vec<Skipped> = Vec::new();

    for (pkg, req) in report.branch_requests.iter_mut() {
        let bug = bz
            .bug(req.rhbz)
            .await
            .map_err(|e| format!("failed to fetch rhbz#{} for {pkg}: {e}", req.rhbz))?;
        let bug = follow_duplicate(&bz, pkg, bug).await?;
        if bug.id != req.rhbz {
            // The request moved, so what we know about the old bug does
            // not carry over: a ping on it was not a ping on this one.
            req.rhbz = bug.id;
            req.pinged = false;
            changed = true;
        }
        let days = (now - bug.creation_time).num_days();
        let standing = standings.get(pkg).cloned().unwrap_or_default();
        // Against the configured Bugzilla identity, not the bug's
        // creator: `escalate` chases requests other people filed and
        // this one adopted (rhbz#2368920 was Kristian's), so the
        // reporter is often somebody else entirely. No configured
        // email means no claim to compare, so the question does not
        // arise and the bug's own state decides.
        let assigned_to_asker = opts
            .email
            .as_deref()
            .is_some_and(|me| !me.is_empty() && bug.assigned_to.eq_ignore_ascii_case(me));
        // Bugzilla's own flag, not just the report's bool: a request
        // adopted into a fresh report has `pinged = false` however
        // many times it was pinged, and pinging again would put a
        // second `needinfo?` on the same person. The flag is the
        // truth; the bool is a local cache of it.
        let waited = needinfo_age(&bug.flags, &bug.assigned_to, now);
        if waited.is_some() && !req.pinged {
            req.pinged = true;
            changed = true;
        }
        match ping_decision(
            &bug.status,
            days,
            req.pinged,
            standing.may_branch(),
            standing.branched,
            assigned_to_asker,
        ) {
            PingDecision::Yours => {
                unblocked_by_people.push(Skipped {
                    pkg: pkg.clone(),
                    rhbz: req.rhbz,
                    why: "assigned to you".to_string(),
                    depends_on: bug.depends_on.clone(),
                });
            }
            PingDecision::NothingOwed { branched } => {
                // Say which, because the follow-up differs, and a
                // branch is not a build: saying "build it" about a
                // package that shipped months ago is how rhbz#2482238
                // stayed open long enough to be pinged for.
                let why = match (present.get(pkg), branched) {
                    (Some(vr), _) => {
                        format!("{pkg}-{vr} is in {} — ebranch close", opts.branch)
                    }
                    (None, true) => {
                        "branched, nothing built — build it, then close the bug".to_string()
                    }
                    (None, false) => format!("you can branch {} yourself", opts.branch),
                };
                unblocked_by_people.push(Skipped {
                    pkg: pkg.clone(),
                    rhbz: req.rhbz,
                    why,
                    depends_on: bug.depends_on.clone(),
                });
            }
            PingDecision::AlreadyPinged => {
                // The ping is step 3 of the EPEL stalled-request
                // policy; step 5 is a releng ticket once it has gone
                // unanswered for two weeks. Reaching this arm already
                // establishes the rest of the policy's premise --
                // `ping_decision` returns `Yours` and `NothingOwed`
                // before it, so the asker has no access and the bug is
                // not theirs.
                match releng_decision(&bug.status, waited, req.releng) {
                    RelengDecision::File(d) => {
                        let standing = standings.get(pkg).cloned().unwrap_or_default();
                        if let Some(n) = file_releng(req.rhbz, pkg, d, &standing, opts).await? {
                            req.releng = Some(n);
                            changed = true;
                        }
                    }
                    RelengDecision::Filed(n) => {
                        if opts.verbose {
                            eprintln!("[escalate] {pkg} rhbz#{}: releng ticket #{n}", req.rhbz);
                        }
                    }
                    RelengDecision::TooSoon(d) => {
                        if opts.verbose {
                            eprintln!(
                                "[escalate] {pkg} rhbz#{}: pinged {d} day(s) ago, \
                                 waiting until {RELENG_MIN_DAYS}",
                                req.rhbz
                            );
                        }
                    }
                    RelengDecision::NoNeedinfo => {
                        // Deliberately not escalated on a rule: see
                        // `releng_decision`.
                        println!(
                            "{pkg} rhbz#{}: the needinfo is gone — read the bug and \
                             decide whether it has stalled",
                            req.rhbz
                        );
                    }
                    RelengDecision::Progressed(status) => {
                        if opts.verbose {
                            eprintln!("[escalate] {pkg} rhbz#{} moved to {status}", req.rhbz);
                        }
                    }
                }
            }
            PingDecision::Closed => {
                if opts.verbose {
                    eprintln!("[escalate] {pkg} rhbz#{} closed, skipping", req.rhbz);
                }
            }
            PingDecision::NotNew(status) => {
                println!("{pkg} rhbz#{} is {status}, please check", req.rhbz);
            }
            PingDecision::TooNew(d) => {
                println!(
                    "{pkg} rhbz#{} created {d} day(s) ago, waiting until {PING_MIN_DAYS}",
                    req.rhbz
                );
            }
            PingDecision::Ping => {
                let body = ping_body(pkg, &opts.branch, opts.fas.as_deref(), opts.sig.as_deref());
                if opts.dry_run {
                    println!("would ping {pkg} rhbz#{} ({days} days old)", req.rhbz);
                    continue;
                }
                let update = serde_json::json!({
                    "comment": { "body": body },
                    "flags": [{
                        "name": "needinfo",
                        "status": "?",
                        "requestee": bug.assigned_to,
                    }],
                });
                let out = bz.update_verified(req.rhbz, &update, 3).await;
                if let Some(note) = out.note() {
                    eprintln!("note: rhbz#{}: {note}", req.rhbz);
                }
                if !out.complete() {
                    return Err(format!(
                        "failed to ping rhbz#{}: {}",
                        req.rhbz,
                        out.last_error.unwrap_or_default()
                    ));
                }
                println!("pinged {pkg} rhbz#{}", req.rhbz);
                req.pinged = true;
                changed = true;
            }
        }
    }
    // "Nobody is waiting on a person" is not the same as "you can do
    // this now": a request can be assigned to you and still be stuck
    // behind its own dependencies. One batched read settles which.
    let dep_ids: Vec<u64> = unblocked_by_people
        .iter()
        .flat_map(|s| s.depends_on.iter().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let still_open: BTreeMap<u64, String> = match bz.bugs(&dep_ids).await {
        Ok(bugs) => bugs
            .iter()
            .filter(|b| b.status != "CLOSED")
            .map(|b| (b.id, b.component.first().cloned().unwrap_or_default()))
            .collect(),
        Err(e) => {
            // Treating unknown as unblocked would promise work that
            // cannot be done; say so and report nothing as ready. The
            // names came from the read that just failed, so these
            // blockers are named by bug alone.
            eprintln!("warning: could not read request dependencies ({e}); not judging readiness");
            dep_ids.iter().map(|id| (*id, String::new())).collect()
        }
    };
    if let Some(report) = skipped_report(&unblocked_by_people, &still_open) {
        println!("{report}");
    }
    Ok(changed)
}

/// A request that needs no maintainer, and what it is still waiting on.
struct Skipped {
    pkg: String,
    rhbz: u64,
    why: String,
    depends_on: Vec<u64>,
}

/// What an escalate run says about the requests it did not ping.
///
/// Split by whether anything is still in the way, because the two
/// read completely differently: one list is work to pick up now, the
/// other is work that is merely nobody else's to chase. Printed as
/// one block at the end rather than per package: a run whose requests
/// were all skipped would otherwise say nothing at all. Pure so the
/// wording and the split are tested without Bugzilla.
///
/// `still_open` maps each unresolved dependency to its package, so a
/// blocker is named by the thing that has to happen rather than by a
/// bug number the reader would have to go and look up.
fn skipped_report(skipped: &[Skipped], still_open: &BTreeMap<u64, String>) -> Option<String> {
    use std::fmt::Write as _;
    if skipped.is_empty() {
        return None;
    }
    let blockers = |s: &Skipped| -> Vec<(String, u64)> {
        let mut b: Vec<(String, u64)> = s
            .depends_on
            .iter()
            .filter_map(|d| still_open.get(d).map(|pkg| (pkg.clone(), *d)))
            .collect();
        b.sort_unstable();
        b
    };
    let (ready, waiting): (Vec<_>, Vec<_>) = skipped.iter().partition(|s| blockers(s).is_empty());

    let width = skipped.iter().map(|s| s.pkg.len()).max().unwrap_or(0);
    let mut out = String::new();
    if !ready.is_empty() {
        let _ = write!(
            out,
            "\n{} request(s) now actionable — nobody else is holding these up:\n",
            ready.len()
        );
        for s in &ready {
            let _ = writeln!(out, "  {:width$}  rhbz#{}  {}", s.pkg, s.rhbz, s.why);
        }
    }
    if !waiting.is_empty() {
        let _ = write!(
            out,
            "\n{} request(s) waiting on dependencies, not on a maintainer:\n",
            waiting.len()
        );
        for s in &waiting {
            let list: Vec<String> = blockers(s)
                .iter()
                .map(|(pkg, b)| {
                    if pkg.is_empty() {
                        format!("rhbz#{b}")
                    } else {
                        format!("{pkg} (rhbz#{b})")
                    }
                })
                .collect();
            let _ = writeln!(
                out,
                "  {:width$}  rhbz#{}  {} — blocked by {}",
                s.pkg,
                s.rhbz,
                s.why,
                list.join(", ")
            );
        }
    }
    (!out.is_empty()).then_some(out)
}

// ---- sync entry points (own runtime, like review_deps) ----

/// `file-request` for a single package. When `report_path` is
/// set, the new bug id is recorded in that report.
pub fn run_file_request(
    pkg: &str,
    blocked: &[String],
    dependson: &[String],
    report_path: Option<&str>,
    opts: &Options,
) -> Result<(), String> {
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| format!("failed to create async runtime: {e}"))?;
    rt.block_on(async {
        // Validate up front (catches SIG-without-FAS) — no network.
        request_description(pkg, &opts.branch, opts.fas.as_deref(), opts.sig.as_deref())?;

        // Base-distro pre-flight: a request for a package that exists
        // in the base distro is always CANTFIX (rhbz#2482250).
        match &opts.base_branch {
            Some(base) => {
                if let Some(vr) = src_probe(&FedrqProbe, base, &[pkg.to_string()])?.get(pkg) {
                    return Err(format!(
                        "{pkg} is in the base distro {base} ({vr}); EPEL must \
                         not replace it — a branch request would be CANTFIX. \
                         An alternate package needs a NEW package review instead"
                    ));
                }
            }
            None => eprintln!(
                "warning: no base-distro mapping for {}; base-distro \
                 pre-flight disabled (pass --base-branch to enable)",
                opts.branch
            ),
        }

        if opts.dry_run {
            println!("would file branch request for {pkg} in {}", opts.branch);
            return Ok(());
        }

        let bz = BzClient::new(&opts.bugzilla_url)
            .with_api_key(opts.api_key.clone())
            .map_err(|e| e.to_string())?;
        // No implicit tracker: the request blocks exactly what
        // --blocked names (the EPEL Packagers SIG, whose tracker every
        // request used to block by default, is defunct).
        let blocks = resolve_refs(&bz, blocked).await?;
        let depends_on = resolve_refs(&bz, dependson).await?;

        let rhbz = file_one(
            &bz,
            pkg,
            &opts.branch,
            opts.fas.as_deref(),
            opts.sig.as_deref(),
            &blocks,
            &depends_on,
        )
        .await?;
        println!("filed {pkg}: rhbz#{rhbz}");

        if let Some(path) = report_path {
            let mut file = ReportFile::load(path)?;
            let mut report = file.view();
            report.branch_requests.insert(
                pkg.to_string(),
                BranchRequest {
                    rhbz,
                    pinged: false,
                    releng: None,
                },
            );
            file.save(&report, path)?;
        }
        Ok(())
    })
}

/// `file-requests` — batch over a report file.
pub fn run_file_requests(
    report_path: &str,
    blocked: &[String],
    opts: &Options,
) -> Result<(), String> {
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| format!("failed to create async runtime: {e}"))?;
    rt.block_on(async {
        let mut file = ReportFile::load(report_path)?;
        let mut report = file.view();
        let changed = file_batch(&mut report, blocked, opts, &FedrqProbe).await?;
        if changed && !opts.dry_run {
            file.save(&report, report_path)?;
        }
        Ok(())
    })
}

/// `escalate` — ping stale requests in a report file.
pub fn run_escalate(report_path: &str, opts: &Options) -> Result<(), String> {
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| format!("failed to create async runtime: {e}"))?;
    rt.block_on(async {
        let mut file = ReportFile::load(report_path)?;
        let mut report = file.view();
        if report.branch_requests.is_empty() {
            println!("No branch requests recorded in {report_path}.");
            return Ok(());
        }
        let changed = escalate(&mut report, opts).await?;
        if changed && !opts.dry_run {
            file.save(&report, report_path)?;
        }
        Ok(())
    })
}

/// The file a branch-request command works on: a `resolve --report`
/// file, or a `check-crate --toml` one, read through its
/// branch-request view and given the filed requests back under
/// `[branch_requests]`.
enum ReportFile {
    Resolve(ResolveReport),
    CheckCrate(CheckCrateReport),
}

impl ReportFile {
    /// A check-crate report names its crate; a resolve report does not.
    fn load(path: &str) -> Result<Self, String> {
        let s = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
        let value: toml::Value = toml::from_str(&s).map_err(|e| format!("parse {path}: {e}"))?;
        Ok(if value.get("crate_name").is_some() {
            Self::CheckCrate(check_crate::load_report(path)?)
        } else {
            Self::Resolve(resolve::load_report(path)?)
        })
    }

    fn view(&self) -> ResolveReport {
        match self {
            Self::Resolve(r) => r.clone(),
            Self::CheckCrate(c) => c.branch_request_report(),
        }
    }

    /// Write `view` back in the file's own format.
    fn save(&mut self, view: &ResolveReport, path: &str) -> Result<(), String> {
        match self {
            Self::Resolve(_) => resolve::write_report(view, path),
            Self::CheckCrate(c) => {
                c.branch_requests = view.branch_requests.clone();
                check_crate::write_toml(c, path)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_nvrs_prefers_longest_package_name() {
        let packages = vec!["python-foo".to_string(), "python-foo-bar".to_string()];
        let nvrs = vec![
            "python-foo-1.0-1.el10".to_string(),
            "python-foo-bar-2.0-1.el10".to_string(),
        ];
        let map = match_nvrs_to_packages(&nvrs, &packages);
        assert_eq!(
            map.get("python-foo").map(String::as_str),
            Some("1.0-1.el10")
        );
        assert_eq!(
            map.get("python-foo-bar").map(String::as_str),
            Some("2.0-1.el10")
        );
    }

    #[test]
    fn partition_filable_skips_overrides_and_base_present() {
        let report = ResolveReport {
            source_branch: "rawhide".into(),
            target_branch: "epel10".into(),
            packages: vec![
                "python-setuptools".into(),
                "rust-newthing".into(),
                "rust-alt".into(),
            ],
            edges: BTreeMap::new(),
            branch_requests: BTreeMap::new(),
            blocked_by_base: BTreeMap::new(),
            overrides: BTreeSet::from(["rust-alt".to_string()]),
        };
        // Pre-flight found python-setuptools in the base — even though
        // the (stale) report lists it as a plain package.
        let mut base_present = BTreeMap::new();
        base_present.insert("python-setuptools".to_string(), "69.0.3-9.el10".to_string());

        // The source has everything but rust-nowhere, which nobody has
        // packaged yet: nothing to branch.
        let source_present: BTreeMap<String, String> = report
            .packages
            .iter()
            .map(|p| (p.clone(), "1-1.fc46".to_string()))
            .collect();

        let mut candidates = report.packages.clone();
        candidates.push("rust-nowhere".to_string());
        let (to_file, skipped) = partition_filable(
            &report,
            candidates,
            &base_present,
            "c10s",
            &source_present,
            &BTreeMap::new(),
            "epel10",
        );
        assert_eq!(to_file, vec!["rust-newthing".to_string()]);
        assert_eq!(skipped.len(), 3);
        assert!(skipped.iter().any(|m| m.contains("rust-nowhere")
            && m.contains("not in rawhide")
            && m.contains("check-pkg-reviews")));
        assert!(skipped.iter().any(|m| m.contains("python-setuptools")
            && m.contains("c10s")
            && m.contains("CANTFIX")));
        assert!(
            skipped
                .iter()
                .any(|m| m.contains("rust-alt") && m.contains("package review"))
        );
    }

    #[test]
    fn partition_filable_skips_a_package_already_branched() {
        // Regression (issue #25): abseil-cpp has been in EPEL 10 for
        // months, but it is neither in the base distro nor missing from
        // the source, so nothing stopped a request being filed for it.
        let report = ResolveReport {
            source_branch: "rawhide".into(),
            target_branch: "epel10".into(),
            packages: vec!["abseil-cpp".into(), "cxxopts".into()],
            edges: BTreeMap::new(),
            branch_requests: BTreeMap::new(),
            blocked_by_base: BTreeMap::new(),
            overrides: BTreeSet::new(),
        };
        let source_present: BTreeMap<String, String> = report
            .packages
            .iter()
            .map(|p| (p.clone(), "1-1.fc46".to_string()))
            .collect();
        let target_present =
            BTreeMap::from([("abseil-cpp".to_string(), "20240722.2-1.el10_3".to_string())]);
        let (to_file, skipped) = partition_filable(
            &report,
            report.packages.clone(),
            &BTreeMap::new(),
            "c10s",
            &source_present,
            &target_present,
            "epel10",
        );
        assert_eq!(to_file, vec!["cxxopts".to_string()]);
        assert_eq!(skipped.len(), 1);
        assert!(
            skipped[0].contains("abseil-cpp")
                && skipped[0].contains("already branched to epel10")
                && skipped[0].contains("20240722.2-1.el10_3"),
            "{}",
            skipped[0]
        );
    }

    #[test]
    fn description_plain() {
        let d = request_description("foo", "epel9", None, None).unwrap();
        assert_eq!(d, "Please branch and build foo in epel9.\n");
    }

    #[test]
    fn description_fas_offers_comaintainer() {
        let d = request_description("foo", "epel9", Some("alice"), None).unwrap();
        assert!(d.contains("co-maintainer of the package (FAS: alice)"));
        assert!(d.contains("/rpms/foo/adduser"));
        assert!(!d.contains("addgroup"));
    }

    #[test]
    fn description_fas_sig_offers_both() {
        let d = request_description("foo", "epel9", Some("alice"), Some("rust-sig")).unwrap();
        assert!(d.contains("the rust-sig would be happy"));
        assert!(d.contains("/rpms/foo/addgroup"));
        assert!(d.contains("Please also add me as a co-maintainer (FAS: alice)"));
    }

    #[test]
    fn description_sig_without_fas_errors() {
        let err = request_description("foo", "epel9", None, Some("SIG")).unwrap_err();
        assert!(err.contains("without"));
    }

    #[test]
    fn ping_body_plain_and_offers() {
        assert_eq!(
            ping_body("foo", "epel9", None, None),
            "Will you be able to branch and build foo in epel9?\n"
        );
        assert!(ping_body("foo", "epel9", Some("alice"), None).contains("FAS: alice"));
        let both = ping_body("foo", "epel9", Some("alice"), Some("rust-sig"));
        assert!(both.contains("rust-sig"));
        assert!(both.contains("FAS: alice"));
    }

    #[test]
    fn ping_decision_rules() {
        // Nothing the asker can do about the branch: the maintainer is
        // the only one who can act, so the bug's own state decides.
        let ask =
            |status: &str, days, pinged| ping_decision(status, days, pinged, false, false, false);
        assert_eq!(ask("NEW", 10, true), PingDecision::AlreadyPinged);
        assert_eq!(ask("CLOSED", 30, false), PingDecision::Closed);
        assert_eq!(ask("NEW", 10, false), PingDecision::Ping);
        assert_eq!(ask("NEW", 3, false), PingDecision::TooNew(3));
        assert_eq!(
            ask("ASSIGNED", 30, false),
            PingDecision::NotNew("ASSIGNED".to_string())
        );
        // Exactly at the threshold pings.
        assert_eq!(ask("NEW", PING_MIN_DAYS, false), PingDecision::Ping);
    }

    #[test]
    fn a_branch_the_asker_cannot_commit_to_still_needs_its_maintainer() {
        // The trap: an existing branch looks like finished work, but
        // without access the asker still cannot build there, so the
        // maintainer is still owed the request.
        assert_eq!(
            ping_decision("NEW", 30, false, false, true, false),
            PingDecision::Ping
        );
    }

    #[test]
    fn a_bug_handed_to_the_person_chasing_it_is_not_escalated() {
        // Reassigning the request to whoever is chasing it is how a
        // maintainer signs off; it usually arrives with the branch and
        // the ACL. Pinging would put `needinfo?` on the assignee, who
        // is now that same person.
        assert_eq!(
            ping_decision("NEW", 30, false, false, false, true),
            PingDecision::Yours
        );
        // It holds even with no access and no branch: the maintainer
        // has said they are done, whatever dist-git shows.
        assert_eq!(
            ping_decision("ASSIGNED", 90, true, false, false, true),
            PingDecision::Yours
        );
        // A closed bug still reports as closed.
        assert_eq!(
            ping_decision("CLOSED", 30, false, false, false, true),
            PingDecision::Closed
        );
    }

    fn skipped(pkg: &str, rhbz: u64, why: &str, depends_on: &[u64]) -> Skipped {
        Skipped {
            pkg: pkg.to_string(),
            rhbz,
            why: why.to_string(),
            depends_on: depends_on.to_vec(),
        }
    }

    /// A flag set `days_ago` before `now`. Both come from the same
    /// instant, so the age is exact rather than a hair under.
    fn needinfo(
        requestee: &str,
        status: &str,
        days_ago: i64,
        now: chrono::DateTime<chrono::Utc>,
    ) -> sandogasa_bugzilla::models::Flag {
        serde_json::from_value(serde_json::json!({
            "name": "needinfo",
            "status": status,
            "requestee": requestee,
            "creation_date": (now - chrono::Duration::days(days_ago)).to_rfc3339(),
        }))
        .expect("a needinfo flag")
    }

    #[test]
    fn the_clock_runs_from_the_ping_not_the_bug() {
        let now = chrono::Utc::now();
        let flags = vec![needinfo("maint@example.com", "?", 20, now)];
        assert_eq!(needinfo_age(&flags, "maint@example.com", now), Some(20));
        // The comparison is on the address, case aside.
        assert_eq!(needinfo_age(&flags, "MAINT@example.com", now), Some(20));
        // A needinfo pointed at somebody else is a different
        // conversation and does not start this clock.
        assert_eq!(needinfo_age(&flags, "other@example.com", now), None);
        // An answered flag is cleared by Bugzilla, which is how we
        // read "they responded" without parsing prose.
        let answered = vec![needinfo("maint@example.com", "+", 20, now)];
        assert_eq!(needinfo_age(&answered, "maint@example.com", now), None);
        // Two pings: the oldest is the one the policy counts from.
        let both = vec![
            needinfo("maint@example.com", "?", 3, now),
            needinfo("maint@example.com", "?", 30, now),
        ];
        assert_eq!(needinfo_age(&both, "maint@example.com", now), Some(30));
    }

    #[test]
    fn releng_rules() {
        // Two weeks of silence is the policy's bar, and the boundary
        // files rather than waiting one more day.
        assert_eq!(
            releng_decision("NEW", Some(RELENG_MIN_DAYS), None),
            RelengDecision::File(RELENG_MIN_DAYS)
        );
        assert_eq!(
            releng_decision("NEW", Some(13), None),
            RelengDecision::TooSoon(13)
        );
        // A recorded ticket settles it before anything else is asked,
        // so a rerun reports rather than files a second one.
        assert_eq!(
            releng_decision("NEW", Some(90), Some(13536)),
            RelengDecision::Filed(13536)
        );
        // Out of NEW is action by the policy's own definition.
        assert_eq!(
            releng_decision("ASSIGNED", Some(90), None),
            RelengDecision::Progressed("ASSIGNED".to_string())
        );
    }

    #[test]
    fn a_cleared_needinfo_is_handed_to_a_person_not_escalated() {
        // The policy would often allow escalating anyway -- a reply of
        // "I do not want to do epel" and nothing else is *no action*
        // by its own wording. Telling that apart from a real answer
        // means reading prose, and being wrong files a public ticket
        // about a maintainer who did reply.
        assert_eq!(
            releng_decision("NEW", None, None),
            RelengDecision::NoNeedinfo
        );
    }

    #[test]
    fn the_ticket_follows_the_trackers_template() {
        let (title, body) = releng_ticket(
            "et",
            "epel10",
            "https://bugzilla.redhat.com/show_bug.cgi?id=2368920",
            "michel",
            &["kkeithle".to_string()],
        );
        assert_eq!(title, "Stalled EPEL package: et");
        // Every heading the template asks for, because triage reads
        // them and free text lands unparsed.
        for heading in [
            "## Describe the issue",
            "## When do you need this? (YYYY/MM/DD)",
            "## When is this no longer needed or useful? (YYYY/MM/DD)",
            "## If we cannot complete your request, what is the impact?",
            "## Checklist",
        ] {
            assert!(body.contains(heading), "missing {heading}:\n{body}");
        }
        assert!(body.contains("https://bugzilla.redhat.com/show_bug.cgi?id=2368920"));
        // The policy requires both FAS mentions: the requester's, and
        // the maintainer's.
        assert!(body.contains("my FAS (@michel)"), "{body}");
        assert!(body.contains("cc: @kkeithle"), "{body}");
    }

    #[test]
    fn a_package_with_no_listed_owner_says_so() {
        // Silently dropping the cc line would leave the ticket missing
        // something the policy names as required, with nothing to say
        // why.
        let (_, body) = releng_ticket("et", "epel10", "url", "michel", &[]);
        assert!(body.contains("cc: (no owner listed in dist-git)"), "{body}");
    }

    #[test]
    fn a_duplicate_is_the_same_title_open_and_nothing_else() {
        let issue = |n: u64, title: &str, state: &str| -> sandogasa_forgejo::Issue {
            serde_json::from_value(serde_json::json!({
                "number": n, "title": title, "state": state,
            }))
            .expect("an issue")
        };
        let title = "Stalled EPEL package: et";
        // The search returns substring matches, so the longer name
        // comes back too; adopting it would file nothing for `et` and
        // record etcd's ticket against it.
        let hits = vec![
            issue(1, "Stalled EPEL package: etcd", "open"),
            issue(2, title, "open"),
        ];
        assert_eq!(open_duplicate(&hits, title), Some(2));
        assert_eq!(open_duplicate(&hits[..1], title), None);
        // A closed ticket of the same name was dealt with; this is a
        // new stall and wants its own.
        assert_eq!(open_duplicate(&[issue(3, title, "closed")], title), None);
    }

    #[test]
    fn adopting_says_whether_the_package_already_landed() {
        // The two cases used to read identically, which is how
        // rhbz#2482238 was offered for adoption months after the
        // build it asked for had shipped.
        assert_eq!(
            adoption_reason("python-aiosmtpd", "epel10", None),
            "escalate may ping it once it is old enough"
        );
        let landed = adoption_reason("python-aiosmtpd", "epel10", Some("1.4.4.post2-18.el10_2"));
        assert_eq!(
            landed,
            "python-aiosmtpd-1.4.4.post2-18.el10_2 is already in epel10; \
             ebranch close will retire it"
        );
        // The offer still stands: adopting is what lets `close` see
        // the bug at all, so this must not read as a refusal.
        assert!(!landed.contains("skip"), "{landed}");
    }

    #[test]
    fn a_request_is_closed_only_once_the_build_is_there() {
        // A branch with nothing built in it does not fulfil the
        // request: the bug asked for the package to be *in* epel10.
        assert_eq!(close_decision("NEW", None), CloseDecision::Pending);
        assert_eq!(
            close_decision("NEW", Some("1.4.4.post2-18.el10_2")),
            CloseDecision::Landed("1.4.4.post2-18.el10_2".to_string())
        );
        // An already-closed bug is left alone whatever the branch says.
        assert_eq!(
            close_decision("CLOSED", Some("1.4.4.post2-18.el10_2")),
            CloseDecision::Closed
        );
    }

    #[test]
    fn a_close_clears_the_needinfo_it_answers() {
        let now = chrono::Utc::now();
        let mut answered = needinfo("maint@example.com", "+", 3, now);
        answered.id = 2;
        let mut open = needinfo("maint@example.com", "?", 3, now);
        open.id = 1;
        let mut other = needinfo("someone@example.com", "?", 3, now);
        other.id = 3;
        // Every outstanding one, not only ours: closing answers the
        // question for whoever was asked, and a flag left on a closed
        // bug sits in their queue forever.
        assert_eq!(outstanding_needinfo(&[answered, open, other]), vec![1, 3]);
    }

    #[test]
    fn a_flag_without_an_id_is_left_alone() {
        // Clearing needs the id -- needinfo is multiplicable, so the
        // type alone does not say which flag is meant. A response
        // that omitted it would otherwise be sent as `"id": 0`.
        let now = chrono::Utc::now();
        assert!(outstanding_needinfo(&[needinfo("m@example.com", "?", 1, now)]).is_empty());
    }

    #[test]
    fn a_close_records_what_it_found() {
        // The evidence goes in the bug: a request closed months after
        // it was satisfied is exactly where "how do you know" is worth
        // answering without rerunning anything.
        let body = close_body("python-aiosmtpd", "epel10", "1.4.4.post2-18.el10_2");
        assert!(
            body.starts_with("python-aiosmtpd-1.4.4.post2-18.el10_2 is in epel10,"),
            "{body}"
        );
        assert!(body.contains("reopen"), "{body}");
        // No trailing whitespace: this text is posted to a public bug.
        assert!(
            body.lines().all(|l| l == l.trim_end()),
            "trailing space in {body:?}"
        );
    }

    #[test]
    fn the_closing_report_is_silent_when_nothing_was_skipped() {
        assert_eq!(skipped_report(&[], &BTreeMap::new()), None);
    }

    #[test]
    fn a_request_with_no_open_dependency_is_ready_to_pick_up() {
        let rows = vec![
            skipped("et", 2368920, "assigned to you", &[]),
            // A dependency that has since closed is not in the way.
            skipped(
                "simpleini",
                2543942,
                "you can branch epel10 yourself",
                &[2531397],
            ),
        ];
        let out = skipped_report(&rows, &BTreeMap::new()).expect("two rows report");
        assert!(out.contains("2 request(s) now actionable"), "{out}");
        assert!(!out.contains("waiting on dependencies"), "{out}");
        assert!(out.contains("rhbz#2368920  assigned to you"), "{out}");
        // Package names line up, so the reasons read as a column.
        assert!(out.contains("  et         rhbz#2368920"), "{out}");
        assert!(out.contains("  simpleini  rhbz#2543942"), "{out}");
    }

    #[test]
    fn a_request_waiting_on_its_dependencies_is_not_called_actionable() {
        // The distinction the two lists draw: nobody is sitting on
        // either request, but only one of them can be acted on, and a
        // run that lumped them together told the reader to go and
        // build something that cannot be built yet.
        let rows = vec![
            skipped("et", 2368920, "assigned to you", &[2543942, 2531397]),
            skipped("simpleini", 2543942, "branched — build it", &[]),
        ];
        let still_open = BTreeMap::from([
            (2543942, "simpleini".to_string()),
            (2531397, "cxxopts".to_string()),
        ]);
        let out = skipped_report(&rows, &still_open).expect("a split report");
        assert!(out.contains("1 request(s) now actionable"), "{out}");
        assert!(out.contains("simpleini  rhbz#2543942  branched"), "{out}");
        assert!(
            out.contains("1 request(s) waiting on dependencies, not on a maintainer"),
            "{out}"
        );
        // A blocker is named by its package and listed in package
        // order, whatever order Bugzilla returned the dependencies
        // in -- the bug number alone says nothing about what has to
        // happen first.
        assert!(
            out.contains(
                "rhbz#2368920  assigned to you — blocked by cxxopts (rhbz#2531397), \
                 simpleini (rhbz#2543942)"
            ),
            "{out}"
        );
    }

    #[test]
    fn someone_elses_bug_assigned_back_to_them_is_still_worth_raising() {
        // Only an assignment to the person *running* ebranch settles
        // anything. A reporter who assigned their own bug to
        // themselves may simply have forgotten it, so the usual rules
        // apply -- and ASSIGNED surfaces for review rather than being
        // pinged blind or dropped.
        assert_eq!(
            ping_decision("ASSIGNED", 90, false, false, false, false),
            PingDecision::NotNew("ASSIGNED".to_string())
        );
        // Still pings a plain NEW request that has sat long enough.
        assert_eq!(
            ping_decision("NEW", 90, false, false, false, false),
            PingDecision::Ping
        );
    }

    #[test]
    fn access_settles_it_whether_or_not_the_branch_exists() {
        // Commit-equivalent access carries both branching and
        // building, so no one else is owed anything either way -- but
        // the follow-up differs, which is what `branched` reports.
        assert_eq!(
            ping_decision("NEW", 30, false, true, true, false),
            PingDecision::NothingOwed { branched: true }
        );
        assert_eq!(
            ping_decision("NEW", 30, false, true, false, false),
            PingDecision::NothingOwed { branched: false }
        );
        // It outranks age and a previous ping, which only say when to
        // ask again, not whether asking is warranted at all.
        assert_eq!(
            ping_decision("NEW", 1, true, true, false, false),
            PingDecision::NothingOwed { branched: false }
        );
        // A closed bug is settled first: there is nothing to report.
        assert_eq!(
            ping_decision("CLOSED", 30, false, true, true, false),
            PingDecision::Closed
        );
    }

    #[test]
    fn report_file_reads_either_kind_and_writes_requests_back() {
        let dir = tempfile::tempdir().unwrap();
        let resolve_path = dir.path().join("resolve.toml");
        let crate_path = dir.path().join("crate.toml");
        let resolve_path = resolve_path.to_str().unwrap();
        let crate_path = crate_path.to_str().unwrap();
        resolve::write_report(
            &ResolveReport {
                source_branch: "rawhide".into(),
                target_branch: "c10s".into(),
                packages: vec!["python-foo".into()],
                edges: BTreeMap::new(),
                branch_requests: BTreeMap::new(),
                blocked_by_base: BTreeMap::new(),
                overrides: BTreeSet::new(),
            },
            resolve_path,
        )
        .unwrap();
        // A check-crate report, as `check-crate -b epel10 --toml` writes
        // it: one missing dependency, one satisfied.
        check_crate::write_toml(
            &serde_json::from_value(serde_json::json!({
                "crate_name": "tiny-dfr", "crate_version": "0.3.7",
                "package": "rust-tiny-dfr", "branch": "epel10",
                "dependencies": [
                    {"name": "glib", "version_req": "^0.20", "kind": "normal",
                     "optional": false, "status": "missing"},
                    {"name": "anyhow", "version_req": "^1", "kind": "normal",
                     "optional": false, "status": "satisfied", "version": "1.0.104"}
                ],
                "transitive_missing": [
                    {"name": "glib-sys", "package": "rust-glib-sys", "status": "missing",
                     "version": "0.20.0", "version_req": "^0.20", "pulled_by": "glib"}
                ],
                "transitive_edges": {"glib": ["glib-sys"]}
            }))
            .unwrap(),
            crate_path,
        )
        .unwrap();

        let resolve_file = ReportFile::load(resolve_path).unwrap();
        assert!(matches!(resolve_file, ReportFile::Resolve(_)));
        assert_eq!(resolve_file.view().packages, vec!["python-foo".to_string()]);

        let mut crate_file = ReportFile::load(crate_path).unwrap();
        assert!(matches!(crate_file, ReportFile::CheckCrate(_)));
        let mut view = crate_file.view();
        assert_eq!(view.source_branch, "rawhide");
        assert_eq!(
            view.packages,
            vec!["rust-glib".to_string(), "rust-glib-sys".to_string()]
        );
        assert_eq!(
            view.edges["rust-glib"],
            BTreeSet::from(["rust-glib-sys".to_string()])
        );

        // Filed requests land in the check-crate file and survive a reload.
        view.branch_requests.insert(
            "rust-glib-sys".into(),
            BranchRequest {
                rhbz: 42,
                pinged: false,
                releng: None,
            },
        );
        crate_file.save(&view, crate_path).unwrap();
        let again = ReportFile::load(crate_path).unwrap();
        assert!(matches!(again, ReportFile::CheckCrate(_)));
        assert_eq!(again.view().branch_requests["rust-glib-sys"].rhbz, 42);
        assert_eq!(
            check_crate::load_report(crate_path).unwrap().crate_name,
            "tiny-dfr"
        );
    }

    #[test]
    fn an_open_request_filed_elsewhere_is_adopted() {
        // Regression (issue #22): file-requests only knew the requests
        // the report recorded, so rhbz#2368920 -- "Please branch and
        // build et in epel10", filed by hand in May -- would have been
        // duplicated.
        let bug = |id: u64, summary: &str, component: &str| {
            serde_json::from_value::<sandogasa_bugzilla::models::Bug>(serde_json::json!({
                "id": id,
                "summary": summary,
                "status": "NEW",
                "resolution": "",
                "product": "Fedora EPEL",
                "component": [component],
                "severity": "unspecified",
                "priority": "unspecified",
                "assigned_to": "someone",
                "creator": "someone",
                "creation_time": "2025-05-28T09:02:27Z",
                "last_change_time": "2025-05-28T09:02:27Z",
            }))
            .expect("a bug as Bugzilla sends it")
        };
        let bugs = [
            bug(2368920, "Please branch and build et in epel10", "et"),
            // Another release's request is not this one.
            bug(
                2400000,
                "Please branch and build cxxopts in epel9",
                "cxxopts",
            ),
            // Not a request at all.
            bug(2400001, "et-6.2.13 is available", "et"),
            // Names a package it was not filed against.
            bug(2400002, "Please branch and build gtest in epel10", "et"),
            // Names a subpackage, and says more after the branch: this
            // is rhbz#2531397's shape, which the first matcher missed
            // and a duplicate was filed over.
            bug(
                2400003,
                "Please branch and build cxxopts-devel in epel10 and other epel10.x branches",
                "cxxopts",
            ),
        ];
        // The report targets a minor of the release the request names.
        let found = adoptable(&bugs, "epel10.4");
        assert_eq!(found.get("et"), Some(&2368920));
        // Recorded under the component, not the subpackage asked for.
        assert_eq!(found.get("cxxopts"), Some(&2400003));
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(adoptable(&bugs, "epel9").contains_key("cxxopts"));
    }

    #[test]
    fn the_oldest_open_request_wins() {
        // Two people filed for the same package; the first is the
        // request and the second is the duplicate.
        let bug = |id: u64| {
            serde_json::from_value::<sandogasa_bugzilla::models::Bug>(serde_json::json!({
                "id": id,
                "summary": "Please branch and build et in epel10",
                "status": "NEW",
                "resolution": "",
                "product": "Fedora EPEL",
                "component": ["et"],
                "severity": "unspecified",
                "priority": "unspecified",
                "assigned_to": "someone",
                "creator": "someone",
                "creation_time": "2025-05-28T09:02:27Z",
                "last_change_time": "2025-05-28T09:02:27Z",
            }))
            .expect("a bug as Bugzilla sends it")
        };
        let found = adoptable(&[bug(2400003), bug(2368920)], "epel10");
        assert_eq!(found.get("et"), Some(&2368920));
    }

    /// A bug as Bugzilla's REST API returns it, with only the fields
    /// this module reads set to anything interesting.
    /// No dist-git answers: the asker holds nothing and the branch is
    /// absent, which is the case where the bug's own state decides.
    fn no_standing() -> BTreeMap<String, BranchStanding> {
        BTreeMap::new()
    }

    fn bug_json(id: u64, status: &str, dupe_of: Option<u64>) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "summary": "Please branch and build cxxopts in epel10",
            "status": status,
            "resolution": if dupe_of.is_some() { "DUPLICATE" } else { "" },
            "dupe_of": dupe_of,
            "product": "Fedora EPEL",
            "component": ["cxxopts"],
            "severity": "unspecified",
            "priority": "unspecified",
            "assigned_to": "maintainer@example.com",
            "creator": "asker@example.com",
            "creation_time": "2026-09-10T10:56:39Z",
            "last_change_time": "2026-09-30T14:25:03Z",
        })
    }

    #[tokio::test]
    async fn a_request_closed_as_a_duplicate_is_followed() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // rhbz#2543941 was closed today as a duplicate of the older
        // rhbz#2531397, which is the one still being worked (issue #26).
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/bug/2543941"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [bug_json(2543941, "CLOSED", Some(2531397))]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/bug/2531397"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [bug_json(2531397, "ON_QA", None)]
            })))
            .mount(&server)
            .await;

        let bz = BzClient::new(&server.uri());
        let start = bz.bug(2543941).await.expect("the recorded bug");
        let live = follow_duplicate(&bz, "cxxopts", start)
            .await
            .expect("the bug it points at");
        assert_eq!(live.id, 2531397);
        assert_eq!(live.status, "ON_QA");
    }

    #[tokio::test]
    async fn a_duplicate_loop_stops_rather_than_spinning() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        for (id, dupe) in [(1u64, 2u64), (2, 1)] {
            Mock::given(method("GET"))
                .and(path(format!("/rest/bug/{id}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "bugs": [bug_json(id, "CLOSED", Some(dupe))]
                })))
                .mount(&server)
                .await;
        }

        let bz = BzClient::new(&server.uri());
        let start = bz.bug(1).await.expect("the recorded bug");
        let stopped = follow_duplicate(&bz, "loopy", start)
            .await
            .expect("a loop is not an error");
        assert_eq!(stopped.id, 2, "stops on the bug it has already seen");
    }

    /// An options set pointed at a mock Bugzilla.
    fn mock_opts(url: &str, dry_run: bool) -> Options {
        Options {
            bugzilla_url: url.to_string(),
            api_key: "not-a-key".to_string(),
            branch: "epel10".to_string(),
            fas: Some("alice".to_string()),
            sig: None,
            dry_run,
            verbose: false,
            base_branch: None,
            email: None,
            provenpackager: false,
            identity: None,
            forge_url: String::new(),
            forge_token: String::new(),
            assume_yes: false,
        }
    }

    /// A report holding one request for one package.
    fn report_with(pkg: &str, rhbz: u64, pinged: bool) -> ResolveReport {
        ResolveReport {
            source_branch: "rawhide".into(),
            target_branch: "epel10".into(),
            packages: vec![pkg.to_string()],
            edges: BTreeMap::new(),
            branch_requests: BTreeMap::from([(
                pkg.to_string(),
                BranchRequest {
                    rhbz,
                    pinged,
                    releng: None,
                },
            )]),
            blocked_by_base: BTreeMap::new(),
            overrides: BTreeSet::new(),
        }
    }

    #[tokio::test]
    async fn escalate_pings_a_request_that_has_sat_in_new() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let old = chrono::Utc::now() - chrono::Duration::days(30);
        let mut bug = bug_json(2368920, "NEW", None);
        bug["creation_time"] = serde_json::json!(old.to_rfc3339());
        Mock::given(method("GET"))
            .and(path("/rest/bug/2368920"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"bugs": [bug]})),
            )
            .mount(&server)
            .await;
        // update_verified reads the bugs before writing, then writes.
        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [bug_json(2368920, "NEW", None)]
            })))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/rest/bug/2368920"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [{"id": 2368920, "changes": {}}]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let mut report = report_with("et", 2368920, false);
        let changed = escalate_with(
            &mut report,
            &mock_opts(&server.uri(), false),
            &no_standing(),
            &BTreeMap::new(),
        )
        .await
        .expect("the ping goes through");
        assert!(changed);
        assert!(
            report.branch_requests["et"].pinged,
            "a pinged request is marked so it is not pinged twice"
        );
    }

    #[tokio::test]
    async fn a_live_needinfo_counts_as_a_ping_the_report_never_saw() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Adopting an existing request into a fresh report gives it
        // `pinged = false` however many times it was pinged, so the
        // bool alone would put a second `needinfo?` on the same
        // person. Found on a real run: simpleini had been pinged
        // hours earlier and a new report offered to ping it again.
        let server = MockServer::start().await;
        let now = chrono::Utc::now();
        let mut bug = bug_json(2543942, "NEW", None);
        bug["creation_time"] = serde_json::json!((now - chrono::Duration::days(30)).to_rfc3339());
        bug["flags"] = serde_json::json!([{
            "name": "needinfo",
            "status": "?",
            "requestee": "maintainer@example.com",
            "creation_date": (now - chrono::Duration::days(2)).to_rfc3339(),
        }]);
        Mock::given(method("GET"))
            .and(path("/rest/bug/2543942"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"bugs": [bug]})),
            )
            .mount(&server)
            .await;
        // No PUT is mounted: a write would fail the test outright.
        let mut report = report_with("simpleini", 2543942, false);
        let changed = escalate_with(
            &mut report,
            &mock_opts(&server.uri(), false),
            &no_standing(),
            &BTreeMap::new(),
        )
        .await
        .expect("the run completes without pinging");
        assert!(
            report.branch_requests["simpleini"].pinged,
            "the live flag is written back, so the report stops lying"
        );
        assert!(changed, "learning that it was pinged is a change to save");
    }

    #[tokio::test]
    async fn escalate_waits_while_a_request_is_young() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let fresh = chrono::Utc::now() - chrono::Duration::days(2);
        let mut bug = bug_json(2543942, "NEW", None);
        bug["creation_time"] = serde_json::json!(fresh.to_rfc3339());
        Mock::given(method("GET"))
            .and(path("/rest/bug/2543942"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"bugs": [bug]})),
            )
            .mount(&server)
            .await;
        // Nothing is written: no PUT is mounted, so one would fail the
        // request outright.

        let mut report = report_with("simpleini", 2543942, false);
        let changed = escalate_with(
            &mut report,
            &mock_opts(&server.uri(), false),
            &no_standing(),
            &BTreeMap::new(),
        )
        .await
        .expect("waiting is not an error");
        assert!(!changed);
        assert!(!report.branch_requests["simpleini"].pinged);
    }

    #[tokio::test]
    async fn escalate_follows_a_duplicate_before_deciding() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // The recorded request was closed against an older one that is
        // already being worked, so there is nothing to ping -- but the
        // record must move to the live bug (issue #26).
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/bug/2543941"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [bug_json(2543941, "CLOSED", Some(2531397))]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/bug/2531397"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [bug_json(2531397, "ON_QA", None)]
            })))
            .mount(&server)
            .await;

        let mut report = report_with("cxxopts", 2543941, true);
        let changed = escalate_with(
            &mut report,
            &mock_opts(&server.uri(), false),
            &no_standing(),
            &BTreeMap::new(),
        )
        .await
        .expect("following a duplicate is not an error");
        assert!(changed);
        let req = &report.branch_requests["cxxopts"];
        assert_eq!(req.rhbz, 2531397);
        assert!(!req.pinged, "the new bug has had no ping of ours");
    }

    #[tokio::test]
    async fn filing_falls_back_to_fedora_when_the_component_is_not_in_epel() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // A package nobody has branched has no EPEL component yet, so
        // Bugzilla refuses the EPEL product and the request goes to
        // Fedora/rawhide instead -- which is why rhbz#2367248 for
        // rust-tiny-dfr lives under Fedora while rhbz#2368920 for et
        // lives under Fedora EPEL.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/rest/bug"))
            .and(body_partial_json(
                serde_json::json!({"product": "Fedora EPEL"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "message": "There is no component named 'widget' in product 'Fedora EPEL'."
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/rest/bug"))
            .and(body_partial_json(serde_json::json!({
                "product": "Fedora",
                "version": "rawhide",
                "component": "widget",
                "summary": "Please branch and build widget in epel10",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 4242})))
            .expect(1)
            .mount(&server)
            .await;

        let bz = BzClient::new(&server.uri());
        let id = file_one(&bz, "widget", "epel10", Some("alice"), None, &[], &[])
            .await
            .expect("the Fedora attempt answers");
        assert_eq!(id, 4242);
    }

    #[tokio::test]
    async fn linking_adds_to_the_edges_a_human_already_drew() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // rhbz#2368920 already carried a depends_on somebody added by
        // hand, so the update has to add rather than set (issue #22).
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [bug_json(2368920, "NEW", None)]
            })))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/rest/bug/2368920"))
            .and(body_partial_json(
                serde_json::json!({"depends_on": {"add": [2531397]}}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [{"id": 2368920, "changes": {}}]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let bz = BzClient::new(&server.uri());
        let edges = BTreeMap::from([(
            "et".to_string(),
            BTreeSet::from(["cxxopts".to_string(), "unfiled".to_string()]),
        )]);
        let requests = BTreeMap::from([
            (
                "et".to_string(),
                BranchRequest {
                    rhbz: 2368920,
                    pinged: false,
                    releng: None,
                },
            ),
            (
                "cxxopts".to_string(),
                BranchRequest {
                    rhbz: 2531397,
                    pinged: false,
                    releng: None,
                },
            ),
        ]);
        // `unfiled` has no request, so it contributes no edge.
        link_requests(&bz, &edges, &requests, false).await;
    }

    #[tokio::test]
    async fn a_bug_bugzilla_refuses_to_link_does_not_stop_the_others() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Bugzilla is wonky under a batch of writes; one refusal must
        // not abandon the rest of the graph, nor the bug IDs the
        // caller has yet to write back to the report.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [bug_json(1, "NEW", None), bug_json(2, "NEW", None)]
            })))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/rest/bug/1"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        // The second bug is still attempted, and succeeds.
        Mock::given(method("PUT"))
            .and(path("/rest/bug/2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [{"id": 2, "changes": {}}]
            })))
            .expect(1)
            .mount(&server)
            .await;

        // No backoff: the retries are the behaviour under test, not
        // the waiting between them.
        let bz = BzClient::new(&server.uri()).with_retry_backoff(std::time::Duration::ZERO);
        let edges = BTreeMap::from([
            ("a".to_string(), BTreeSet::from(["dep".to_string()])),
            ("b".to_string(), BTreeSet::from(["dep".to_string()])),
        ]);
        let requests = BTreeMap::from([
            (
                "a".to_string(),
                BranchRequest {
                    rhbz: 1,
                    pinged: false,
                    releng: None,
                },
            ),
            (
                "b".to_string(),
                BranchRequest {
                    rhbz: 2,
                    pinged: false,
                    releng: None,
                },
            ),
            (
                "dep".to_string(),
                BranchRequest {
                    rhbz: 3,
                    pinged: false,
                    releng: None,
                },
            ),
        ]);
        link_requests(&bz, &edges, &requests, false).await;
    }

    #[tokio::test]
    async fn searching_finds_the_open_request_for_a_candidate() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .and(query_param("bug_status", "__open__"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [
                    bug_json(2368920, "NEW", None),
                    // Open against the same component, but not a request.
                    {
                        "id": 2400001,
                        "summary": "cxxopts-3.3.1 is available",
                        "status": "NEW",
                        "resolution": "",
                        "product": "Fedora",
                        "component": ["cxxopts"],
                        "severity": "unspecified",
                        "priority": "unspecified",
                        "assigned_to": "someone",
                        "creator": "someone",
                        "creation_time": "2026-05-28T09:02:27Z",
                        "last_change_time": "2026-05-28T09:02:27Z",
                    }
                ]
            })))
            .mount(&server)
            .await;

        let bz = BzClient::new(&server.uri());
        let found = search_existing(&bz, &["cxxopts".to_string()], "epel10.4").await;
        assert_eq!(found.get("cxxopts"), Some(&2368920));
        assert_eq!(found.len(), 1, "{found:?}");
    }

    #[tokio::test]
    async fn a_search_that_fails_warns_rather_than_stopping_the_run() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Bugzilla being unreachable must not stop a filing run; the
        // cost is that a request filed elsewhere may be duplicated, and
        // the warning says so.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let bz = BzClient::new(&server.uri());
        let found = search_existing(&bz, &["cxxopts".to_string()], "epel10").await;
        assert!(found.is_empty());
    }

    #[tokio::test]
    async fn refreshing_moves_a_record_off_a_duplicate() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // The batch read of what the report records.
        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [bug_json(2543941, "CLOSED", Some(2531397))]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/bug/2543941"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [bug_json(2543941, "CLOSED", Some(2531397))]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/bug/2531397"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [bug_json(2531397, "ON_QA", None)]
            })))
            .mount(&server)
            .await;

        let bz = BzClient::new(&server.uri());
        let mut report = report_with("cxxopts", 2543941, true);
        let changed = refresh_recorded(&bz, &mut report)
            .await
            .expect("the read succeeds");
        assert!(changed);
        assert_eq!(report.branch_requests["cxxopts"].rhbz, 2531397);
        assert!(!report.branch_requests["cxxopts"].pinged);

        // A report holding nothing asks Bugzilla nothing.
        let mut empty = report_with("cxxopts", 2531397, false);
        empty.branch_requests.clear();
        assert!(!refresh_recorded(&bz, &mut empty).await.unwrap());
    }

    #[test]
    fn previewing_links_names_only_the_packages_with_requests() {
        let edges = BTreeMap::from([
            (
                "et".to_string(),
                BTreeSet::from(["cxxopts".to_string(), "unfiled".to_string()]),
            ),
            // A package with no request of its own is skipped.
            ("unfiled".to_string(), BTreeSet::from(["et".to_string()])),
        ]);
        let requests = BTreeMap::from([
            (
                "et".to_string(),
                BranchRequest {
                    rhbz: 2368920,
                    pinged: false,
                    releng: None,
                },
            ),
            (
                "cxxopts".to_string(),
                BranchRequest {
                    rhbz: 2531397,
                    pinged: false,
                    releng: None,
                },
            ),
        ]);
        preview_links(&edges, &requests);
    }

    /// A probe answering from a table: which branch has which package,
    /// at which version, in which repo class.
    #[derive(Default)]
    struct FakeProbe {
        /// (branch, repo) -> package -> version-release.
        present: BTreeMap<(String, Option<String>), BTreeMap<String, String>>,
    }

    impl FakeProbe {
        fn has(mut self, branch: &str, repo: Option<&str>, pkg: &str, vr: &str) -> Self {
            self.present
                .entry((branch.to_string(), repo.map(str::to_string)))
                .or_default()
                .insert(pkg.to_string(), vr.to_string());
            self
        }
    }

    impl SourceProbe for FakeProbe {
        fn present(
            &self,
            branch: &str,
            repo: Option<&str>,
            packages: &[String],
        ) -> Result<BTreeMap<String, String>, String> {
            let key = (branch.to_string(), repo.map(str::to_string));
            Ok(self
                .present
                .get(&key)
                .map(|have| {
                    packages
                        .iter()
                        .filter_map(|p| have.get(p).map(|vr| (p.clone(), vr.clone())))
                        .collect()
                })
                .unwrap_or_default())
        }
    }

    #[tokio::test]
    async fn a_filing_run_files_only_what_is_left_after_the_pre_flights() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // The et closure as it really stands: one package the base
        // distro owns, one already branched and sitting in testing, one
        // nobody has packaged, one with a request open already, and one
        // genuinely to file for.
        let probe = FakeProbe::default()
            .has("rawhide", None, "et", "6.2.13-1.fc46")
            .has("rawhide", None, "cxxopts", "3.3.1-1.fc46")
            .has("rawhide", None, "simpleini", "4.22-1.fc46")
            .has("rawhide", None, "openssl", "4.0-1.fc46")
            .has("c10s", None, "openssl", "3.5.8-1.el10")
            .has("epel10", Some("@testing"), "cxxopts", "3.3.1-1.el10_3");

        let server = MockServer::start().await;
        // Only et has a request open already.
        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [{
                    "id": 2368920,
                    "summary": "Please branch and build et in epel10",
                    "status": "NEW",
                    "resolution": "",
                    "product": "Fedora EPEL",
                    "component": ["et"],
                    "severity": "unspecified",
                    "priority": "unspecified",
                    "assigned_to": "someone",
                    "creator": "someone",
                    "creation_time": "2026-05-28T09:02:27Z",
                    "last_change_time": "2026-05-28T09:02:27Z",
                }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/rest/bug"))
            .and(body_partial_json(
                serde_json::json!({"component": "simpleini"}),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 2543942})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mut report = ResolveReport {
            source_branch: "rawhide".into(),
            target_branch: "epel10".into(),
            packages: vec![
                "et".into(),
                "cxxopts".into(),
                "openssl".into(),
                "simpleini".into(),
                "unpackaged".into(),
            ],
            edges: BTreeMap::new(),
            branch_requests: BTreeMap::new(),
            blocked_by_base: BTreeMap::new(),
            overrides: BTreeSet::new(),
        };
        let mut opts = mock_opts(&server.uri(), false);
        opts.base_branch = Some("c10s".to_string());

        let changed = file_batch(&mut report, &[], &opts, &probe)
            .await
            .expect("the run completes");
        assert!(changed);
        // et adopted the open request; simpleini was filed; the other
        // three never reached Bugzilla, and the mock would have failed
        // the test had they been posted.
        assert_eq!(report.branch_requests["et"].rhbz, 2368920);
        assert_eq!(report.branch_requests["simpleini"].rhbz, 2543942);
        assert_eq!(
            report.branch_requests.len(),
            2,
            "{:?}",
            report.branch_requests
        );
    }

    #[tokio::test]
    async fn a_dry_run_writes_nothing_anywhere() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let probe = FakeProbe::default().has("rawhide", None, "simpleini", "4.22-1.fc46");
        let server = MockServer::start().await;
        // Reading is allowed; a POST is not mounted, so filing would
        // fail the run outright.
        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"bugs": []})))
            .mount(&server)
            .await;

        let mut report = ResolveReport {
            source_branch: "rawhide".into(),
            target_branch: "epel10".into(),
            packages: vec!["simpleini".into()],
            edges: BTreeMap::new(),
            branch_requests: BTreeMap::new(),
            blocked_by_base: BTreeMap::new(),
            overrides: BTreeSet::new(),
        };
        let opts = mock_opts(&server.uri(), true);
        let changed = file_batch(&mut report, &[], &opts, &probe)
            .await
            .expect("a dry run completes");
        assert!(!changed);
        assert!(report.branch_requests.is_empty());
    }

    #[test]
    fn a_branched_package_is_asked_to_be_built_not_branched() {
        // Issue #30: et has had an epel10 branch in dist-git since
        // months before anything was built there. A request for it is
        // still worth filing — the dependency edges anchor on it — but
        // asking to branch what is branched reads as noise.
        let branched = BranchStanding {
            branched: true,
            access: None,
            scope: None,
            owners: Vec::new(),
        };
        assert_eq!(request_kind(Some(&branched)), RequestKind::Build);
        assert_eq!(request_kind(None), RequestKind::Branch);
        assert_eq!(
            request_kind(Some(&BranchStanding::default())),
            RequestKind::Branch
        );

        assert_eq!(
            RequestKind::Build.summary("et", "epel10"),
            "Please build et in epel10"
        );
        let body = request_description_for(RequestKind::Build, "et", "epel10", None, None).unwrap();
        assert!(
            body.contains("already has a et branch in dist-git"),
            "{body}"
        );
        assert!(!body.contains("Please branch and build"), "{body}");
    }

    #[test]
    fn access_decides_whether_one_can_branch_rather_than_ask() {
        use sandogasa_distgit::AccessLevel::*;
        let with = |access| BranchStanding {
            branched: false,
            access: Some(access),
            scope: None,
            owners: Vec::new(),
        };
        // salimma is an admin on et, so the request was asking for work
        // they could do.
        assert!(with(Admin).may_branch());
        assert!(with(Owner).may_branch());
        assert!(with(Commit).may_branch());
        // Collaborator is how EPEL access is usually granted, but only
        // where its scope reaches the branch being asked for: a scope
        // that stops short, or one that could not be read, asks a
        // person. A ticket holder never acts.
        assert!(!with(Collaborator).may_branch());
        assert!(
            BranchStanding {
                branched: false,
                access: Some(Collaborator),
                scope: Some(true),
                owners: Vec::new(),
            }
            .may_branch()
        );
        assert!(
            !BranchStanding {
                branched: false,
                access: Some(Collaborator),
                scope: Some(false),
                owners: Vec::new(),
            }
            .may_branch()
        );
        assert!(!with(Ticket).may_branch());
        assert!(!BranchStanding::default().may_branch());
    }

    #[test]
    fn a_collaborator_scope_is_read_the_way_pagure_reads_it() {
        // fzf grants epel-packagers-sig "epel*", which is the common
        // shape; a scope naming one branch covers only that one.
        assert!(scope_covers("epel*", "epel10"));
        assert!(scope_covers("epel*", "epel9"));
        assert!(!scope_covers("epel9", "epel10"));
        assert!(!scope_covers("rawhide", "epel10"));
        // Comma-separated, each pattern trimmed, as Pagure splits it.
        assert!(scope_covers("rawhide, epel10", "epel10"));
        assert!(scope_covers("f4*,epel*", "epel10.3"));
        assert!(!scope_covers("f4*, epel9*", "epel10"));
        assert!(!scope_covers("", "epel10"));
    }

    #[test]
    fn a_request_is_claimed_only_when_it_is_the_asker_s_own_bookkeeping() {
        let mut opts = mock_opts("http://127.0.0.1:1", true);
        opts.email = Some("me@example.com".into());
        let can = BranchStanding {
            branched: false,
            access: Some(sandogasa_distgit::AccessLevel::Commit),
            scope: None,
            owners: Vec::new(),
        };
        let cannot = BranchStanding {
            branched: false,
            access: Some(sandogasa_distgit::AccessLevel::Ticket),
            scope: None,
            owners: Vec::new(),
        };
        assert_eq!(claim_for(Some(&can), &opts), Some("me@example.com"));
        // Somebody else's package: the bug is a question for them, and
        // assigning it to us would take it off their list.
        assert_eq!(claim_for(Some(&cannot), &opts), None);
        assert_eq!(claim_for(None, &opts), None);
        // No configured account, nothing to claim as.
        opts.email = None;
        assert_eq!(claim_for(Some(&can), &opts), None);
    }

    #[test]
    fn a_branch_the_asker_requested_is_recorded_not_asked_for() {
        // The summary stays in the family `adoptable` matches, so a
        // re-run finds the bug; the body says what actually happened.
        assert_eq!(
            RequestKind::Requested.summary("cpdup", "epel10"),
            RequestKind::Branch.summary("cpdup", "epel10")
        );
        let body =
            request_description_for(RequestKind::Requested, "cpdup", "epel10", None, None).unwrap();
        assert!(
            body.contains("I have asked for the epel10 branch"),
            "{body}"
        );
        assert!(body.contains("fedpkg request-branch"), "{body}");
        assert!(!body.contains("Please branch and build"), "{body}");
        // No co-maintainer offer on your own package, however the run
        // was invoked.
        let with_offer =
            request_description_for(RequestKind::Requested, "cpdup", "epel10", Some("me"), None)
                .unwrap();
        assert_eq!(with_offer, body);
        // And the kind follows what the asker did, not just dist-git.
        let unbranched = BranchStanding::default();
        assert_eq!(
            Offer {
                requested: true,
                file: true
            }
            .kind(Some(&unbranched)),
            RequestKind::Requested
        );
        assert_eq!(
            Offer::only_file().kind(Some(&unbranched)),
            RequestKind::Branch
        );
    }

    #[test]
    fn a_dry_run_says_the_question_is_coming_rather_than_answering_it() {
        // The prompt is real; only a run with nobody to ask decides on
        // the asker's behalf.
        let dry = unattended_record_note(true);
        assert!(dry.contains("would ask whether to run it"), "{dry}");
        assert!(dry.contains("whether to file a request"), "{dry}");
        let unattended = unattended_record_note(false);
        assert!(unattended.contains("not a terminal"), "{unattended}");
        assert!(
            unattended.contains("filing a request anyway"),
            "{unattended}"
        );
    }

    #[test]
    fn provenpackager_is_mentioned_only_for_a_build_of_a_branched_package() {
        let note = provenpackager_note("et", RequestKind::Build, true).expect("a note");
        assert!(note.contains("emergency"), "{note}");
        assert!(note.contains("exception"), "{note}");
        // Branching is not something provenpackager can do, so a
        // branch request must not raise it — nor must a run by
        // somebody who does not hold it.
        assert_eq!(provenpackager_note("et", RequestKind::Branch, true), None);
        assert_eq!(provenpackager_note("et", RequestKind::Build, false), None);
    }
}
