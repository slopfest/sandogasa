// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Submitting a checked side tag to Bodhi (`check-update --submit`).
//!
//! After the reverse-dependency check passes (or the reviewer
//! curates / overrides the findings), creates the update from the
//! side tag via `POST /updates/` with `from_tag` — the API behind
//! `bodhi updates new --from-tag`. The plan (packages, type, bugs,
//! karma thresholds, notes) is shown for confirmation first, so an
//! accidentally missing package is visible before anything is
//! published.
//!
//! Authentication reuses the bodhi CLI's cached OIDC session, like
//! the karma flow (see `karma::ensure_session`).

use std::path::Path;

use sandogasa_bodhi::models::{NewUpdateFromTag, UpdateSource};
use sandogasa_bodhi::{BodhiClient, auth};

/// What to submit alongside the side tag.
pub struct SubmitOptions {
    /// Update notes/description (from `--notes` / `--notes-file`).
    pub notes: String,
    /// `bugfix` | `enhancement` | `security` | `newpackage`.
    pub update_type: String,
    /// `unspecified` | `low` | `medium` | `high` | `urgent`.
    pub severity: String,
    /// Bug IDs to associate (closed when the update goes stable).
    pub bugs: Vec<u64>,
    /// Push automatically at the karma thresholds. Forced off when
    /// `rpmfusion` is non-empty — see [`SubmitOptions::autokarma`].
    pub autokarma: bool,
    /// RPM Fusion counterparts of this update's packages, as
    /// (Fedora name, RPM Fusion name). Non-empty means the update
    /// must not reach stable on karma alone: RPM Fusion has to
    /// rebuild its side first, and an autopush would land the pair
    /// broken for anyone with both repositories enabled. Empty
    /// whenever `--rpmfusion-inventory` was not given, so the guard
    /// costs nothing when it is not asked for.
    pub rpmfusion: Vec<(String, String)>,
    pub stable_karma: i32,
    pub unstable_karma: i32,
    /// Skip the submission confirmation.
    pub assume_yes: bool,
    /// Koji profile for reading build changelogs, when not the
    /// default one.
    pub koji_profile: Option<String>,
}

/// Resolve the update notes from `--notes` (inline) or `--notes-file`
/// (for longer descriptions). Bodhi rejects empty notes, so require
/// one of the two and a non-blank result — checked up front, before
/// the analysis runs for minutes.
pub fn resolve_notes(inline: Option<&str>, file: Option<&Path>) -> Result<String, String> {
    let notes = match (inline, file) {
        (Some(n), _) => n.to_string(),
        (None, Some(p)) => std::fs::read_to_string(p)
            .map_err(|e| format!("cannot read --notes-file {}: {e}", p.display()))?,
        (None, None) => {
            return Err(
                "Bodhi requires update notes: pass --notes <text> or --notes-file <path>"
                    .to_string(),
            );
        }
    };
    if notes.trim().is_empty() {
        return Err("update notes are empty".to_string());
    }
    Ok(notes)
}

/// Why a bug does not belong on this update, if it does not.
///
/// The package check is first because it is the stronger claim: the
/// update ships nothing that bug is about. The release check catches
/// what it cannot — a bug for a package the update *does* build, but
/// for a different release.
fn misfiled_reason(
    bug: &crate::karma::BugFacts,
    packages: &[String],
    branch: &str,
) -> Option<String> {
    if let Some(package) = crate::karma::bug_package(&bug.summary, bug.component.as_deref())
        && !packages.contains(&package)
    {
        return Some(format!("no {package} among the update's packages"));
    }
    crate::karma::other_release_note(&bug.summary, branch)
}

/// A bug the update cannot be fixing, and why.
struct Misfiled {
    bug_id: u64,
    summary: String,
    reason: String,
}

