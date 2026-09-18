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
use crate::{changelog, gbpconf, git, plan};

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
    /// When upstream itself carries a `debian/*` branch: `Some(true)`
    /// starts from it, `Some(false)` ignores it, `None` asks (a
    /// non-interactive run starts fresh with a warning).
    pub packaging: Option<bool>,
    /// Create the packaging project on salsa under this namespace and
    /// add it as `origin` (nothing is pushed: the first push is
    /// `update --stage push`, which also adds the CI file).
    pub salsa: Option<String>,
    /// Register the clone with myrepos (`mr config`).
    pub mr: bool,
    /// The mrconfig to register in; `None` is `~/.mrconfig`.
    pub mrconfig: Option<PathBuf>,
}

/// The GitLab instance `--salsa` creates projects on.
const SALSA: &str = "salsa.debian.org";

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
/// enabled — the layout `update` detects. When upstream itself carries
/// a `debian/*` branch (antifennel's author keeps one), offer to start
/// from it instead — their packaging commits stay in the history, so
/// diverging is ordinary commits and their later changes can still be
/// merged — with the release tag merged in and gbp.conf's keys filled
/// in where missing. Writing (or reviewing) `debian/` is the packager's
/// job and is printed as the next step, with the `dh_make` command for
/// a package that has none anywhere.
pub fn clone(ui: &Ui, opts: &CloneOptions) -> Result<(), Box<dyn std::error::Error>> {
    let dir = opts
        .dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(plan::repo_name_from_url(&opts.url)));
    if dir.exists() {
        return Err(format!("{} already exists", dir.display()).into());
    }
    // Cheap preconditions before the clone: the tools the optional
    // steps need, and glab's token for salsa.
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or("the clone directory has no name")?;
    let mrconfig = match (&opts.mr, &opts.mrconfig) {
        (false, _) => None,
        (true, Some(p)) => Some(p.clone()),
        (true, None) => Some(
            dirs::home_dir()
                .ok_or("cannot locate ~/.mrconfig: $HOME is unset; pass --mrconfig <path>")?
                .join(".mrconfig"),
        ),
    };
    if !ui.dry_run {
        if opts.mr && !sandogasa_cli::tool_exists("mr") {
            return Err("mr not found; install myrepos (or drop --mr)".into());
        }
        if opts.salsa.is_some() {
            git::ensure_tools(false, false, false, true, false, false, false)?;
            git::ensure_glab_auth(Path::new("."), SALSA)?;
        }
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
    if !ui.dry_run && git::tree_has_path(&dir, &tag, "debian") {
        eprintln!(
            "warning: the tree at {tag} carries a debian/ directory. dpkg-source drops \
             the orig tarball's copy when unpacking, so the branch's debian/ wins, but \
             the two will disagree; ask upstream to keep packaging on a branch instead"
        );
    }
    let theirs = if ui.dry_run {
        eprintln!("    (if upstream carries a debian/* branch, offers to start from it)");
        None
    } else {
        upstream_packaging(
            ui,
            &dir,
            &opts.upstream_remote,
            &opts.debian_branch,
            opts.packaging,
        )
    };
    let from_upstream = theirs.is_some();
    let review = match theirs {
        Some(branch) => {
            let start = format!("{}/{branch}", opts.upstream_remote);
            ui.step(&format!(
                "Start {} from upstream's {start} (not tracking it)",
                opts.debian_branch
            ));
            ui.run_required(
                &plan::checkout_new_no_track_argv(&opts.debian_branch, &start),
                &dir,
            )?;
            if git::is_ancestor(&dir, &tag, "HEAD") {
                eprintln!("note: {tag} is already part of that branch");
            } else {
                ui.step(&format!("Merge upstream {version} (tag {tag}) into it"));
                ui.run_required(&plan::merge_argv(&tag), &dir)?;
            }
            complete_gbp_conf(ui, &dir, &opts.debian_branch, &format)?;
            "review debian/ — it is upstream's packaging (Maintainer, changelog), and \
             any change is an ordinary commit here"
                .to_string()
        }
        None => {
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
            format!(
                "write the rest of debian/ — with no packaging anywhere, `{}` makes the \
                 skeleton (its class, license and copyright answers are yours; the \
                 provisional orig it creates is replaced by the first `gbp export-orig`)",
                plan::dh_make_argv(&name.to_lowercase(), &version).join(" ")
            )
        }
    };
    let origin = match &opts.salsa {
        Some(namespace) => Some(publish_to_salsa(ui, &dir, namespace, &name)?),
        None => None,
    };
    if let Some(mrconfig) = &mrconfig {
        register_mr(
            ui,
            &dir,
            &name,
            mrconfig,
            opts,
            origin.as_deref(),
            from_upstream,
        )?;
    }
    let remote = match origin {
        Some(_) => String::new(),
        None => ", add the packaging remote (git remote add origin <url>)".to_string(),
    };
    eprintln!(
        "Next, in {}: {review}{remote}, then `dbranch update --stage push` for the first \
         push (it adds debian/salsa-ci.yml so CI runs); later releases: `dbranch update`.",
        dir.display()
    );
    Ok(())
}

