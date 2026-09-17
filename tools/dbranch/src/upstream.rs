// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Packaging straight from upstream's git tags — gbp's "when upstream
//! uses git, no tarballs" flow. The Debian branch descends from an
//! upstream release tag; a new release is `git merge <tag>`; and the
//! orig tarball is generated from the tag (`gbp export-orig
//! --pristine-tar-commit`) rather than imported from a download.
//! `clone` sets a repository up this way, and `update` detects the
//! layout (an upstream remote plus gbp.conf's `upstream-tag`) and
//! merges instead of running `gbp import-orig`.

use std::path::{Path, PathBuf};

use crate::ui::Ui;
use crate::{gbpconf, git, plan};

/// A repository packaged from upstream's git: the remote holding
/// upstream and the gbp `upstream-tag` format naming its release tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamGit {
    pub remote: String,
    pub tag_format: String,
}

/// Inputs for `clone`.
pub struct CloneOptions {
    /// Upstream's git URL.
    pub url: String,
    /// Directory to clone into; `None` uses the repository's name.
    pub dir: Option<PathBuf>,
    /// Upstream release to start from; `None` takes the newest tag.
    pub upstream_version: Option<String>,
    /// The Debian packaging branch to create (`debian/latest`).
    pub debian_branch: String,
    /// Name for the remote holding upstream's git (`upstream`).
    pub upstream_remote: String,
}

/// gbp's `upstream-tag` format for a tag's style: `v1.2` →
/// `v%(version)s`, `1.2` → `%(version)s`; `None` for a tag that does
/// not look like a version.
pub fn tag_format_for(tag: &str) -> Option<&'static str> {
    let versionish = |s: &str| s.starts_with(|c: char| c.is_ascii_digit());
    match tag.strip_prefix('v') {
        Some(rest) if versionish(rest) => Some("v%(version)s"),
        _ if versionish(tag) => Some("%(version)s"),
        _ => None,
    }
}

/// The `git tag --list` glob matching an `upstream-tag` format's tags:
/// `v%(version)s` → `v[0-9]*`. `None` for a format without a plain
/// `%(version)s` placeholder (gbp's character-mangling variants are
/// not supported here).
pub fn tag_glob(format: &str) -> Option<String> {
    let (pre, post) = format.split_once("%(version)s")?;
    Some(format!("{pre}[0-9]*{post}"))
}

/// The version a tag carries under `format`: `v1.2` under
/// `v%(version)s` → `1.2`; `None` when the tag does not fit.
pub fn version_of_tag(format: &str, tag: &str) -> Option<String> {
    let (pre, post) = format.split_once("%(version)s")?;
    let v = tag.strip_prefix(pre)?.strip_suffix(post)?;
    (!v.is_empty()).then(|| v.to_string())
}

/// The tag naming `version` under `format`.
pub fn tag_of_version(format: &str, version: &str) -> String {
    format.replace("%(version)s", version)
}

/// Detect the upstream-git layout: a remote named `remote` (default
/// `upstream`) plus gbp.conf's `upstream-tag`. `Ok(None)` when there is
/// no such remote — the tarball flow applies. An error when the remote
/// exists (or was named explicitly) but the tag format is missing or
/// unsupported, since the release tags could not be named.
pub fn detect(
    repo: &Path,
    remote: Option<&str>,
) -> Result<Option<UpstreamGit>, Box<dyn std::error::Error>> {
    let name = remote.unwrap_or("upstream");
    if !git::remotes(repo).iter().any(|r| r == name) {
        return match remote {
            Some(_) => Err(format!("no git remote named {name}").into()),
            None => Ok(None),
        };
    }
    let Some(tag_format) = gbpconf::read_repo(repo).upstream_tag else {
        return Err(format!(
            "remote {name} holds upstream's git, but debian/gbp.conf sets no \
             upstream-tag (e.g. `upstream-tag = v%(version)s`), so its release \
             tags cannot be named"
        )
        .into());
    };
    if tag_glob(&tag_format).is_none() {
        return Err(format!(
            "upstream-tag = {tag_format}: only a plain %(version)s placeholder is supported"
        )
        .into());
    }
    Ok(Some(UpstreamGit {
        remote: name.to_string(),
        tag_format,
    }))
}

