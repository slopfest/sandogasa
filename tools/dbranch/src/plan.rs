// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Pure helpers that turn branch names and versions into the exact
//! commands dbranch runs (and shows under `--explain`). Kept separate
//! from execution so every command can be asserted in tests.

/// The Ubuntu codename for a branch: the segment after a `namespace/`
/// prefix, or the whole name when unprefixed. `ubuntu/questing` →
/// `questing`, `noble` → `noble`.
pub fn codename_from_branch(branch: &str) -> &str {
    branch.rsplit('/').next().unwrap_or(branch)
}

/// The gbp `debian-tag` format for a PPA branch: the branch's
/// namespace (the part before the last `/`, e.g. `ubuntu/questing` →
/// `ubuntu`) plus `/%(version)s`, so tags land under that namespace
/// (`ubuntu/<version>`) rather than gbp's default `debian/<version>`.
/// A branch with no namespace defaults to `ubuntu`.
pub fn debian_tag_format(branch: &str) -> String {
    let namespace = branch.rsplit_once('/').map_or("ubuntu", |(ns, _)| ns);
    format!("{namespace}/%(version)s")
}

/// The PPA target branches for a no-argument bulk run: every local
/// branch except those in `exclude` (the current Debian branch and
/// gbp's plumbing branches — `upstream-branch` and the pristine-tar
/// branch).
pub fn ppa_branches(all: &[String], exclude: &[String]) -> Vec<String> {
    all.iter()
        .filter(|b| !exclude.iter().any(|e| e == *b))
        .cloned()
        .collect()
}

/// Strip a Debian epoch (`N:`) for filename use — `.dsc`/`.changes`
/// names never carry the epoch.
pub fn version_no_epoch(version: &str) -> &str {
    match version.split_once(':') {
        Some((_, rest)) => rest,
        None => version,
    }
}

/// The source `.dsc` filename for a package at a version.
pub fn dsc_filename(package: &str, version: &str) -> String {
    format!("{package}_{}.dsc", version_no_epoch(version))
}

/// The source `.changes` filename `debuild -S` produces (in the parent
/// directory) — what the upload stage hands to `dput`.
pub fn changes_filename(package: &str, version: &str) -> String {
    format!("{package}_{}_source.changes", version_no_epoch(version))
}

/// Turn a PPA name into a dput target, tolerating a `ppa:` prefix:
/// `michel/sugarjar` and `ppa:michel/sugarjar` both → `ppa:michel/sugarjar`.
pub fn ppa_target(ppa: &str) -> String {
    format!("ppa:{}", ppa.strip_prefix("ppa:").unwrap_or(ppa))
}

/// pbuilder-dist's result directory for a codename
/// (`~/pbuilder/<codename>_result`); `None` if `$HOME` is unset.
pub fn pbuilder_result_dir(codename: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|home| {
        std::path::Path::new(&home)
            .join("pbuilder")
            .join(format!("{codename}_result"))
    })
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// `git checkout <branch>`.
pub fn checkout_argv(branch: &str) -> Vec<String> {
    argv(&["git", "checkout", branch])
}

/// `git checkout -b <branch> <start_point>` — create a new PPA branch
/// off the current Debian branch.
pub fn checkout_new_argv(branch: &str, start_point: &str) -> Vec<String> {
    argv(&["git", "checkout", "-b", branch, start_point])
}

/// `git merge --signoff --no-edit <source>` — merge the Debian branch
/// in; `--signoff` matches the merge commits in the damo history.
pub fn merge_argv(source: &str) -> Vec<String> {
    argv(&["git", "merge", "--signoff", "--no-edit", source])
}

/// `git add debian/changelog`.
pub fn add_changelog_argv() -> Vec<String> {
    argv(&["git", "add", "debian/changelog"])
}

/// `git commit -s --no-edit --cleanup=strip` — finalize a
/// conflict-resolved merge. `--cleanup=strip` drops the `# Conflicts:`
/// comment block git leaves in `MERGE_MSG` (with `--no-edit` the
/// default cleanup is `whitespace`, which would keep those `#` lines).
pub fn commit_merge_argv() -> Vec<String> {
    argv(&["git", "commit", "-s", "--no-edit", "--cleanup=strip"])
}