/// Create `<namespace>/<name>` on salsa — public, with the CI config
/// path preset to `debian/salsa-ci.yml` so the first push carrying that
/// file runs a pipeline — and add it as `origin`. Nothing is pushed: a
/// fresh branch holds only a gbp.conf, an adopted upstream packaging
/// wants a look first, and neither carries a salsa-ci.yml yet. The
/// first push is `update --stage push`, which adds that file, so it
/// runs CI (GitLab makes the first pushed branch the default). Returns
/// the project's ssh URL.
fn publish_to_salsa(
    ui: &Ui,
    dir: &Path,
    namespace: &str,
    name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    ui.step(&format!(
        "Create {SALSA}/{namespace}/{name} and make it origin"
    ));
    let lookup = plan::glab_namespace_argv(SALSA, namespace);
    ui.show_command(&lookup);
    let ssh_url = if ui.dry_run {
        ui.show_command(&plan::glab_create_project_argv(
            SALSA,
            name,
            "<namespace-id>",
            plan::SALSA_CI_PATH,
        ));
        format!("git@{SALSA}:{namespace}/{name}.git")
    } else {
        let (code, out, err) = ui.run_query(&lookup, dir)?;
        if code != 0 {
            let msg = if err.trim().is_empty() { out } else { err };
            return Err(format!("`glab api namespaces` failed: {}", msg.trim()).into());
        }
        let id = plan::namespace_id(&out, namespace)
            .ok_or_else(|| format!("no namespace {namespace} on {SALSA} (or no access to it)"))?;
        let create =
            plan::glab_create_project_argv(SALSA, name, &id.to_string(), plan::SALSA_CI_PATH);
        let (code, out) = ui.run_capture(&create, dir)?;
        if code != 0 {
            return Err(format!("creating the project failed: {}", out.trim()).into());
        }
        plan::project_ssh_url(&out)
            .ok_or_else(|| format!("unexpected reply creating the project: {}", out.trim()))?
    };
    ui.run_required(&plan::git_remote_add_argv("origin", &ssh_url), dir)?;
    Ok(ssh_url)
}

/// Register the clone with myrepos: `mr -c <mrconfig> config <section>
/// checkout=… update=…`, the section being the directory relative to
/// the mrconfig's own directory (myrepos' convention), with the
/// commands from [`mr_commands`]. Runs in the clone's parent directory.
fn register_mr(
    ui: &Ui,
    dir: &Path,
    name: &str,
    mrconfig: &Path,
    opts: &CloneOptions,
    origin: Option<&str>,
    from_upstream: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let abs = std::path::absolute(dir)?;
    let section = mr_section(mrconfig, &abs);
    let (checkout, update) = mr_commands(opts, name, origin, from_upstream);
    ui.step(&format!(
        "Register {section} with myrepos in {}",
        mrconfig.display()
    ));
    let parent = abs.parent().unwrap_or(Path::new("."));
    let cwd = if parent.is_dir() {
        parent
    } else {
        Path::new(".")
    };
    ui.run_required(
        &plan::mr_config_argv(&mrconfig.to_string_lossy(), &section, &checkout, &update),
        cwd,
    )
}

