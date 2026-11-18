// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The package's git checkout, as the tracking-issue comments need it
//! ([`sandogasa_cbs::notify::Checkout`]): what a build was made from
//! against the merge request's head, fetching what the checkout lacks,
//! and which Hyperscale releases the package has a SIG branch for.

use std::path::Path;
use std::process::Command;

use sandogasa_cbs::notify::{Checkout, MergeRequest};
use sandogasa_cbs::tracking::Provenance;
use sandogasa_cbs::{Sig, Stage};

/// The checkout at `repo`.
pub struct GitCheckout<'a> {
    pub repo: &'a Path,
}

impl Checkout for GitCheckout<'_> {
    fn fetch_missing(&self, mr: &MergeRequest, commits: &[Option<String>]) {
        fetch_missing(self.repo, mr, commits);
    }

    fn provenance(&self, head: &str, commit: &str) -> Provenance {
        provenance(self.repo, head, commit)
    }

    fn release_tags(&self) -> Vec<String> {
        hyperscale_release_tags(self.repo)
    }
}

/// Fetch, once each, what the checkout at `repo` lacks to compare
/// `commits` with the merge request: its head from CentOS Stream, the
/// builds' commits from the checkout's remotes.
/// The preview needs them, so this runs before anything is confirmed;
/// it only updates remote-tracking refs, and says so, as it can take a
/// while.
fn fetch_missing(repo: &Path, mr: &MergeRequest, commits: &[Option<String>]) {
    if !has_commit(repo, &mr.head) {
        eprintln!("Fetching the merge request's head, to say what the build carries…");
        let _ = git(repo, &["fetch", "--quiet", &mr.repo_url, &mr.mr_ref]);
    }
    if commits.iter().flatten().any(|c| !has_commit(repo, c)) {
        eprintln!("Fetching the checkout's remotes, to say what the build carries…");
        let _ = git(repo, &["fetch", "--quiet", "--all"]);
    }
}

/// Whether the checkout at `repo` has `commit`.
fn has_commit(repo: &Path, commit: &str) -> bool {
    git(repo, &["cat-file", "-e", &format!("{commit}^{{commit}}")]).is_ok()
}

/// What `commit` is, against the merge request's `head`, in the checkout
/// at `repo`, from what it has: `Unknown`, with a warning, when it lacks
/// either, or when what it carries cannot be listed.
pub fn provenance(repo: &Path, head: &str, commit: &str) -> Provenance {
    if !(has_commit(repo, head) && has_commit(repo, commit)) {
        eprintln!(
            "warning: the checkout lacks {} or {}; the comment cannot say what the build carries",
            short(head),
            short(commit)
        );
        return Provenance::Unknown;
    }
    // A fixed length, not git's abbreviation, which grows with the repo
    // and differs between clones: an earlier comment's list of carried
    // commits is matched by its text.
    let id = git(repo, &["rev-parse", "--short=12", commit]).unwrap_or_else(|_| short(commit));
    let full = |c: &str| git(repo, &["rev-parse", c]).ok();
    if full(head) == full(commit) {
        return Provenance::Head(id);
    }
    if git(repo, &["merge-base", "--is-ancestor", head, commit]).is_err() {
        return Provenance::Apart(id);
    }
    match git(
        repo,
        &[
            "log",
            "--abbrev=12",
            "--format=%h %s",
            &format!("{head}..{commit}"),
        ],
    ) {
        Ok(carried) if !carried.is_empty() => {
            Provenance::Carried(id, carried.lines().map(str::to_string).collect())
        }
        _ => Provenance::Unknown,
    }
}

/// The first twelve characters of a commit id.
fn short(commit: &str) -> String {
    commit.chars().take(12).collect()
}

/// Run `git` in `repo` to read something, its output trimmed.
fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The `-release` tags of the Hyperscale collections the checkout at
/// `repo` has a SIG branch for, in its own branches or its remotes':
/// each branch's own collection (`c10s-sig-hyperscale` and `c9s-hs`
/// main, `c9s-hsx` experimental, `c9s-hs+fb` facebook), newest release
/// first; only releases the SIG still releases for and variants not
/// deprecated, so an EOL branch does not keep issues open.
fn hyperscale_release_tags(repo: &Path) -> Vec<String> {
    let refs = git(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname:lstrip=2)",
            "refs/heads",
            "refs/remotes",
        ],
    )
    .unwrap_or_default();
    // Each branch's own collection: c9s-hs builds into main, c9s-hsx
    // into experimental, and so on.
    let mut collections: Vec<sandogasa_cbs::Collection> = refs
        .lines()
        // `origin/c10s-sig-hyperscale` is the branch after the remote.
        .map(|r| r.rsplit('/').next().unwrap_or(r))
        .filter_map(|b| Sig::Hyperscale.collection_for_branch(b).ok())
        .filter(|c| sandogasa_cbs::HYPERSCALE_MAJORS.contains(&c.major))
        // A deprecated variant's old builds would keep issues open.
        .filter(|c| sandogasa_cbs::hyperscale_variant(&c.repository).is_none_or(|v| !v.deprecated))
        .collect();
    collections.sort_by(|a, b| b.major.cmp(&a.major).then(a.repository.cmp(&b.repository)));
    let mut tags: Vec<String> = collections.iter().map(|c| c.tag(Stage::Release)).collect();
    tags.dedup();
    tags
}