/// Drop bugs this update cannot be fixing.
///
/// A bug listed on an update that does not fix it will be closed
/// when the update goes stable. Catching it here is better than
/// voting -1 on it afterwards: nothing has been published yet, so
/// correcting the list costs nothing.
///
/// Two ways a bug fails to belong. Its package may not be among the
/// update's; only bugs that name their package outright are
/// considered for that (see `karma::bug_package`), since a stale
/// component on a CVE or FTBFS bug would otherwise drop it wrongly.
/// Or it may be a bot-filed report for *another release* — an
/// F46FailsToInstall listed on an F45 update, which the package
/// check cannot see because the package is built. That one is read
/// from the bots' own wording, which names the release outright, so
/// it carries none of the staleness the component does.
///
/// Under `--yes` this warns and keeps them: silently discarding a
/// bug the user passed to `--bug` is not something to do unasked.
async fn screen_bugs(
    bz: &sandogasa_bugzilla::BzClient,
    packages: &[String],
    branch: &str,
    bugs: &[u64],
    assume_yes: bool,
) -> Result<Vec<u64>, String> {
    let fetched = crate::karma::fetch_bugs(bz, bugs).await;
    let misfiled: Vec<Misfiled> = bugs
        .iter()
        .filter_map(|id| {
            let bug = fetched.get(id)?;
            let reason = misfiled_reason(bug, packages, branch)?;
            Some(Misfiled {
                bug_id: *id,
                summary: bug.summary.clone(),
                reason,
            })
        })
        .collect();
    if misfiled.is_empty() {
        return Ok(bugs.to_vec());
    }

    let shown: Vec<String> = misfiled
        .iter()
        .map(|b| format!("  #{} {}\n    {}", b.bug_id, b.summary, b.reason))
        .collect();
    eprintln!("\nThese bugs do not look like this update's to close:");
    for bug in &shown {
        eprintln!("{bug}");
    }
    if assume_yes {
        eprintln!(
            "keeping them under --yes; they would be closed by an update that does not fix them"
        );
        return Ok(bugs.to_vec());
    }
    let dropped: Vec<u64> = sandogasa_cli::confirm_each(
        "Leave these bugs off the update?",
        "Leave this bug off?",
        &shown,
        true,
    )
    .map_err(|e| e.to_string())?
    .into_iter()
    .map(|i| misfiled[i].bug_id)
    .collect();
    Ok(bugs
        .iter()
        .copied()
        .filter(|id| !dropped.contains(id))
        .collect())
}

/// Propose bugs the update looks like it closes, and return the ones
/// the user accepts.
///
/// Skipped entirely under `--yes`: attaching a bug means closing it
/// when the update goes stable, which is not something to do without
/// being asked. Each candidate is shown with how it was found.
async fn propose_bugs(
    bz: &sandogasa_bugzilla::BzClient,
    report: &crate::check_update::CheckUpdateReport,
    already_listed: &[u64],
    koji_profile: Option<&str>,
) -> Result<Vec<u64>, String> {
    // The check knows each package's old and new version-release,
    // which is what says how much of a changelog is new.
    let builds: Vec<(String, String)> = report
        .changes
        .iter()
        .map(|c| (c.package.clone(), c.new.clone()))
        .collect();
    if builds.is_empty() {
        return Ok(Vec::new());
    }
    let old_versions: std::collections::BTreeMap<String, Option<String>> = report
        .changes
        .iter()
        .map(|c| (c.package.clone(), c.old.clone()))
        .collect();
    let changelogs = builds
        .iter()
        .filter_map(|(pkg, evr)| {
            let nvr = format!("{pkg}-{evr}");
            sandogasa_koji::build_info_with_changelog(&nvr, koji_profile)
                .ok()
                .map(|text| (pkg.clone(), text))
        })
        .collect();

    let trackers =
        sandogasa_bugclass::bugzilla::lookup_branch_trackers(bz, report.dist_branch()).await;
    let facts = crate::karma::UpdateFacts {
        builds: &builds,
        trackers: &trackers,
        branch: report.dist_branch(),
        unsatisfied: &report.installability_issues,
        full_analysis: report.full_analysis,
    };
    let candidates =
        crate::discover::candidates(bz, &changelogs, &old_versions, &facts, already_listed).await;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let shown: Vec<String> = candidates
        .iter()
        .map(|c| format!("  #{} {}\n    {}", c.bug_id, c.summary, c.source))
        .collect();
    eprintln!("\nThis update looks like it closes:");
    for candidate in &shown {
        eprintln!("{candidate}");
    }
    Ok(sandogasa_cli::confirm_each(
        "Add these bugs to the update?",
        "Add this bug?",
        &shown,
        true,
    )
    .map_err(|e| e.to_string())?
    .into_iter()
    .map(|i| candidates[i].bug_id)
    .collect())
}