/// The mrconfig section for `dir`: its path relative to the mrconfig's
/// directory when under it (`src/debian/pkgs/thing` for `~/.mrconfig`),
/// else the absolute path, which myrepos also accepts.
fn mr_section(mrconfig: &Path, dir: &Path) -> String {
    let base = mrconfig.parent().unwrap_or(Path::new("/"));
    let base = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
    let dir_c = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    dir_c
        .strip_prefix(&base)
        .map(|rel| rel.to_string_lossy().into_owned())
        .unwrap_or_else(|_| dir_c.to_string_lossy().into_owned())
}

/// The myrepos checkout and update commands for a clone. With a salsa
/// `origin`: `gbp clone --all` of it, then upstream added and its tags
/// fetched (the style of the user's other packaging entries), and
/// `gbp pull` plus a tag fetch to update. Without one, only upstream
/// exists, so the checkout is the `dbranch clone` invocation that
/// reproduces this setup and the update a tag fetch.
fn mr_commands(
    opts: &CloneOptions,
    name: &str,
    origin: Option<&str>,
    from_upstream: bool,
) -> (String, String) {
    let up = &opts.upstream_remote;
    let fetch = format!("git fetch --tags {up}");
    match origin {
        Some(url) => (
            format!(
                "gbp clone --all {url} {name} && cd {name} && git remote add {up} {} && {fetch}",
                opts.url
            ),
            format!("gbp pull && {fetch}"),
        ),
        None => {
            let mut flags = vec![if from_upstream {
                "--from-upstream-packaging".to_string()
            } else {
                "--fresh".to_string()
            }];
            if opts.debian_branch != "debian/latest" {
                flags.push(format!("--debian-branch {}", opts.debian_branch));
            }
            if *up != "upstream" {
                flags.push(format!("--upstream-remote {up}"));
            }
            if let Some(v) = &opts.upstream_version {
                flags.push(format!("--upstream-version {v}"));
            }
            (
                format!("dbranch clone {} {} {name}", flags.join(" "), opts.url),
                fetch,
            )
        }
    }
}

/// Upstream's own packaging branch to start from, if any: a `debian/*`
/// branch on `remote` (the one named like ours first), described by its
/// last author and changelog version, and accepted per `choice` —
/// `Some(true)` takes it, `Some(false)` ignores it, `None` asks
/// (default yes) when interactive and otherwise warns and starts fresh.
fn upstream_packaging(
    ui: &Ui,
    repo: &Path,
    remote: &str,
    debian_branch: &str,
    choice: Option<bool>,
) -> Option<String> {
    let branches: Vec<String> = git::remote_branches(repo, remote)
        .into_iter()
        .filter(|b| b.starts_with("debian/"))
        .collect();
    let branch = branches
        .iter()
        .find(|b| *b == debian_branch)
        .or(branches.first())?
        .clone();
    if choice == Some(false) {
        eprintln!("note: upstream carries a {branch} branch; --fresh ignores it");
        return None;
    }
    let rev = format!("{remote}/{branch}");
    let by = git::commit_author_date(repo, &rev).unwrap_or_default();
    let version = git::show_file(repo, &rev, "debian/changelog")
        .and_then(|t| changelog::stanza_headers(&t).into_iter().next())
        .map(|h| h.version)
        .unwrap_or_else(|| "no changelog".to_string());
    let what =
        format!("upstream carries a {branch} branch — packaging by {by}, changelog {version}");
    if choice == Some(true) {
        eprintln!("{what}; starting from it");
        return Some(branch);
    }
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        eprintln!(
            "warning: {what}; starting fresh (pass --from-upstream-packaging to start from it)"
        );
        return None;
    }
    ui.confirm(&format!("{what} — start from it?"))
        .then_some(branch)
}