#[cfg(test)]
mod tests {
    use super::*;

    use sandogasa_cbs::notify::{Branch, plan};
    use sandogasa_cbs::tags::TagChange;

    /// The Hyperscale releases come from the checkout's SIG branches,
    /// its own or its remotes', of releases the SIG still releases for.
    #[test]
    fn the_release_tags_are_the_checkouts_hyperscale_branches() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let git = |args: &[&str]| crate::test_support::git(repo, args);
        git(&["init", "--quiet", "-b", "c10s-sig-hyperscale"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "Update to 7.1",
        ]);
        // A remote's branch counts, a mirrored Stream one does not.
        git(&["update-ref", "refs/remotes/origin/c9s-hs", "HEAD"]);
        git(&["branch", "c10s"]);
        git(&["branch", "c8s-sig-hyperscale"]);
        // Each branch's own collection, once: c9s-hsx is experimental.
        // A deprecated variant's branch does not count.
        git(&["branch", "c9s-hsx"]);
        git(&["branch", "c9s-hs+gnome"]);
        git(&["update-ref", "refs/remotes/origin/c9s-hsx", "HEAD"]);
        let r10 = "hyperscale10s-packages-main-release";
        let r9 = "hyperscale9s-packages-main-release";
        let x9 = "hyperscale9s-packages-experimental-release";
        assert_eq!(GitCheckout { repo }.release_tags(), [r10, x9, r9]);
        assert!(
            GitCheckout {
                repo: Path::new("/nonexistent")
            }
            .release_tags()
            .is_empty()
        );
    }

    #[test]
    fn a_builds_commit_is_the_head_carries_sig_only_commits_or_is_apart() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let git = |args: &[&str]| crate::test_support::git(repo, args);
        git(&["init", "--quiet"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "Update git",
        ]);
        let head = git(&["rev-parse", "HEAD"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "Pin Ruby 3 for CBS's el9 buildroot",
        ]);
        let pinned = git(&["rev-parse", "HEAD"]);
        let short_head = git(&["rev-parse", "--short=12", &head]);
        let short_pinned = git(&["rev-parse", "--short=12", &pinned]);
        assert_eq!(
            provenance(repo, &head, &head),
            Provenance::Head(short_head.clone())
        );
        assert_eq!(
            provenance(repo, &head, &pinned),
            Provenance::Carried(
                short_pinned.clone(),
                vec![format!("{short_pinned} Pin Ruby 3 for CBS's el9 buildroot")]
            )
        );
        assert_eq!(
            provenance(repo, &pinned, &head),
            Provenance::Apart(short_head)
        );
        assert_eq!(
            provenance(repo, &head, "0123456789abcdef0123456789abcdef01234567"),
            Provenance::Unknown
        );
    }

    /// git's c9s build with the Ruby pin on top of merge request !33,
    /// whose head the checkout has never fetched: it is fetched from
    /// CentOS Stream's merge request ref, and the comment lists the pin.
    #[test]
    fn a_comment_lists_the_sig_only_commits_the_build_carries() {
        let tmp = tempfile::tempdir().unwrap();
        let git = |dir: &Path, args: &[&str]| crate::test_support::git(dir, args);
        let commit = |dir: &Path, m: &str| {
            git(
                dir,
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "commit",
                    "--quiet",
                    "--allow-empty",
                    "-m",
                    m,
                ],
            );
            git(dir, &["rev-parse", "HEAD"])
        };
        // CentOS Stream's repo, with the merge request's head.
        let mut server = mockito::Server::new();
        let stream_root = tmp.path().join("stream");
        let stream = stream_root.join("redhat/centos-stream/rpms/git.git");
        std::fs::create_dir_all(&stream).unwrap();
        git(&stream, &["init", "--quiet", "--bare"]);
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        git(&work, &["init", "--quiet"]);
        let head = commit(&work, "Update git to 2.52.1");
        git(
            &work,
            &[
                "push",
                "--quiet",
                &stream.display().to_string(),
                "HEAD:refs/merge-requests/33/head",
            ],
        );
        // The checkout: the pin on top, without the head's ref.
        let pinned = commit(&work, "Pin Ruby 3 for CBS's el9 buildroot");
        let checkout = tmp.path().join("checkout");
        git(tmp.path(), &["init", "--quiet", "checkout"]);
        git(
            &checkout,
            &[
                "config",
                &format!("url.{}/.insteadOf", stream_root.display()),
                &format!("{}/", server.url()),
            ],
        );
        git(
            &checkout,
            &[
                "fetch",
                "--quiet",
                &work.display().to_string(),
                &format!("{pinned}:refs/heads/c9s-proposed-RHEL-306262"),
            ],
        );
        let url = server.url();
        let mr_url = format!("{url}/redhat/centos-stream/rpms/git/-/merge_requests/33");
        let serve_mr = |server: &mut mockito::Server, sha: &str| {
            server
                .mock(
                    "GET",
                    "/api/v4/projects/redhat%2Fcentos-stream%2Frpms%2Fgit/merge_requests/33",
                )
                .with_status(200)
                .with_body(
                    serde_json::json!({
                        "iid": 33, "title": "Update git to 2.52.1", "state": "opened",
                        "web_url": mr_url, "source_branch": "a", "target_branch": "c9s",
                        "sha": sha,
                    })
                    .to_string(),
                )
                .create()
        };
        let mr = serve_mr(&mut server, &head);
        let path = "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Fgit";
        server
            .mock("GET", format!("{path}/issues").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(
                serde_json::json!([{
                    "iid": 2, "title": "git c9s", "state": "opened", "web_url": "u2",
                    "labels": ["c9s", "cpu-sig-tracker"],
                    "description": format!("- **MR**: [Update git to 2.52.1]({mr_url}) — opened\n- **JIRA**: [RHEL-306262](j) — New"),
                }])
                .to_string(),
            )
            .create();
        server
            .mock("GET", format!("{path}/issues/2/notes").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body("[]")
            .create();
        server
            .mock("GET", format!("{path}/labels").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(r#"[{"name": "stage::testing"}]"#)
            .create();
        let nvr = "git-2.52.1-1~proposed.el9";
        let steps = [(
            nvr.to_string(),
            Stage::Testing,
            vec![TagChange::tag(
                "proposed_updates9s-packages-main-testing",
                nvr,
            )],
        )];
        let on = Branch {
            sig: Sig::ProposedUpdates,
            package: "git",
            branch: "c9s-proposed-RHEL-306262",
            checkout: &GitCheckout { repo: &checkout },
            dry_run: false,
            tool: "cbspkg",
        };
        let built = |nvrs: &[String]| Ok(vec![Some(pinned.clone()); nvrs.len()]);
        let notices = plan(&url, Some("t"), &on, &steps, &built);
        let note = &notices[0].note;
        assert!(
            note.contains("the merge request's head and these SIG-only commits"),
            "{note}"
        );
        assert!(
            note.contains("Pin Ruby 3 for CBS's el9 buildroot"),
            "{note}"
        );

        // The merge request moves on to a head only CentOS Stream has: it
        // is fetched from the merge request's ref, and the build, made
        // before, does not contain it.
        git(&work, &["checkout", "--quiet", "--detach", &head]);
        let moved = commit(&work, "Update git to 2.52.1, take 2");
        git(
            &work,
            &[
                "push",
                "--quiet",
                "--force",
                &stream.display().to_string(),
                "HEAD:refs/merge-requests/33/head",
            ],
        );
        assert!(!has_commit(&checkout, &moved));
        mr.remove();
        serve_mr(&mut server, &moved);
        let notices = plan(&url, Some("t"), &on, &steps, &built);
        assert!(has_commit(&checkout, &moved));
        let note = &notices[0].note;
        assert!(
            note.contains("does not contain the merge request's current head"),
            "{note}"
        );
        // A dry run fetches nothing: without the head, nothing is said.
        let fresh = tmp.path().join("fresh");
        git(tmp.path(), &["init", "--quiet", "fresh"]);
        let fresh_checkout = GitCheckout { repo: &fresh };
        let dry = Branch {
            checkout: &fresh_checkout,
            dry_run: true,
            ..on
        };
        let notices = plan(&url, Some("t"), &dry, &steps, &built);
        assert!(!notices[0].note.contains("Built from"));
    }
}