/// `git commit -s -m <message> debian/changelog`.
pub fn commit_changelog_argv(message: &str) -> Vec<String> {
    argv(&["git", "commit", "-s", "-m", message, "debian/changelog"])
}

/// `git commit -s -m <message> <file>` — commit a single file (used
/// for the one-time gbp.conf / salsa-ci.yml tweaks on a new branch).
pub fn commit_file_argv(message: &str, file: &str) -> Vec<String> {
    argv(&["git", "commit", "-s", "-m", message, file])
}

/// `gbp dch --bpo -R -D <codename> -U <urgency> --spawn-editor=never` —
/// create the finalized rebuild stanza (with the correct date/maintainer
/// footer). `-R`/`--release` would otherwise spawn an editor by default;
/// dbranch normalizes the entry afterward (preserving the `-U` urgency in
/// the header), so suppress it. `urgency` is usually `medium`.
pub fn gbp_dch_argv(codename: &str, urgency: &str) -> Vec<String> {
    argv(&[
        "gbp",
        "dch",
        "--bpo",
        "-R",
        "-D",
        codename,
        "-U",
        urgency,
        "--spawn-editor=never",
    ])
}

/// `gbp import-orig --uscan --pristine-tar --no-interactive` — pull the
/// new upstream release via uscan and import it onto the Debian branch,
/// recording the tarball with pristine-tar. The head of the `update`
/// flow. `--no-interactive` is essential: dbranch runs gbp with a null
/// stdin, so its "What is the upstream version?" prompt (raised when the
/// tarball version is ambiguous, e.g. a `0~`-mangled date version) would
/// otherwise hit `EOFError` and abort. With it, gbp uses its own guessed
/// version (taken from the uscan tarball name) without asking.
pub fn gbp_import_orig_argv() -> Vec<String> {
    argv(&[
        "gbp",
        "import-orig",
        "--uscan",
        "--pristine-tar",
        "--no-interactive",
    ])
}

/// Whether `gbp import-orig` failed only because the upstream is already
/// present — gbp aborts with `Upstream tag '<tag>' already exists` when
/// the version's tag is in the repo. Matching this (case-insensitive)
/// lets the `update` flow self-heal a run that imported the upstream but
/// died before writing the changelog: skip the import, regenerate the
/// changelog, rather than dead-ending. The match is deliberately scoped
/// to the *tag* message and **not** bare `already exists`, because a
/// failed download also reports `Failed to download …: … already exists`
/// — that is a real error we must not paper over.
pub fn import_already_done(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    lower.contains("upstream tag") && lower.contains("already exists")
}

/// `gbp dch -c -R -D unstable -U <urgency> --spawn-editor=never` —
/// generate, finalize, and commit the new-upstream changelog entry on
/// the Debian branch (`-c`/`--commit` commits it, `-R`/`--release`
/// finalizes the date). The distribution is pinned to `unstable`:
/// without `-D`, dch's release heuristic fills in the *host's*
/// distribution (e.g. an Ubuntu devel codename), which fails Debian CI.
/// `urgency` is usually `medium`, raised (e.g. `high`) for a security
/// upload. Not normalized — unlike a rebuild, this is a genuine
/// new-upstream entry.
pub fn gbp_dch_release_argv(urgency: &str) -> Vec<String> {
    argv(&[
        "gbp",
        "dch",
        "-c",
        "-R",
        "-D",
        "unstable",
        "-U",
        urgency,
        "--spawn-editor=never",
    ])
}

/// `debuild -S -sa -d` — build the source package.
pub fn debuild_argv() -> Vec<String> {
    argv(&["debuild", "-S", "-sa", "-d"])
}

/// `dh clean` — run the package's clean target, removing build cruft
/// like the `debian/files` `debuild -S` leaves, so the work tree is
/// clean for `gbp tag`.
pub fn dh_clean_argv() -> Vec<String> {
    argv(&["dh", "clean"])
}

/// `gbp tag` — tag the release on the current (Debian) branch, using
/// the version from `debian/changelog` and gbp's `debian-tag` format.
pub fn gbp_tag_argv() -> Vec<String> {
    argv(&["gbp", "tag"])
}