/// Make upstream's gbp.conf serve this branch: `debian-branch` set to
/// ours, and `upstream-tag`, `pristine-tar` and `pristine-tar-commit`
/// added when absent (an existing `upstream-tag` is theirs to keep).
/// Created outright when their branch has none. Committed if changed.
fn complete_gbp_conf(
    ui: &Ui,
    repo: &Path,
    debian_branch: &str,
    tag_format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let rel = "debian/gbp.conf";
    if !repo.join(rel).exists() {
        ui.step("Create gbp.conf: package from upstream's git tags");
        let text = gbpconf::upstream_git_config(debian_branch, tag_format);
        return crate::rebuild::create_packaging_file(
            ui,
            repo,
            "gbp.conf",
            "packaging from upstream's git tags",
            &text,
        );
    }
    ui.step(&format!("Adjust gbp.conf for {debian_branch}"));
    let changed = crate::rebuild::edit_file(ui, repo, rel, |text| {
        let cfg = gbpconf::parse(text);
        let mut t = gbpconf::set_key(text, "debian-branch", debian_branch, None);
        if cfg.upstream_tag.is_none() {
            t = gbpconf::set_key(&t, "upstream-tag", tag_format, Some("debian-branch"));
        }
        if cfg.pristine_tar.is_none() {
            t = gbpconf::set_key(&t, "pristine-tar", "True", None);
        }
        if !t
            .lines()
            .any(|l| l.trim_start().starts_with("pristine-tar-commit"))
        {
            t = gbpconf::set_key(&t, "pristine-tar-commit", "True", Some("pristine-tar"));
        }
        Some(t)
    })?;
    if changed {
        ui.explain_diff(repo, &[rel]);
        ui.run_required(
            &plan::commit_file_argv(&format!("Adjust gbp.conf for {debian_branch}"), rel),
            repo,
        )?;
    }
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
            packaging: Some(false),
            salsa: None,
            mr: false,
            mrconfig: None,
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

    /// Give the upstream fixture a `debian/latest` packaging branch of
    /// its own, based on the v0.2.0 commit (before the newest tag), with
    /// a gbp.conf missing the pristine-tar keys and a changelog.
    fn add_upstream_packaging(up: &Path) {
        git(up, &["checkout", "-q", "-b", "debian/latest", "v0.2.0"]);
        std::fs::create_dir_all(up.join("debian")).unwrap();
        std::fs::write(
            up.join("debian/gbp.conf"),
            "[DEFAULT]\ndebian-branch = debian/latest\nupstream-tag = v%(version)s\n",
        )
        .unwrap();
        std::fs::write(
            up.join("debian/changelog"),
            "thing (0.2.0-1) unstable; urgency=medium\n\n  * Initial release.\n\n \
             -- P <p@x>  Thu, 19 Sep 2024 19:34:54 -0700\n",
        )
        .unwrap();
        git(up, &["add", "-A"]);
        git(
            up,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "Initial packaging.",
            ],
        );
        git(up, &["checkout", "-q", "main"]);
    }

    #[test]
    fn clone_can_start_from_upstreams_packaging_branch() {
        let up = upstream();
        add_upstream_packaging(up.path());
        let theirs = git::rev_parse(up.path(), "debian/latest").unwrap();
        let work = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("GIT_CONFIG_COUNT", "3");
            std::env::set_var("GIT_CONFIG_KEY_0", "user.name");
            std::env::set_var("GIT_CONFIG_VALUE_0", "T");
            std::env::set_var("GIT_CONFIG_KEY_1", "user.email");
            std::env::set_var("GIT_CONFIG_VALUE_1", "t@x");
            std::env::set_var("GIT_CONFIG_KEY_2", "commit.gpgsign");
            std::env::set_var("GIT_CONFIG_VALUE_2", "false");
        }
        let opts = |dir: &str, packaging: Option<bool>| CloneOptions {
            url: up.path().to_string_lossy().into_owned(),
            dir: Some(work.path().join(dir)),
            upstream_version: None,
            debian_branch: "debian/latest".to_string(),
            upstream_remote: "upstream".to_string(),
            packaging,
            salsa: None,
            mr: false,
            mrconfig: None,
        };
        // From upstream's packaging: their commit and the newest tag are
        // both in our history, the branch tracks nothing (upstream must
        // not become the push remote), and gbp.conf gained the missing
        // pristine-tar keys while keeping their upstream-tag.
        let dir = work.path().join("from");
        clone(&ui(), &opts("from", Some(true))).unwrap();
        assert!(git::is_ancestor(&dir, &theirs, "HEAD"));
        assert!(git::is_ancestor(&dir, "v0.10.0", "HEAD"));
        assert_eq!(git::branch_remote(&dir, "debian/latest"), None);
        let conf = std::fs::read_to_string(dir.join("debian/gbp.conf")).unwrap();
        assert!(conf.contains("upstream-tag = v%(version)s"), "{conf}");
        assert!(conf.contains("pristine-tar-commit = True"), "{conf}");
        let changelog = std::fs::read_to_string(dir.join("debian/changelog")).unwrap();
        assert!(changelog.contains("0.2.0-1"));
        let status = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(status.stdout.is_empty());
        // --fresh ignores it: a plain start at the tag.
        let dir = work.path().join("fresh");
        clone(&ui(), &opts("fresh", Some(false))).unwrap();
        assert!(!git::is_ancestor(&dir, &theirs, "HEAD"));
        assert!(!dir.join("debian/changelog").exists());
    }

    #[test]
    fn mr_section_is_relative_to_the_mrconfig_dir() {
        let home = tempfile::tempdir().unwrap();
        let mrconfig = home.path().join(".mrconfig");
        let dir = home.path().join("src/debian/pkgs/thing");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(mr_section(&mrconfig, &dir), "src/debian/pkgs/thing");
        // Outside the mrconfig's tree: absolute.
        let elsewhere = tempfile::tempdir().unwrap();
        let section = mr_section(&mrconfig, elsewhere.path());
        assert!(Path::new(&section).is_absolute(), "{section}");
    }

    #[test]
    fn mr_commands_follow_the_two_layouts() {
        let opts = CloneOptions {
            url: "https://git.sr.ht/~technomancy/antifennel".to_string(),
            dir: None,
            upstream_version: None,
            debian_branch: "debian/latest".to_string(),
            upstream_remote: "upstream".to_string(),
            packaging: None,
            salsa: None,
            mr: true,
            mrconfig: None,
        };
        let (co, up) = mr_commands(
            &opts,
            "antifennel",
            Some("git@salsa.debian.org:michel/antifennel.git"),
            true,
        );
        assert_eq!(
            co,
            "gbp clone --all git@salsa.debian.org:michel/antifennel.git antifennel && \
             cd antifennel && git remote add upstream \
             https://git.sr.ht/~technomancy/antifennel && git fetch --tags upstream"
        );
        assert_eq!(up, "gbp pull && git fetch --tags upstream");
        // No salsa project yet: the dbranch invocation that reproduces it.
        let (co, up) = mr_commands(&opts, "antifennel", None, true);
        assert_eq!(
            co,
            "dbranch clone --from-upstream-packaging \
             https://git.sr.ht/~technomancy/antifennel antifennel"
        );
        assert_eq!(up, "git fetch --tags upstream");
        let custom = CloneOptions {
            debian_branch: "debian/sid".to_string(),
            upstream_remote: "src".to_string(),
            upstream_version: Some("0.3.0".to_string()),
            ..opts
        };
        let (co, up) = mr_commands(&custom, "antifennel", None, false);
        assert_eq!(
            co,
            "dbranch clone --fresh --debian-branch debian/sid --upstream-remote src \
             --upstream-version 0.3.0 https://git.sr.ht/~technomancy/antifennel antifennel"
        );
        assert_eq!(up, "git fetch --tags src");
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
            packaging: None,
            // Both optional steps narrate under --dry-run without glab or mr.
            salsa: Some("michel".to_string()),
            mr: true,
            mrconfig: Some(work.path().join(".mrconfig")),
        };
        clone(&ui, &opts).unwrap();
        assert!(!work.path().join("thing").exists());
        assert!(!work.path().join(".mrconfig").exists());
        let up = UpstreamGit {
            remote: "upstream".to_string(),
            tag_format: "v%(version)s".to_string(),
        };
        merge_release(&ui, work.path(), &up, None, "medium").unwrap();
    }
}
