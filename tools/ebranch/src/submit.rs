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

use sandogasa_bodhi::models::NewUpdateFromTag;
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
    /// Push automatically at the karma thresholds.
    pub autokarma: bool,
    pub stable_karma: i32,
    pub unstable_karma: i32,
    /// Skip the submission confirmation.
    pub assume_yes: bool,
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

/// A bug the update cannot be fixing, and why.
struct Misfiled {
    bug_id: u64,
    summary: String,
    package: String,
}

/// Drop bugs whose package the update does not build.
///
/// A bug listed on an update that ships nothing for it will be
/// closed when the update goes stable, against a package the update
/// never touched. Catching it here is better than voting -1 on it
/// afterwards: nothing has been published yet, so correcting the
/// list costs nothing.
///
/// Only bugs that name their package outright are considered (see
/// `karma::bug_package`), so a CVE or FTBFS bug is never dropped.
/// Under `--yes` this warns and keeps them: silently discarding a
/// bug the user passed to `--bug` is not something to do unasked.
async fn screen_bugs(
    bz: &sandogasa_bugzilla::BzClient,
    packages: &[String],
    bugs: &[u64],
    assume_yes: bool,
) -> Result<Vec<u64>, String> {
    let fetched = crate::karma::fetch_bugs(bz, bugs).await;
    let misfiled: Vec<Misfiled> = bugs
        .iter()
        .filter_map(|id| {
            let bug = fetched.get(id)?;
            let package = crate::karma::bug_package(&bug.summary, bug.component.as_deref())?;
            (!packages.contains(&package)).then(|| Misfiled {
                bug_id: *id,
                summary: bug.summary.clone(),
                package,
            })
        })
        .collect();
    if misfiled.is_empty() {
        return Ok(bugs.to_vec());
    }

    eprintln!("\nThis update builds nothing for these bugs' packages:");
    for bug in &misfiled {
        eprintln!("  #{} {}", bug.bug_id, bug.summary);
        eprintln!("    no {} among the update's packages", bug.package);
    }
    if assume_yes {
        eprintln!(
            "keeping them under --yes; they would be closed by an update that does not fix them"
        );
        return Ok(bugs.to_vec());
    }
    if !sandogasa_cli::confirm("Leave these bugs off the update?", true)
        .map_err(|e| e.to_string())?
    {
        return Ok(bugs.to_vec());
    }
    let dropped: Vec<u64> = misfiled.iter().map(|b| b.bug_id).collect();
    Ok(bugs
        .iter()
        .copied()
        .filter(|id| !dropped.contains(id))
        .collect())
}

/// Format the submission plan shown for confirmation. Leads with the
/// package list — the whole point of checking before submitting is
/// spotting a subpackage update that is accidentally missing one.
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
    if opts.autokarma {
        let _ = writeln!(
            o,
            "  autopush at karma +{} (unpush at {})",
            opts.stable_karma, opts.unstable_karma
        );
    } else {
        let _ = writeln!(o, "  autopush: disabled");
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
pub fn run(tag: &str, packages: &[String], opts: &SubmitOptions) -> Result<Vec<String>, String> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;

    // Screen the bug list before the plan is shown, so what is
    // confirmed is what gets submitted.
    let bugs = if opts.bugs.is_empty() {
        Vec::new()
    } else {
        rt.block_on(screen_bugs(
            &sandogasa_bugzilla::BzClient::new("https://bugzilla.redhat.com"),
            packages,
            &opts.bugs,
            opts.assume_yes,
        ))?
    };

    if opts.assume_yes {
        // No prompt, but still show what is being submitted.
        eprint!("{}", plan_text(tag, packages, &bugs, opts));
    } else if !confirm_plan(tag, packages, &bugs, opts)? {
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
            from_tag: tag.to_string(),
            notes: opts.notes.clone(),
            update_type: opts.update_type.clone(),
            severity: opts.severity.clone(),
            bugs: bugs.clone(),
            close_bugs: true,
            autokarma: opts.autokarma,
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
        }
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

    #[tokio::test]
    async fn screen_bugs_keeps_bugs_the_update_builds_for() {
        let server = bz_with(serde_json::json!({
            "bugs": [bz_bug(1, "rust-ctor-1.0.8 is available", "rust-ctor")]
        }))
        .await;
        let bz = sandogasa_bugzilla::BzClient::new(&server.uri());
        let kept = screen_bugs(&bz, &["rust-ctor".to_string()], &[1], true)
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
        let kept = screen_bugs(&bz, &["rust-ctor".to_string()], &[2], true)
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
        let kept = screen_bugs(&bz, &["rust-ctor".to_string()], &[3], true)
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
        let kept = screen_bugs(&bz, &["rust-ctor".to_string()], &[4], false)
            .await
            .unwrap();
        assert_eq!(kept, vec![4], "a fetch failure must not drop bugs");
    }
}