/// `pbuilder-dist <codename> ../<pkg>_<version>.dsc` — scratch-build
/// the source package in the codename's chroot.
pub fn pbuilder_argv(codename: &str, dsc_relpath: &str) -> Vec<String> {
    argv(&["pbuilder-dist", codename, dsc_relpath])
}

/// `pbuilder-dist <codename> create` — build the codename's base
/// chroot the first time (no `~/pbuilder/<codename>-base.tgz` yet).
pub fn pbuilder_create_argv(codename: &str) -> Vec<String> {
    argv(&["pbuilder-dist", codename, "create"])
}

/// `pbuilder-dist <codename> update` — refresh an existing base chroot
/// so the build isn't against stale packages.
pub fn pbuilder_update_argv(codename: &str) -> Vec<String> {
    argv(&["pbuilder-dist", codename, "update"])
}

/// Path to a codename's pbuilder base tarball
/// (`~/pbuilder/<codename>-base.tgz`); `None` if `$HOME` is unset.
pub fn pbuilder_base_tgz(codename: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|home| {
        std::path::Path::new(&home)
            .join("pbuilder")
            .join(format!("{codename}-base.tgz"))
    })
}

/// `lintian -I <target>...` — lint built artifacts. `-I` surfaces the
/// info-level (`I:`) tags too, not just warnings/errors.
pub fn lintian_argv(targets: &[String]) -> Vec<String> {
    let mut a = vec!["lintian".to_string(), "-I".to_string()];
    a.extend(targets.iter().cloned());
    a
}

/// `git push` — publish the checked-out branch to its already-set
/// upstream (the minimal command once tracking exists).
pub fn push_argv() -> Vec<String> {
    argv(&["git", "push"])
}

/// `git push -u <remote> <branch>` — publish the branch and set its
/// upstream. Used for the first push of a branch with no upstream yet
/// (you can't set tracking beforehand — the remote ref doesn't exist
/// until this push). Later pushes use the plain [`push_argv`].
pub fn push_set_upstream_argv(remote: &str, branch: &str) -> Vec<String> {
    argv(&["git", "push", "-u", remote, branch])
}

/// `dput [<target>] <changes>` — upload a `.changes` to its archive.
/// `Some(target)` is a dput host (e.g. `mentors`, `ftp-master`) or a
/// PPA (`ppa:<user>/<name>`, see [`ppa_target`]); `None` omits the
/// target so dput uses its configured default (the Debian archive) —
/// used by the Debian-branch `update` flow.
pub fn dput_argv(target: Option<&str>, changes: &str) -> Vec<String> {
    match target {
        Some(t) => argv(&["dput", t, changes]),
        None => argv(&["dput", changes]),
    }
}

/// `glab ci list --sha <sha> -F json` — list the CI pipeline(s) for an
/// exact commit, as JSON. dbranch polls this (see [`crate::rebuild`])
/// to watch the pipeline for the commit it just pushed: targeting the
/// SHA dodges the post-push race where `glab ci status -b <branch>`
/// would report the *previous* commit's pipeline (the new one not yet
/// created), and dodges `--live`, which needs a TTY and won't wait
/// unattended. glab finds the GitLab host/project from the git remote
/// itself (e.g. salsa.debian.org). Run with stdin on `/dev/null` (see
/// [`crate::ui::Ui::run_query`]).
pub fn glab_ci_list_sha_argv(sha: &str) -> Vec<String> {
    argv(&["glab", "ci", "list", "--sha", sha, "-F", "json"])
}

/// One CI pipeline's identity and state, parsed from glab's JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineInfo {
    pub id: i64,
    pub status: String,
    pub web_url: String,
}