/// The release tag to use: `want` under `format` (which must exist),
/// else the newest tag by version order. Without a `format` the newest
/// version-looking tag decides both the tag style and the version.
/// Returns `(tag, version, format)`.
pub fn release_tag(
    repo: &Path,
    format: Option<&str>,
    want: Option<&str>,
) -> Result<(String, String, String), Box<dyn std::error::Error>> {
    let glob = format
        .map(tag_glob)
        .map(|g| g.ok_or("unsupported upstream-tag"));
    let tags = git::tags_by_version(repo, glob.transpose()?.as_deref());
    let (tag, format) = match format {
        Some(f) => (tags.into_iter().next(), f.to_string()),
        None => {
            let found = tags
                .into_iter()
                .find_map(|t| tag_format_for(&t).map(|f| (t, f)));
            match found {
                Some((t, f)) => (Some(t), f.to_string()),
                None => (None, String::new()),
            }
        }
    };
    if let Some(v) = want {
        let tag = tag_of_version(&format, v);
        if !git::rev_parse(repo, &tag).is_some() {
            return Err(format!("no tag {tag} for upstream version {v}").into());
        }
        return Ok((tag, v.to_string(), format));
    }
    let tag = tag.ok_or("no version-like tags found; pass --upstream-version")?;
    let version = version_of_tag(&format, &tag).ok_or("tag does not carry a version")?;
    Ok((tag, version, format))
}

/// `clone`: clone upstream with itself as remote `upstream_remote`,
/// start the Debian branch at the chosen release tag, and commit a
/// gbp.conf naming the tag style (`upstream-tag`) with pristine-tar
/// enabled — the layout `update` detects. Writing the rest of
/// `debian/` is the packager's job and is printed as the next step.
pub fn clone(ui: &Ui, opts: &CloneOptions) -> Result<(), Box<dyn std::error::Error>> {
    let dir = opts
        .dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(plan::repo_name_from_url(&opts.url)));
    if dir.exists() {
        return Err(format!("{} already exists", dir.display()).into());
    }
    ui.step(&format!(
        "Clone {} with upstream as remote {}",
        opts.url, opts.upstream_remote
    ));
    let cwd = Path::new(".");
    ui.run_required(
        &plan::git_clone_argv(&opts.url, &opts.upstream_remote, &dir.to_string_lossy()),
        cwd,
    )?;
    // The tags are only known once the clone exists; a dry run
    // narrates with placeholders.
    let (tag, version, format) = if ui.dry_run {
        let f = "v%(version)s".to_string();
        ("<tag>".to_string(), "<version>".to_string(), f)
    } else {
        release_tag(&dir, None, opts.upstream_version.as_deref())?
    };
    ui.step(&format!(
        "Start {} at upstream {version} (tag {tag})",
        opts.debian_branch
    ));
    ui.run_required(&plan::checkout_new_argv(&opts.debian_branch, &tag), &dir)?;
    ui.step("Create gbp.conf: package from upstream's git tags");
    let text = gbpconf::upstream_git_config(&opts.debian_branch, &format);
    crate::rebuild::create_packaging_file(
        ui,
        &dir,
        "gbp.conf",
        "packaging from upstream's git tags",
        &text,
    )?;
    eprintln!(
        "Next: write the rest of debian/ in {} (dh_make or by hand), add the \
         packaging remote (git remote add origin <url>), and for later releases \
         run `dbranch update` there.",
        dir.display()
    );
    Ok(())
}

/// The import stage for an upstream-git repository: fetch upstream's
/// tags, pick the release, merge its tag into the Debian branch and
/// write the `<version>-1` changelog entry. A tag already merged (a
/// re-run after a failure further on) is noted and skipped.
pub fn merge_release(
    ui: &Ui,
    repo: &Path,
    up: &UpstreamGit,
    want: Option<&str>,
    urgency: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let (tag, version) = fetch_and_merge(ui, repo, up, want)?;
    ui.step(&format!("Generate the changelog entry for {version}-1"));
    ui.run_required(
        &plan::gbp_dch_new_upstream_argv(&format!("{version}-1"), urgency),
        repo,
    )?;
    let _ = tag;
    Ok(())
}