/// Whether the update may autopush, given what the operator asked
/// for and what RPM Fusion ships.
///
/// `--disable-autokarma` turns it off; so does a counterpart, and the
/// flag cannot turn it back on. The operator's switch is an override
/// in one direction only, because the other direction is the one that
/// breaks users' systems.
fn effective_autokarma(opts: &SubmitOptions) -> bool {
    opts.autokarma && opts.rpmfusion.is_empty()
}

/// Format the submission plan shown for confirmation. Leads with the
/// package list — the whole point of checking before submitting is
/// spotting a subpackage update that is accidentally missing one.
/// What the plan calls this submission.
///
/// A side tag names itself; a list of builds names the NVRs, since
/// the packages line below shows source names and the dist tag is
/// what says which release each update will be for.
fn source_label(source: &UpdateSource) -> String {
    match source {
        UpdateSource::FromTag(tag) => tag.clone(),
        UpdateSource::Builds(nvrs) => nvrs.join(", "),
    }
}

fn plan_text(tag: &str, packages: &[String], bugs: &[u64], opts: &SubmitOptions) -> String {
    use std::fmt::Write as _;
    let mut o = String::new();
    let _ = writeln!(o, "\nSubmission plan for {tag}:");
    let _ = writeln!(
        o,
        "  packages ({}): {}",
        packages.len(),
        packages.join(", ")
    );
    let _ = writeln!(
        o,
        "  type: {}, severity: {}",
        opts.update_type, opts.severity
    );
    if !bugs.is_empty() {
        let listed = bugs
            .iter()
            .map(|b| format!("#{b}"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(o, "  bugs (closed on stable): {listed}");
    }
    if effective_autokarma(opts) {
        let _ = writeln!(
            o,
            "  autopush at karma +{} (unpush at {})",
            opts.stable_karma, opts.unstable_karma
        );
    } else if opts.rpmfusion.is_empty() {
        let _ = writeln!(o, "  autopush: disabled");
    } else {
        // Name the counterparts rather than just disabling: a policy
        // the operator cannot see applied is one they cannot check.
        let pairs = opts
            .rpmfusion
            .iter()
            .map(|(fedora, rf)| format!("{fedora} -> {rf}"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            o,
            "  autopush: disabled — RPM Fusion ships a counterpart ({pairs}), \
             which needs rebuilding before this reaches stable"
        );
    }
    let mut lines = opts.notes.lines();
    let first = lines.next().unwrap_or("");
    let rest = lines.count();
    if rest == 0 {
        let _ = writeln!(o, "  notes: {first}");
    } else {
        let _ = writeln!(o, "  notes: {first} (+{rest} more lines)");
    }
    o
}

/// Print the submission plan and confirm (default yes).
fn confirm_plan(
    tag: &str,
    packages: &[String],
    bugs: &[u64],
    opts: &SubmitOptions,
) -> Result<bool, String> {
    eprint!("{}", plan_text(tag, packages, bugs, opts));
    sandogasa_cli::confirm("Submit this update?", true).map_err(|e| e.to_string())
}

/// Submit the side tag to Bodhi. `packages` are the update's source
/// packages (from the check report), shown in the confirmation plan.
/// Returns the created update alias(es) so the caller can post the
/// check report as a follow-up comment.
pub fn run(
    source: &UpdateSource,
    report: &crate::check_update::CheckUpdateReport,
    opts: &SubmitOptions,
) -> Result<Vec<String>, String> {
    let label = source_label(source);
    let packages = &report.updated_packages;
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let bz = sandogasa_bugzilla::BzClient::new("https://bugzilla.redhat.com");

    // Settle the bug list before the plan is shown, so what is
    // confirmed is what gets submitted: propose what the update looks
    // like it closes, then drop anything it cannot be closing.
    let mut bugs = opts.bugs.clone();
    if !opts.assume_yes {
        bugs.extend(rt.block_on(propose_bugs(
            &bz,
            report,
            &bugs,
            opts.koji_profile.as_deref(),
        ))?);
    }
    let bugs = if bugs.is_empty() {
        Vec::new()
    } else {
        rt.block_on(screen_bugs(
            &bz,
            packages,
            report.dist_branch(),
            &bugs,
            opts.assume_yes,
        ))?
    };

    if opts.assume_yes {
        // No prompt, but still show what is being submitted.
        eprint!("{}", plan_text(&label, packages, &bugs, opts));
    } else if !confirm_plan(&label, packages, &bugs, opts)? {
        return Err("aborted: update not submitted".to_string());
    }

    rt.block_on(async {
        // Refresh preemptively: the analysis may have run for long
        // enough that a token valid at the start is close to (or
        // past) expiry by the time we post.
        let http = crate::karma::http_client();
        let token =
            auth::cli_session_token_refreshed(&http, &auth::cli_cache_path(), auth::FEDORA_IDP)
                .await?;
        let client = BodhiClient::new()
            .with_token(token)
            .map_err(|e| e.to_string())?;
        let req = NewUpdateFromTag {
            source: source.clone(),
            notes: opts.notes.clone(),
            update_type: opts.update_type.clone(),
            severity: opts.severity.clone(),
            bugs: bugs.clone(),
            close_bugs: true,
            autokarma: effective_autokarma(opts),
            stable_karma: opts.stable_karma,
            unstable_karma: opts.unstable_karma,
        };
        let resp = client
            .new_update_from_tag(&req)
            .await
            .map_err(|e| e.to_string())?;
        for caveat in &resp.caveats {
            eprintln!("note from bodhi: {}", caveat.description);
        }
        let aliases: Vec<String> = resp.aliases().iter().map(|a| a.to_string()).collect();
        if aliases.is_empty() {
            eprintln!("submitted (Bodhi returned no update alias)");
        }
        for alias in &aliases {
            eprintln!("submitted: https://bodhi.fedoraproject.org/updates/{alias}");
        }
        Ok(aliases)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> SubmitOptions {
        SubmitOptions {
            notes: "Update uutils to 0.2.\n\nRebuilt against new uucore.".to_string(),
            update_type: "enhancement".to_string(),
            severity: "unspecified".to_string(),
            bugs: vec![2482250],
            autokarma: true,
            stable_karma: 3,
            unstable_karma: -3,
            assume_yes: false,
            koji_profile: None,
            rpmfusion: vec![],
        }
    }

    #[test]
    fn an_rpm_fusion_counterpart_disables_autopush_and_says_why() {
        // libheif pairs with RPM Fusion's libheif-freeworld: letting
        // this reach stable on karma would leave the pair broken for
        // anyone with both repositories enabled.
        let mut o = opts();
        o.rpmfusion = vec![("libheif".to_string(), "libheif-freeworld".to_string())];
        assert!(!effective_autokarma(&o));
        let text = plan_text("f43-build-side-1", &["libheif".to_string()], &[], &o);
        assert!(text.contains("autopush: disabled"), "{text}");
        assert!(text.contains("libheif -> libheif-freeworld"), "{text}");
        assert!(text.contains("needs rebuilding"), "{text}");
        // The operator's flag cannot switch it back on: the override
        // runs one way, because the other way is the one that breaks
        // users' systems.
        o.autokarma = true;
        assert!(!effective_autokarma(&o));
    }

    #[test]
    fn an_update_rpm_fusion_does_not_ship_keeps_autopush() {
        // The common case: no counterpart, nothing changes, and the
        // plan says what it always said.
        let o = opts();
        assert!(effective_autokarma(&o));
        let text = plan_text("f43-build-side-1", &["zlib".to_string()], &[], &o);
        assert!(text.contains("autopush at karma +3"), "{text}");
        assert!(!text.contains("RPM Fusion"), "{text}");
        // And --disable-autokarma still disables it on its own, with
        // no counterpart to explain.
        let mut off = opts();
        off.autokarma = false;
        let text = plan_text("f43-build-side-1", &["zlib".to_string()], &[], &off);
        assert!(text.contains("autopush: disabled"), "{text}");
        assert!(!text.contains("RPM Fusion"), "{text}");
    }

    #[test]
    fn resolve_notes_inline_wins() {
        assert_eq!(resolve_notes(Some("hi"), None).unwrap(), "hi");
    }

    #[test]
    fn resolve_notes_reads_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("notes.md");
        std::fs::write(&p, "Long description.\n").unwrap();
        assert_eq!(
            resolve_notes(None, Some(&p)).unwrap(),
            "Long description.\n"
        );
        let err = resolve_notes(None, Some(&dir.path().join("missing.md"))).unwrap_err();
        assert!(err.contains("missing.md"), "{err}");
    }

    #[test]
    fn resolve_notes_requires_nonempty() {
        let err = resolve_notes(None, None).unwrap_err();
        assert!(err.contains("--notes"), "{err}");
        let err = resolve_notes(Some("  \n"), None).unwrap_err();
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn plan_text_lists_packages_and_settings() {
        let o = opts();
        let text = plan_text(
            "epel9-build-side-133287",
            &["rust-uucore".to_string(), "uutils-coreutils".to_string()],
            &o.bugs,
            &o,
        );
        assert!(text.contains("epel9-build-side-133287"), "{text}");
        assert!(
            text.contains("packages (2): rust-uucore, uutils-coreutils"),
            "{text}"
        );
        assert!(
            text.contains("type: enhancement, severity: unspecified"),
            "{text}"
        );
        assert!(text.contains("bugs (closed on stable): #2482250"), "{text}");
        assert!(
            text.contains("autopush at karma +3 (unpush at -3)"),
            "{text}"
        );
        assert!(
            text.contains("notes: Update uutils to 0.2. (+2 more lines)"),
            "{text}"
        );
    }

    #[test]
    fn plan_text_disabled_autokarma_and_single_line_notes() {
        let mut o = opts();
        o.autokarma = false;
        o.bugs.clear();
        o.notes = "One-liner".to_string();
        let text = plan_text("f44-build-side-1", &["fish".to_string()], &o.bugs, &o);
        assert!(text.contains("autopush: disabled"), "{text}");
        assert!(!text.contains("bugs"), "{text}");
        assert!(text.contains("notes: One-liner\n"), "{text}");
    }

    /// A Bugzilla stub returning one bug per id.
    async fn bz_with(bugs: serde_json::Value) -> wiremock::MockServer {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .respond_with(ResponseTemplate::new(200).set_body_json(bugs))
            .mount(&server)
            .await;
        server
    }

    fn bz_bug(id: u64, summary: &str, component: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "summary": summary,
            "status": "NEW",
            "resolution": "",
            "product": "Fedora",
            "component": [component],
            "severity": "",
            "priority": "",
            "assigned_to": "",
            "creator": "",
            "creation_time": "2026-01-01T00:00:00Z",
            "last_change_time": "2026-01-01T00:00:00Z",
            "keywords": [],
            "blocks": [],
        })
    }

    #[test]
    fn a_bug_for_another_release_is_misfiled_though_its_package_is_built() {
        // The real pairing: one dist-git commit named every
        // release's FailsToInstall bug in its Fixes: trailer, so an
        // f46 bug rides along on an f45 update. The package check
        // cannot see it -- the package *is* built -- and submitting
        // it would close the f46 bug on an f45 push.
        let bug = crate::karma::BugFacts {
            summary: "F46FailsToInstall: gnome-shell-extension-frippery-applications-menu"
                .to_string(),
            component: Some("gnome-shell-frippery".to_string()),
            ..Default::default()
        };
        let packages = vec!["gnome-shell-frippery".to_string()];
        let reason = misfiled_reason(&bug, &packages, "f45").expect("f46 does not belong on f45");
        assert!(reason.contains("f46"), "{reason}");
        assert!(reason.contains("f45"), "{reason}");
        // The same bug on its own release belongs there.
        assert_eq!(misfiled_reason(&bug, &packages, "f46"), None);
    }

    #[test]
    fn a_package_the_update_does_not_build_is_named_before_any_release() {
        // Both could be true at once; the package is the stronger
        // claim and is what the user is told.
        let bug = crate::karma::BugFacts {
            summary: "zsh-5.9 is available".to_string(),
            component: Some("zsh".to_string()),
            ..Default::default()
        };
        let reason = misfiled_reason(&bug, &["rust-ctor".to_string()], "f45")
            .expect("zsh is not among the packages");
        assert!(reason.contains("no zsh"), "{reason}");
    }

    #[tokio::test]
    async fn screen_bugs_keeps_bugs_the_update_builds_for() {
        let server = bz_with(serde_json::json!({
            "bugs": [bz_bug(1, "rust-ctor-1.0.8 is available", "rust-ctor")]
        }))
        .await;
        let bz = sandogasa_bugzilla::BzClient::new(&server.uri());
        let kept = screen_bugs(&bz, &["rust-ctor".to_string()], "f44", &[1], true)
            .await
            .unwrap();
        assert_eq!(kept, vec![1]);
    }

    #[tokio::test]
    async fn screen_bugs_keeps_unclassified_bugs() {
        // A CVE names no package we can check against the builds, so
        // it is never a candidate for dropping.
        let server = bz_with(serde_json::json!({
            "bugs": [bz_bug(2, "CVE-2026-1234 rust-dtor: overflow", "rust-dtor")]
        }))
        .await;
        let bz = sandogasa_bugzilla::BzClient::new(&server.uri());
        let kept = screen_bugs(&bz, &["rust-ctor".to_string()], "f44", &[2], true)
            .await
            .unwrap();
        assert_eq!(kept, vec![2]);
    }

    #[tokio::test]
    async fn screen_bugs_warns_but_keeps_under_assume_yes() {
        // The update builds no rust-dtor, but dropping a bug the user
        // passed to --bug is not something to do without asking.
        let server = bz_with(serde_json::json!({
            "bugs": [bz_bug(3, "rust-dtor-1.0.5 is available", "rust-dtor")]
        }))
        .await;
        let bz = sandogasa_bugzilla::BzClient::new(&server.uri());
        let kept = screen_bugs(&bz, &["rust-ctor".to_string()], "f44", &[3], true)
            .await
            .unwrap();
        assert_eq!(kept, vec![3]);
    }

    #[tokio::test]
    async fn screen_bugs_keeps_everything_when_bugzilla_is_down() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let bz = sandogasa_bugzilla::BzClient::new(&server.uri());
        let kept = screen_bugs(&bz, &["rust-ctor".to_string()], "f44", &[4], false)
            .await
            .unwrap();
        assert_eq!(kept, vec![4], "a fetch failure must not drop bugs");
    }
}