/// The most recent pipeline from `glab ci list ... -F json` output
/// (the list is newest-first). `None` if the JSON is empty/unparseable
/// — i.e. no pipeline exists for the commit yet.
pub fn latest_pipeline(json: &str) -> Option<PipelineInfo> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let first = value.as_array()?.first()?;
    Some(PipelineInfo {
        id: first.get("id")?.as_i64()?,
        status: first.get("status")?.as_str()?.to_string(),
        web_url: first
            .get("web_url")
            .and_then(|u| u.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

/// `glab api projects/:id/pipelines/<id>/jobs?per_page=100` — list a
/// pipeline's jobs as JSON. `glab` substitutes `:id` with the current
/// repo and emits the raw API response; `per_page=100` avoids needing
/// pagination for any realistic pipeline. Used to report per-job
/// progress while watching (see [`crate::rebuild`]).
pub fn glab_pipeline_jobs_argv(pipeline_id: i64) -> Vec<String> {
    vec![
        "glab".to_string(),
        "api".to_string(),
        format!("projects/:id/pipelines/{pipeline_id}/jobs?per_page=100"),
    ]
}

/// One CI job's identity and state, parsed from glab's jobs JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobInfo {
    pub id: i64,
    pub name: String,
    pub stage: String,
    pub status: String,
}

/// Parse the jobs array from `glab api .../jobs`, sorted by id
/// (ascending ≈ stage/creation order, for readable progress output).
/// Empty on unparseable/empty input.
pub fn parse_jobs(json: &str) -> Vec<JobInfo> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(array) = value.as_array() else {
        return Vec::new();
    };
    let mut jobs: Vec<JobInfo> = array
        .iter()
        .filter_map(|j| {
            Some(JobInfo {
                id: j.get("id")?.as_i64()?,
                name: j.get("name")?.as_str()?.to_string(),
                stage: j
                    .get("stage")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
                status: j.get("status")?.as_str()?.to_string(),
            })
        })
        .collect();
    jobs.sort_by_key(|j| j.id);
    jobs
}

/// Whether a pipeline status is terminal (the pipeline has finished);
/// the complement of the in-progress states glab keeps polling
/// through. Mirrors GitLab's pipeline status vocabulary.
pub fn is_terminal_status(status: &str) -> bool {
    matches!(
        status,
        "success" | "failed" | "canceled" | "skipped" | "manual"
    )
}

/// `glab auth status --hostname <host>` — verify glab has a token for
/// the specific instance the repo lives on. glab stores a separate
/// token per host, so a bare `glab auth status` would pass on a
/// gitlab.com login even with no salsa.debian.org token; scoping to
/// `host` checks the one the CI commands will actually use.
pub fn glab_auth_status_argv(host: &str) -> Vec<String> {
    argv(&["glab", "auth", "status", "--hostname", host])
}

/// The host of a git remote URL — scp-like (`git@host:path`),
/// `ssh://[user@]host/path`, or `https://[user@]host/path` →
/// `host`. `None` if it can't be parsed.
pub fn host_from_remote_url(url: &str) -> Option<String> {
    // Drop any `scheme://` prefix; scp-like URLs have none.
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    // Drop `user@` (or `git@`) credentials before the host.
    let rest = rest.rsplit_once('@').map(|(_, r)| r).unwrap_or(rest);
    // The host runs up to the first `/` (path) or `:` (scp path/port).
    let host: String = rest
        .chars()
        .take_while(|c| *c != '/' && *c != ':')
        .collect();
    (!host.is_empty()).then_some(host)
}

/// The gbp-style commit subject for a changelog release commit.
pub fn changelog_commit_message(version: &str) -> String {
    format!("Update changelog for {version} release")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codename_strips_namespace() {
        assert_eq!(codename_from_branch("ubuntu/questing"), "questing");
        assert_eq!(codename_from_branch("noble"), "noble");
        assert_eq!(codename_from_branch("ubuntu/resolute"), "resolute");
    }

    #[test]
    fn import_already_done_matches_only_the_tag_message() {
        // The upstream-tag-exists abort: recoverable.
        assert!(import_already_done(
            "gbp:error: Upstream tag 'upstream/0_20260612' already exists"
        ));
        // Case-insensitive.
        assert!(import_already_done("UPSTREAM TAG 'x' ALREADY EXISTS\n"));
        // A failed download also says "already exists" but is a real
        // error — must NOT be treated as already-imported.
        assert!(!import_already_done(
            "gbp:error: Failed to download https://e/x.tar.gz: ../x.tar.gz already exists"
        ));
        // Other unrelated failures.
        assert!(!import_already_done(
            "gbp:error: uscan failed: no watch file"
        ));
        assert!(!import_already_done(""));
    }

    #[test]
    fn debian_tag_format_uses_branch_namespace() {
        assert_eq!(debian_tag_format("ubuntu/questing"), "ubuntu/%(version)s");
        // No namespace → default to `ubuntu`.
        assert_eq!(debian_tag_format("noble"), "ubuntu/%(version)s");
    }

    #[test]
    fn ppa_branches_excludes_listed() {
        let all: Vec<String> = [
            "master",
            "upstream",
            "pristine-tar",
            "noble",
            "ubuntu/questing",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let exclude: Vec<String> = ["master", "upstream", "pristine-tar"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            ppa_branches(&all, &exclude),
            vec!["noble", "ubuntu/questing"]
        );
    }

    #[test]
    fn dsc_filename_drops_epoch() {
        assert_eq!(
            dsc_filename("damo", "3.2.8-1~questing+1"),
            "damo_3.2.8-1~questing+1.dsc"
        );
        assert_eq!(
            dsc_filename("damo", "1:3.2.8-1~questing+1"),
            "damo_3.2.8-1~questing+1.dsc"
        );
    }

    #[test]
    fn changes_filename_and_dput_and_ppa() {
        assert_eq!(
            changes_filename("damo", "1:3.2.8-1~questing+1"),
            "damo_3.2.8-1~questing+1_source.changes"
        );
        assert_eq!(ppa_target("michel/sugarjar"), "ppa:michel/sugarjar");
        // A leading `ppa:` is tolerated, not doubled.
        assert_eq!(ppa_target("ppa:michel/sugarjar"), "ppa:michel/sugarjar");
        assert_eq!(
            dput_argv(Some("ppa:michel/sugarjar"), "../damo_1_source.changes"),
            ["dput", "ppa:michel/sugarjar", "../damo_1_source.changes"]
        );
        // No target → dput's configured default (Debian archive).
        assert_eq!(
            dput_argv(None, "../damo_1_source.changes"),
            ["dput", "../damo_1_source.changes"]
        );
    }

    #[test]
    fn command_builders_match_the_real_commands() {
        assert_eq!(checkout_argv("noble"), ["git", "checkout", "noble"]);
        assert_eq!(
            checkout_new_argv("ubuntu/plucky", "debian/unstable"),
            ["git", "checkout", "-b", "ubuntu/plucky", "debian/unstable"]
        );
        assert_eq!(
            merge_argv("master"),
            ["git", "merge", "--signoff", "--no-edit", "master"]
        );
        // The merge commit strips git's `# Conflicts:` comment block.
        assert_eq!(
            commit_merge_argv(),
            ["git", "commit", "-s", "--no-edit", "--cleanup=strip"]
        );
        assert_eq!(
            gbp_dch_argv("questing", "medium"),
            [
                "gbp",
                "dch",
                "--bpo",
                "-R",
                "-D",
                "questing",
                "-U",
                "medium",
                "--spawn-editor=never"
            ]
        );
        assert_eq!(debuild_argv(), ["debuild", "-S", "-sa", "-d"]);
        assert_eq!(dh_clean_argv(), ["dh", "clean"]);
        assert_eq!(gbp_tag_argv(), ["gbp", "tag"]);
        assert_eq!(
            gbp_import_orig_argv(),
            [
                "gbp",
                "import-orig",
                "--uscan",
                "--pristine-tar",
                "--no-interactive"
            ]
        );
        assert_eq!(
            gbp_dch_release_argv("high"),
            [
                "gbp",
                "dch",
                "-c",
                "-R",
                "-D",
                "unstable",
                "-U",
                "high",
                "--spawn-editor=never"
            ]
        );
        assert_eq!(
            pbuilder_argv("questing", "../damo_3.2.8-1~questing+1.dsc"),
            [
                "pbuilder-dist",
                "questing",
                "../damo_3.2.8-1~questing+1.dsc"
            ]
        );
        assert_eq!(
            pbuilder_create_argv("questing"),
            ["pbuilder-dist", "questing", "create"]
        );
        assert_eq!(
            pbuilder_update_argv("questing"),
            ["pbuilder-dist", "questing", "update"]
        );
        assert_eq!(
            lintian_argv(&["/r/damo_3.2.8-1~questing+1_arm64.deb".to_string()]),
            ["lintian", "-I", "/r/damo_3.2.8-1~questing+1_arm64.deb"]
        );
        assert_eq!(
            changelog_commit_message("3.2.8-1~questing+1"),
            "Update changelog for 3.2.8-1~questing+1 release"
        );
        assert_eq!(
            commit_file_argv("Adjust gbp.conf for noble", "debian/gbp.conf"),
            [
                "git",
                "commit",
                "-s",
                "-m",
                "Adjust gbp.conf for noble",
                "debian/gbp.conf"
            ]
        );
        assert_eq!(push_argv(), ["git", "push"]);
        assert_eq!(
            push_set_upstream_argv("origin", "noble"),
            ["git", "push", "-u", "origin", "noble"]
        );
        assert_eq!(
            glab_ci_list_sha_argv("ea4102c"),
            ["glab", "ci", "list", "--sha", "ea4102c", "-F", "json"]
        );
        assert_eq!(
            glab_auth_status_argv("salsa.debian.org"),
            ["glab", "auth", "status", "--hostname", "salsa.debian.org"]
        );
    }

    #[test]
    fn latest_pipeline_parses_newest_first() {
        let json = r#"[
            {"id": 1111431, "status": "running",
             "web_url": "https://salsa.debian.org/x/-/pipelines/1111431",
             "sha": "ea4102c40f70ec2f7c1df38624b19818d7b1363e"},
            {"id": 1106046, "status": "success",
             "web_url": "https://salsa.debian.org/x/-/pipelines/1106046",
             "sha": "270aea27409e80c6592a93f0e81234cd32180306"}
        ]"#;
        let p = latest_pipeline(json).unwrap();
        assert_eq!(p.id, 1111431);
        assert_eq!(p.status, "running");
        assert_eq!(p.web_url, "https://salsa.debian.org/x/-/pipelines/1111431");
        // Empty list (no pipeline yet) / junk → None.
        assert_eq!(latest_pipeline("[]"), None);
        assert_eq!(latest_pipeline("not json"), None);
    }

    #[test]
    fn parse_jobs_extracts_and_sorts_by_id() {
        let json = r#"[
            {"id": 20, "name": "lintian", "stage": "test", "status": "running"},
            {"id": 18, "name": "build source", "stage": "build", "status": "success"}
        ]"#;
        let jobs = parse_jobs(json);
        assert_eq!(jobs.len(), 2);
        // Sorted ascending by id.
        assert_eq!(jobs[0].name, "build source");
        assert_eq!(jobs[0].stage, "build");
        assert_eq!(jobs[0].status, "success");
        assert_eq!(jobs[1].name, "lintian");
        assert_eq!(jobs[1].status, "running");
        assert!(parse_jobs("not json").is_empty());
        assert!(parse_jobs("{}").is_empty());
    }

    #[test]
    fn glab_pipeline_jobs_argv_targets_current_repo() {
        assert_eq!(
            glab_pipeline_jobs_argv(1111431),
            [
                "glab",
                "api",
                "projects/:id/pipelines/1111431/jobs?per_page=100"
            ]
        );
    }

    #[test]
    fn terminal_status_covers_finished_states() {
        for s in ["success", "failed", "canceled", "skipped", "manual"] {
            assert!(is_terminal_status(s), "{s} should be terminal");
        }
        for s in ["created", "pending", "running", "preparing", "scheduled"] {
            assert!(!is_terminal_status(s), "{s} should be in-progress");
        }
    }

    #[test]
    fn host_from_remote_url_parses_each_form() {
        assert_eq!(
            host_from_remote_url("git@salsa.debian.org:python-team/packages/damo.git").as_deref(),
            Some("salsa.debian.org")
        );
        assert_eq!(
            host_from_remote_url("ssh://git@salsa.debian.org/python-team/packages/damo.git")
                .as_deref(),
            Some("salsa.debian.org")
        );
        assert_eq!(
            host_from_remote_url("https://salsa.debian.org/python-team/packages/damo.git")
                .as_deref(),
            Some("salsa.debian.org")
        );
        assert_eq!(
            host_from_remote_url("https://user@gitlab.com/foo/bar.git").as_deref(),
            Some("gitlab.com")
        );
        assert_eq!(host_from_remote_url("").as_deref(), None);
    }
}