/// Fetch upstream's tags and merge the chosen release tag, returning
/// `(tag, version)`. Split from [`merge_release`] so the git half can
/// be exercised without gbp.
pub fn fetch_and_merge(
    ui: &Ui,
    repo: &Path,
    up: &UpstreamGit,
    want: Option<&str>,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    ui.step(&format!("Fetch upstream's tags from {}", up.remote));
    ui.run_required(&plan::git_fetch_tags_argv(&up.remote), repo)?;
    let (tag, version) = if ui.dry_run {
        ("<tag>".to_string(), "<version>".to_string())
    } else {
        let (t, v, _) = release_tag(repo, Some(&up.tag_format), want)?;
        (t, v)
    };
    if !ui.dry_run && git::is_ancestor(repo, &tag, "HEAD") {
        eprintln!("note: {tag} is already merged; skipping the merge");
        return Ok((tag, version));
    }
    ui.step(&format!(
        "Merge upstream {version} (tag {tag}) into the Debian branch"
    ));
    ui.run_required(&plan::merge_argv(&tag), repo)?;
    Ok((tag, version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(p: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(p)
            .env("GIT_AUTHOR_NAME", "T")
            .env("GIT_AUTHOR_EMAIL", "t@x")
            .env("GIT_COMMITTER_NAME", "T")
            .env("GIT_COMMITTER_EMAIL", "t@x")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    /// An "upstream" repo with releases tagged `v0.1.0`, `v0.2.0`,
    /// `v0.10.0` (version order, not lexical) and a non-release tag.
    fn upstream() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        git(p, &["init", "-q", "-b", "main"]);
        for (i, v) in ["0.1.0", "0.2.0", "0.10.0"].iter().enumerate() {
            std::fs::write(p.join("VERSION"), v).unwrap();
            git(p, &["add", "-A"]);
            git(
                p,
                &[
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "-qm",
                    &format!("release {v}"),
                ],
            );
            git(p, &["tag", &format!("v{v}")]);
            if i == 0 {
                git(p, &["tag", "some-milestone"]);
            }
        }
        dir
    }

    fn ui() -> Ui {
        Ui {
            explain: false,
            dry_run: false,
            quiet: true,
        }
    }

    #[test]
    fn tag_style_helpers() {
        assert_eq!(tag_format_for("v1.2.3"), Some("v%(version)s"));
        assert_eq!(tag_format_for("0.3.1"), Some("%(version)s"));
        assert_eq!(tag_format_for("release-1"), None);
        assert_eq!(tag_format_for("v"), None);
        assert_eq!(tag_glob("v%(version)s").as_deref(), Some("v[0-9]*"));
        assert_eq!(
            tag_glob("upstream/%(version)s").as_deref(),
            Some("upstream/[0-9]*")
        );
        assert_eq!(tag_glob("v%(version%~%.)s"), None);
        assert_eq!(
            version_of_tag("v%(version)s", "v1.2").as_deref(),
            Some("1.2")
        );
        assert_eq!(version_of_tag("v%(version)s", "1.2"), None);
        assert_eq!(version_of_tag("v%(version)s", "v"), None);
        assert_eq!(tag_of_version("%(version)s", "0.3.1"), "0.3.1");
    }

    #[test]
    fn release_tag_picks_newest_by_version_order() {
        let up = upstream();
        let p = up.path();
        // No format known: the newest version-like tag decides the style.
        let (tag, version, format) = release_tag(p, None, None).unwrap();
        assert_eq!(
            (tag.as_str(), version.as_str(), format.as_str()),
            ("v0.10.0", "0.10.0", "v%(version)s")
        );
        // A known format scopes the listing; an explicit version must exist.
        let (tag, version, _) = release_tag(p, Some("v%(version)s"), Some("0.2.0")).unwrap();
        assert_eq!((tag.as_str(), version.as_str()), ("v0.2.0", "0.2.0"));
        assert!(release_tag(p, Some("v%(version)s"), Some("9.9")).is_err());
        // A repo with no version-like tags asks for --upstream-version.
        let bare = tempfile::tempdir().unwrap();
        git(bare.path(), &["init", "-q"]);
        assert!(release_tag(bare.path(), None, None).is_err());
    }

    #[test]
    fn clone_sets_up_the_packaging_repo() {
        let up = upstream();
        let work = tempfile::tempdir().unwrap();
        let dir = work.path().join("pkg");
        let opts = CloneOptions {
            url: up.path().to_string_lossy().into_owned(),
            dir: Some(dir.clone()),
            upstream_version: None,
            debian_branch: "debian/latest".to_string(),
            upstream_remote: "upstream".to_string(),
        };
        // Real commits need an identity the fixture cannot pre-set in a
        // clone; git picks it up from the environment.
        // SAFETY: tests in this module run single-threaded per process
        // for env access only via Command, so set for the child instead.
        let ui = ui();
        // Route the identity through the repo config after the clone by
        // running clone with HOME-independent config via GIT_CONFIG_*.
        unsafe {
            std::env::set_var("GIT_CONFIG_COUNT", "3");
            std::env::set_var("GIT_CONFIG_KEY_0", "user.name");
            std::env::set_var("GIT_CONFIG_VALUE_0", "T");
            std::env::set_var("GIT_CONFIG_KEY_1", "user.email");
            std::env::set_var("GIT_CONFIG_VALUE_1", "t@x");
            std::env::set_var("GIT_CONFIG_KEY_2", "commit.gpgsign");
            std::env::set_var("GIT_CONFIG_VALUE_2", "false");
        }
        clone(&ui, &opts).unwrap();
        assert_eq!(git::current_branch(&dir).unwrap(), "debian/latest");
        assert_eq!(git::remotes(&dir), ["upstream"]);
        assert!(git::is_ancestor(&dir, "v0.10.0", "HEAD"));
        let conf = std::fs::read_to_string(dir.join("debian/gbp.conf")).unwrap();
        assert!(conf.contains("upstream-tag = v%(version)s"), "{conf}");
        assert!(conf.contains("debian-branch = debian/latest"), "{conf}");
        assert!(conf.contains("pristine-tar-commit = True"), "{conf}");
        // Committed, and the tree is clean.
        let status = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(status.stdout.is_empty());
        // Refuses to clobber an existing directory.
        assert!(clone(&ui, &opts).is_err());

        // The layout is detected, and a later release merges in.
        let up_git = detect(&dir, None).unwrap().unwrap();
        assert_eq!(up_git.tag_format, "v%(version)s");
        std::fs::write(up.path().join("VERSION"), "0.11.0").unwrap();
        git(up.path(), &["add", "-A"]);
        git(
            up.path(),
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "release 0.11.0",
            ],
        );
        git(up.path(), &["tag", "v0.11.0"]);
        let (tag, version) = fetch_and_merge(&ui, &dir, &up_git, None).unwrap();
        assert_eq!((tag.as_str(), version.as_str()), ("v0.11.0", "0.11.0"));
        assert!(git::is_ancestor(&dir, "v0.11.0", "HEAD"));
        assert_eq!(
            std::fs::read_to_string(dir.join("VERSION")).unwrap(),
            "0.11.0"
        );
        // Re-running notes the merge is done rather than failing.
        fetch_and_merge(&ui, &dir, &up_git, None).unwrap();
    }

    #[test]
    fn detect_needs_remote_and_tag_format() {
        let up = upstream();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        git(p, &["init", "-q", "-b", "debian/latest"]);
        // No upstream remote: the tarball flow.
        assert_eq!(detect(p, None).unwrap(), None);
        assert!(detect(p, Some("upstream")).is_err());
        git(
            p,
            &["remote", "add", "upstream", &up.path().to_string_lossy()],
        );
        // Remote but no upstream-tag: an error naming the fix.
        let err = detect(p, None).unwrap_err().to_string();
        assert!(err.contains("upstream-tag"), "{err}");
        std::fs::create_dir_all(p.join("debian")).unwrap();
        std::fs::write(
            p.join("debian/gbp.conf"),
            "[DEFAULT]\nupstream-tag = %(version)s\n",
        )
        .unwrap();
        assert_eq!(
            detect(p, None).unwrap(),
            Some(UpstreamGit {
                remote: "upstream".to_string(),
                tag_format: "%(version)s".to_string(),
            })
        );
        // A differently named remote works when named.
        git(p, &["remote", "rename", "upstream", "src"]);
        assert_eq!(detect(p, None).unwrap(), None);
        assert_eq!(detect(p, Some("src")).unwrap().unwrap().remote, "src");
        // Mangling formats are refused.
        std::fs::write(
            p.join("debian/gbp.conf"),
            "[DEFAULT]\nupstream-tag = v%(version%~%.)s\n",
        )
        .unwrap();
        assert!(detect(p, Some("src")).is_err());
    }

    #[test]
    fn dry_run_narrates_without_a_clone() {
        let ui = Ui {
            explain: false,
            dry_run: true,
            quiet: true,
        };
        let work = tempfile::tempdir().unwrap();
        let opts = CloneOptions {
            url: "https://git.example/proj/thing.git".to_string(),
            dir: Some(work.path().join("thing")),
            upstream_version: None,
            debian_branch: "debian/latest".to_string(),
            upstream_remote: "upstream".to_string(),
        };
        clone(&ui, &opts).unwrap();
        assert!(!work.path().join("thing").exists());
        let up = UpstreamGit {
            remote: "upstream".to_string(),
            tag_format: "v%(version)s".to_string(),
        };
        merge_release(&ui, work.path(), &up, None, "medium").unwrap();
    }
}
