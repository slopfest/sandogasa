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

/// Split a dput target into a Launchpad `(owner, ppa_name)` when it's a
/// PPA target (`ppa:owner/name` → `("owner", "name")`); `None` for a
/// plain dput host (`mentors`, …) or the default target, which have no
/// Launchpad PPA to pre-check.
pub fn ppa_owner_name(target: &str) -> Option<(&str, &str)> {
    target.strip_prefix("ppa:")?.split_once('/')
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

/// `git add <file>` — stage a newly created file so it can be shown with
/// `git diff --cached` and committed (a brand-new file isn't picked up by
/// a bare `git commit <file>`).
pub fn git_add_argv(file: &str) -> Vec<String> {
    argv(&["git", "add", file])
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

/// `gbp dch --stable -R -U <urgency> --spawn-editor=never` — the
/// proposed-update analogue of [`gbp_dch_argv`] for a Debian stable
/// branch (`debian/<codename>`). `--stable` produces a `+deb<N>u<M>`
/// stable entry; dbranch then normalizes it — rewriting the version to
/// the `~deb<N>u<M>` form (so it sorts *older* than the plain build) and
/// synthesizing the body — so the exact distribution/number gbp picks
/// doesn't matter, and no `-D` is needed (`--stable` targets the stable
/// suite itself).
pub fn gbp_dch_stable_argv(urgency: &str) -> Vec<String> {
    argv(&[
        "gbp",
        "dch",
        "--stable",
        "-R",
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

/// `debuild -S {-sa|-si} -d` — build the source package. The `-s`
/// option decides what the `.changes` *offers for upload*, not what the
/// `.dsc` references (that always names the orig tarball): `-sa` forces
/// the orig tarball into the upload, `-si` — dpkg's default — includes
/// it only when the upstream version differs from the previous
/// changelog entry's. Both are passed explicitly so the narrated
/// command records which one this run decided on.
pub fn debuild_argv(include_orig: bool) -> Vec<String> {
    let source = if include_orig { "-sa" } else { "-si" };
    argv(&["debuild", "-S", source, "-d"])
}

/// Whether a `.changes` file offers an orig tarball for upload. Matches
/// any mention of a `.orig.tar` file, which is loose — the `Changes:`
/// field could in principle name one in prose. That bias is deliberate:
/// a false positive leaves the upload alone, while a false negative
/// would nag about rebuilding a source package that is already right.
pub fn changes_includes_orig(contents: &str) -> bool {
    contents.contains(".orig.tar")
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

/// [`gbp_dch_release_argv`] plus `-N <version>`: the new-upstream entry
/// for a release merged from upstream's git, where the version comes
/// from the tag rather than a tarball name.
pub fn gbp_dch_new_upstream_argv(version: &str, urgency: &str) -> Vec<String> {
    let mut a = gbp_dch_release_argv(urgency);
    a.extend(["-N".to_string(), version.to_string()]);
    a
}

/// `gbp export-orig --pristine-tar --pristine-tar-commit` — generate the
/// orig tarball from the upstream tag (`upstream-tag` in gbp.conf) into
/// `..` and record it on the pristine-tar branch, for a package built
/// from upstream's git rather than a downloaded tarball.
pub fn gbp_export_orig_argv() -> Vec<String> {
    argv(&[
        "gbp",
        "export-orig",
        "--pristine-tar",
        "--pristine-tar-commit",
    ])
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

/// `git clone --no-checkout -o <remote> <url> <dir>` — clone upstream
/// with itself as remote `remote` (gbp's `-o upstream` convention), not
/// checked out yet so the Debian branch can be started at a tag.
pub fn git_clone_argv(url: &str, remote: &str, dir: &str) -> Vec<String> {
    argv(&["git", "clone", "--no-checkout", "-o", remote, url, dir])
}

/// `git checkout --no-track -b <branch> <start>` — start a branch from
/// a remote-tracking ref *without* tracking it: upstream's packaging
/// branch is a starting point, not where ours is pushed.
pub fn checkout_new_no_track_argv(branch: &str, start_point: &str) -> Vec<String> {
    argv(&["git", "checkout", "--no-track", "-b", branch, start_point])
}

/// `dh_make -p <name>_<version> --createorig` — the skeleton step for a
/// package with no packaging anywhere, printed rather than run (its
/// class, license and copyright answers are the packager's).
/// `--createorig` makes the provisional orig tarball dh_make wants;
/// the first `gbp export-orig` replaces it with the tag-derived one.
pub fn dh_make_argv(name: &str, version: &str) -> Vec<String> {
    let pkg = format!("{name}_{version}");
    argv(&["dh_make", "-p", &pkg, "--createorig"])
}

/// `git remote add <name> <url>`.
pub fn git_remote_add_argv(name: &str, url: &str) -> Vec<String> {
    argv(&["git", "remote", "add", name, url])
}

/// `glab api --hostname <host> namespaces?search=<ns>` — look a
/// namespace (user or group) up by path, for its id.
pub fn glab_namespace_argv(host: &str, namespace: &str) -> Vec<String> {
    argv(&[
        "glab",
        "api",
        "--hostname",
        host,
        &format!("namespaces?search={namespace}"),
    ])
}

/// The id of the namespace whose `full_path` is exactly `namespace` in
/// a `namespaces?search=` listing (the search is a substring match).
pub fn namespace_id(json: &str, namespace: &str) -> Option<i64> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    v.as_array()?
        .iter()
        .find(|n| n.get("full_path").and_then(|p| p.as_str()) == Some(namespace))
        .and_then(|n| n.get("id")?.as_i64())
}

/// `glab api --hostname <host> -X POST projects -f name=… -f path=…
/// -f namespace_id=… -f visibility=public -f ci_config_path=…` —
/// create a public project with its CI config path preset, so the
/// first push of a branch carrying that file already runs a pipeline.
pub fn glab_create_project_argv(
    host: &str,
    name: &str,
    namespace_id: &str,
    ci_config_path: &str,
) -> Vec<String> {
    argv(&[
        "glab",
        "api",
        "--hostname",
        host,
        "-X",
        "POST",
        "projects",
        "-f",
        &format!("name={name}"),
        "-f",
        &format!("path={name}"),
        "-f",
        &format!("namespace_id={namespace_id}"),
        "-f",
        "visibility=public",
        "-f",
        &format!("ci_config_path={ci_config_path}"),
    ])
}

/// A created project's `ssh_url_to_repo` from the API's JSON.
pub fn project_ssh_url(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    Some(v.get("ssh_url_to_repo")?.as_str()?.to_string())
}

/// `mr -c <mrconfig> config <section> checkout=<cmd> update=<cmd>` —
/// register a repository with myrepos. `section` is the directory
/// relative to the mrconfig's own directory.
pub fn mr_config_argv(mrconfig: &str, section: &str, checkout: &str, update: &str) -> Vec<String> {
    argv(&[
        "mr",
        "-c",
        mrconfig,
        "config",
        section,
        &format!("checkout={checkout}"),
        &format!("update={update}"),
    ])
}

/// `git push <remote> tag <tag>` — push one tag by name (the explicit
/// form, so a branch of the same name is never meant).
pub fn push_tag_argv(remote: &str, tag: &str) -> Vec<String> {
    argv(&["git", "push", remote, "tag", tag])
}

/// The upstream part of a Debian version: no epoch, no revision
/// (`1:0.3.1-1` → `0.3.1`; a native `0.3.1` stays).
pub fn upstream_version(version: &str) -> &str {
    let v = version.split_once(':').map(|(_, v)| v).unwrap_or(version);
    v.rsplit_once('-').map(|(u, _)| u).unwrap_or(v)
}

/// `git fetch --tags <remote>` — bring in upstream's release tags.
pub fn git_fetch_tags_argv(remote: &str) -> Vec<String> {
    argv(&["git", "fetch", "--tags", remote])
}

/// The repository name a git URL clones into by default: the last path
/// segment without `.git` (`https://h/g/thing.git` → `thing`,
/// `git@h:g/thing` → `thing`, `/tmp/thing.git` → `thing`).
pub fn repo_name_from_url(url: &str) -> String {
    let last = url
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(url);
    last.strip_suffix(".git").unwrap_or(last).to_string()
}

/// `git config <key> <value>` — e.g. record a new branch's chosen
/// remote as `branch.<b>.pushRemote` so later stages need not ask.
pub fn git_config_argv(key: &str, value: &str) -> Vec<String> {
    argv(&["git", "config", key, value])
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

/// The dput host for Debusine uploads. Its dput-ng profile ships in
/// debusine-client (`/usr/share/dput-ng/profiles/debusine.debian.net.json`).
pub const DEBUSINE_HOST: &str = "debusine.debian.net";

/// A Debusine personal-repository workspace: `r-<name>-<project>`
/// (wiki.debian.org/DebusineDebianNet#Repositories). `name` is the
/// repository owner's Debusine name, `project` the source package.
pub fn debusine_workspace(name: &str, project: &str) -> String {
    format!("r-{name}-{project}")
}

/// A personal repository's publish workflow:
/// `publish-to-<suite>-<project>`. The suite is the *base* release the
/// repository serves — `sid` for unstable, `trixie` for a trixie
/// backport (the wiki's `~bpo13+1` example publishes to `trixie`, not
/// `trixie-backports`).
pub fn debusine_workflow(suite: &str, project: &str) -> String {
    format!("publish-to-{suite}-{project}")
}

/// The scope debusine.debian.net keeps Debian work in; personal
/// repositories are child workspaces of its `developers` workspace.
pub const DEBUSINE_SCOPE: &str = "debian";

/// The wiki page describing personal repositories: naming, the
/// `create-repository` workflow and `archive suite create`.
pub const DEBUSINE_WIKI: &str = "https://wiki.debian.org/DebusineDebianNet#Repositories";

/// A workspace's page on the instance (`https://debusine.debian.net/
/// debian/r-<name>-<project>/`), whose HTTP status says whether the
/// workspace exists — dput cannot create one.
pub fn debusine_workspace_url(workspace: &str) -> String {
    format!("https://{DEBUSINE_HOST}/{DEBUSINE_SCOPE}/{workspace}/")
}

/// A workflow template's page in a workspace
/// (`…/<workspace>/workflow-template/<name>/`): 200 means the
/// `publish-to-` workflow dput will name exists, which is the whole of
/// what an upload needs.
pub fn debusine_workflow_template_url(workspace: &str, workflow: &str) -> String {
    format!("https://{DEBUSINE_HOST}/{DEBUSINE_SCOPE}/{workspace}/workflow-template/{workflow}/")
}

/// `curl -s -o /dev/null -w %{http_code} <url>` — just the HTTP status
/// of a URL, for an existence check.
pub fn http_status_argv(url: &str) -> Vec<String> {
    argv(&["curl", "-s", "-o", "/dev/null", "-w", "%{http_code}", url])
}

/// The `create-repository` workflow's input: the suffix of the
/// workspace it creates (`r-<suffix>`), as the YAML the client reads
/// from stdin.
pub fn debusine_create_repository_data(workspace: &str) -> String {
    let suffix = workspace.strip_prefix("r-").unwrap_or(workspace);
    format!("suffix: \"{suffix}\"\n")
}

/// `debusine workflow start --workspace developers --data -
/// create-repository` — create the personal repository workspace
/// (its suffix arrives on stdin, see
/// [`debusine_create_repository_data`]). From the wiki's recipe.
pub fn debusine_create_repository_argv() -> Vec<String> {
    argv(&[
        "debusine",
        "workflow",
        "start",
        "--workspace",
        "developers",
        "--data",
        "-",
        "create-repository",
    ])
}

/// A personal repository's suite name, `<suite>-<project>` (the wiki
/// recommends the base Debian suite plus an own suffix), whose publish
/// workflow is then [`debusine_workflow`].
pub fn debusine_suite(suite: &str, project: &str) -> String {
    format!("{suite}-{project}")
}

/// `debusine archive suite create --workspace <ws> --architecture all
/// --architecture amd64 --architecture arm64 --base-workflow-template
/// upload-to-<dist> <suite>` — create the suite in a personal
/// repository, which also creates its `publish-to-<suite>` workflow
/// template from the shared `upload-to-*` one (`unstable` for `sid`,
/// else the suite's own name).
pub fn debusine_create_suite_argv(workspace: &str, base_suite: &str, suite: &str) -> Vec<String> {
    let dist = if base_suite == "sid" {
        "unstable"
    } else {
        base_suite
    };
    let template = format!("upload-to-{dist}");
    argv(&[
        "debusine",
        "archive",
        "suite",
        "create",
        "--workspace",
        workspace,
        "--architecture",
        "all",
        "--architecture",
        "amd64",
        "--architecture",
        "arm64",
        "--base-workflow-template",
        &template,
        suite,
    ])
}

/// `dput -O debusine_workspace=<ws> -O debusine_workflow=<wf>
/// debusine.debian.net <changes>` — upload to a Debusine personal
/// repository. The `-O` overrides replace the profile's defaults (the
/// shared `developers` workspace and its distribution-derived
/// `upload-to-*` workflow) with the personal repository's names.
pub fn dput_debusine_argv(workspace: &str, workflow: &str, changes: &str) -> Vec<String> {
    argv(&[
        "dput",
        "-O",
        &format!("debusine_workspace={workspace}"),
        "-O",
        &format!("debusine_workflow={workflow}"),
        DEBUSINE_HOST,
        changes,
    ])
}

/// `curl -sfG <launchpad-archive> --data-urlencode …` — query the
/// Launchpad API for published source packages named `source` in
/// `ppa:<owner>/<ppa>`. `-f` makes a missing PPA (HTTP 404) a non-zero
/// exit (no body), distinct from a real `{"total_size": 0}` answer. Used
/// to pre-flight a PPA upload: catch a wrong/typo'd `--ppa` before dput.
pub fn launchpad_sources_argv(owner: &str, ppa: &str, source: &str) -> Vec<String> {
    argv(&[
        "curl",
        "-sfG",
        &format!("https://api.launchpad.net/1.0/~{owner}/+archive/ubuntu/{ppa}"),
        "--data-urlencode",
        "ws.op=getPublishedSources",
        "--data-urlencode",
        "exact_match=true",
        "--data-urlencode",
        &format!("source_name={source}"),
    ])
}

/// The `total_size` from a Launchpad `getPublishedSources` JSON response
/// — the number of published source-package entries matching the query.
/// `None` when the body isn't the expected JSON (e.g. curl failed, or a
/// 404 "no such PPA" page), which the caller treats as "couldn't
/// verify" rather than "zero".
pub fn published_source_count(json: &str) -> Option<u64> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()?
        .get("total_size")?
        .as_u64()
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
pub fn glab_ci_list_sha_argv(project: &GitLabProject, sha: &str) -> Vec<String> {
    argv(&[
        "glab",
        "ci",
        "list",
        "-R",
        &project.url,
        "--sha",
        sha,
        "-F",
        "json",
    ])
}

/// The GitLab project behind a git remote, for pointing glab at it
/// explicitly (`-R <url>` for `glab ci`, `--hostname` + the encoded
/// project path for `glab api`). Left to guess among several remotes
/// glab prefers `origin`, which is wrong when the branch lives on a
/// fork.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitLabProject {
    /// The remote URL as configured.
    pub url: String,
    /// The GitLab host (`salsa.debian.org`).
    pub host: String,
    /// The project path (`group/sub/repo`, no `.git`).
    pub path: String,
}

impl GitLabProject {
    /// Parse a remote URL — scp-like (`git@host:group/repo.git`),
    /// `ssh://[user@]host[:port]/group/repo.git` or
    /// `https://[user@]host/group/repo.git`. `None` without a host or
    /// a path.
    pub fn from_remote_url(url: &str) -> Option<Self> {
        let (scheme, rest) = match url.split_once("://") {
            Some((s, r)) => (Some(s), r),
            None => (None, url),
        };
        let rest = rest.rsplit_once('@').map(|(_, r)| r).unwrap_or(rest);
        let host_end = rest.find(['/', ':'])?;
        let host = &rest[..host_end];
        let mut path = &rest[host_end + 1..];
        // With a scheme, a `:` after the host starts a port, not the path.
        if scheme.is_some() && rest.as_bytes()[host_end] == b':' {
            path = path.split_once('/').map(|(_, p)| p)?;
        }
        let path = path.trim_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        (!host.is_empty() && !path.is_empty()).then(|| Self {
            url: url.to_string(),
            host: host.to_string(),
            path: path.to_string(),
        })
    }

    /// The project's REST id: its path with `/` percent-encoded.
    fn api_id(&self) -> String {
        self.path.replace('/', "%2F")
    }
}

/// `glab api --hostname <host> <endpoint>` against `project`'s host.
fn glab_api_argv(project: &GitLabProject, endpoint: &str) -> Vec<String> {
    argv(&["glab", "api", "--hostname", &project.host, endpoint])
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

/// `glab api projects/<project>/pipelines/<id>/jobs?per_page=100` —
/// list a pipeline's jobs as raw API JSON; `per_page=100` avoids
/// needing pagination for any realistic pipeline. Used to report
/// per-job progress while watching (see [`crate::rebuild`]).
pub fn glab_pipeline_jobs_argv(project: &GitLabProject, pipeline_id: i64) -> Vec<String> {
    let endpoint = format!(
        "projects/{}/pipelines/{pipeline_id}/jobs?per_page=100",
        project.api_id()
    );
    glab_api_argv(project, &endpoint)
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

/// Where Salsa's README recommends pointing a project's CI config path,
/// and the file dbranch creates / adjusts on a rebuild branch.
pub const SALSA_CI_PATH: &str = "debian/salsa-ci.yml";

/// `glab api projects/<project>` — the project's settings as JSON,
/// read for its `ci_config_path`.
pub fn glab_project_argv(project: &GitLabProject) -> Vec<String> {
    glab_api_argv(project, &format!("projects/{}", project.api_id()))
}

/// `glab api -X PUT projects/<project> -f ci_config_path=<path>` — point
/// the project's CI config path at `path`. Needs Maintainer there.
pub fn glab_set_ci_config_path_argv(project: &GitLabProject, path: &str) -> Vec<String> {
    let endpoint = format!("projects/{}", project.api_id());
    let field = format!("ci_config_path={path}");
    argv(&[
        "glab",
        "api",
        "--hostname",
        &project.host,
        "-X",
        "PUT",
        &endpoint,
        "-f",
        &field,
    ])
}

/// The project's `ci_config_path` from `glab api projects/:id` JSON;
/// `None` when unset (`null` / empty — GitLab then looks for a root
/// `.gitlab-ci.yml`, so a Salsa project runs nothing) or on junk.
pub fn ci_config_path(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let path = v.get("ci_config_path")?.as_str()?.trim();
    (!path.is_empty()).then(|| path.to_string())
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
    fn changes_includes_orig_reads_the_files_stanza() {
        // A `-sa` upload: the orig tarball is offered alongside the
        // .dsc and the debian tarball.
        let full = "Files:\n \
                    a1 12 devel optional damo_3.2.8-1~questing+1.dsc\n \
                    b2 34 devel optional damo_3.2.8.orig.tar.gz\n \
                    c3 56 devel optional damo_3.2.8-1~questing+1.debian.tar.xz\n";
        assert!(changes_includes_orig(full));
        // A `-si`/`-sd` upload of a later revision: no orig on offer,
        // which an archive that doesn't already have it will reject.
        let diff_only = "Files:\n \
                         a1 12 devel optional damo_3.2.8-1~questing+1.dsc\n \
                         c3 56 devel optional damo_3.2.8-1~questing+1.debian.tar.xz\n";
        assert!(!changes_includes_orig(diff_only));
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
    fn debusine_workspace_setup_commands() {
        assert_eq!(
            debusine_workspace_url("r-michelin-opentmux"),
            "https://debusine.debian.net/debian/r-michelin-opentmux/"
        );
        assert_eq!(
            debusine_workflow_template_url("r-m-p", "publish-to-sid-p"),
            "https://debusine.debian.net/debian/r-m-p/workflow-template/publish-to-sid-p/"
        );
        assert_eq!(
            http_status_argv("https://x/y/"),
            [
                "curl",
                "-s",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
                "https://x/y/"
            ]
        );
        assert_eq!(
            debusine_create_repository_data("r-michelin-opentmux"),
            "suffix: \"michelin-opentmux\"\n"
        );
        assert_eq!(
            debusine_create_repository_argv(),
            [
                "debusine",
                "workflow",
                "start",
                "--workspace",
                "developers",
                "--data",
                "-",
                "create-repository"
            ]
        );
        assert_eq!(debusine_suite("sid", "opentmux"), "sid-opentmux");
        let sid = debusine_create_suite_argv("r-michelin-opentmux", "sid", "sid-opentmux");
        assert_eq!(
            &sid[..6],
            [
                "debusine",
                "archive",
                "suite",
                "create",
                "--workspace",
                "r-michelin-opentmux"
            ]
        );
        assert!(sid.contains(&"upload-to-unstable".to_string()));
        assert_eq!(sid.last().map(String::as_str), Some("sid-opentmux"));
        let trixie = debusine_create_suite_argv("r-m-p", "trixie", "trixie-p");
        assert!(trixie.contains(&"upload-to-trixie".to_string()));
    }

    #[test]
    fn debusine_names_and_dput_overrides() {
        // The wiki's personal-repository pattern, with the user's real
        // iptstate upload as the reference command.
        assert_eq!(
            debusine_workspace("michelin", "iptstate"),
            "r-michelin-iptstate"
        );
        assert_eq!(
            debusine_workflow("sid", "iptstate"),
            "publish-to-sid-iptstate"
        );
        assert_eq!(
            debusine_workflow("trixie", "iptstate"),
            "publish-to-trixie-iptstate"
        );
        assert_eq!(
            dput_debusine_argv(
                "r-michelin-iptstate",
                "publish-to-sid-iptstate",
                "../iptstate_2.3.0-1_source.changes"
            ),
            [
                "dput",
                "-O",
                "debusine_workspace=r-michelin-iptstate",
                "-O",
                "debusine_workflow=publish-to-sid-iptstate",
                "debusine.debian.net",
                "../iptstate_2.3.0-1_source.changes"
            ]
        );
    }

    #[test]
    fn ppa_owner_name_splits_only_ppa_targets() {
        assert_eq!(
            ppa_owner_name("ppa:michel/sugarjar"),
            Some(("michel", "sugarjar"))
        );
        // A plain dput host is not a PPA.
        assert_eq!(ppa_owner_name("mentors"), None);
        // A `ppa:` with no `/` is malformed → None.
        assert_eq!(ppa_owner_name("ppa:michel"), None);
    }

    #[test]
    fn launchpad_sources_argv_builds_the_query() {
        assert_eq!(
            launchpad_sources_argv("michel-slm", "kernel-utils", "sugarjar"),
            [
                "curl",
                "-sfG",
                "https://api.launchpad.net/1.0/~michel-slm/+archive/ubuntu/kernel-utils",
                "--data-urlencode",
                "ws.op=getPublishedSources",
                "--data-urlencode",
                "exact_match=true",
                "--data-urlencode",
                "source_name=sugarjar",
            ]
        );
    }

    #[test]
    fn published_source_count_reads_total_size() {
        assert_eq!(
            published_source_count(r#"{"start": 0, "total_size": 3, "entries": []}"#),
            Some(3)
        );
        assert_eq!(
            published_source_count(r#"{"total_size": 0, "entries": []}"#),
            Some(0)
        );
        // Not the expected JSON (e.g. a 404 page / curl failure) → None.
        assert_eq!(published_source_count("<html>404</html>"), None);
        assert_eq!(published_source_count(""), None);
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
        assert_eq!(
            gbp_dch_stable_argv("high"),
            [
                "gbp",
                "dch",
                "--stable",
                "-R",
                "-U",
                "high",
                "--spawn-editor=never"
            ]
        );
        assert_eq!(debuild_argv(true), ["debuild", "-S", "-sa", "-d"]);
        assert_eq!(debuild_argv(false), ["debuild", "-S", "-si", "-d"]);
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
            git_config_argv("branch.noble.pushRemote", "fork"),
            ["git", "config", "branch.noble.pushRemote", "fork"]
        );
    }

    #[test]
    fn upstream_git_argvs() {
        assert_eq!(
            git_clone_argv("https://h/g/thing.git", "upstream", "thing"),
            [
                "git",
                "clone",
                "--no-checkout",
                "-o",
                "upstream",
                "https://h/g/thing.git",
                "thing"
            ]
        );
        assert_eq!(
            git_fetch_tags_argv("upstream"),
            ["git", "fetch", "--tags", "upstream"]
        );
        assert_eq!(
            checkout_new_no_track_argv("debian/latest", "upstream/debian/latest"),
            [
                "git",
                "checkout",
                "--no-track",
                "-b",
                "debian/latest",
                "upstream/debian/latest"
            ]
        );
        assert_eq!(
            dh_make_argv("antifennel", "0.3.1"),
            ["dh_make", "-p", "antifennel_0.3.1", "--createorig"]
        );
        assert_eq!(
            push_tag_argv("origin", "0.3.1"),
            ["git", "push", "origin", "tag", "0.3.1"]
        );
        assert_eq!(upstream_version("0.3.1-1"), "0.3.1");
        assert_eq!(upstream_version("1:0.3.1-2~bpo13+1"), "0.3.1");
        assert_eq!(upstream_version("0.3.1"), "0.3.1");
        assert_eq!(upstream_version("2026.09-1-1"), "2026.09-1");
        assert_eq!(
            git_remote_add_argv("origin", "git@h:ns/thing.git"),
            ["git", "remote", "add", "origin", "git@h:ns/thing.git"]
        );
        assert_eq!(
            glab_namespace_argv("salsa.debian.org", "michel"),
            [
                "glab",
                "api",
                "--hostname",
                "salsa.debian.org",
                "namespaces?search=michel"
            ]
        );
        let create = glab_create_project_argv("salsa.debian.org", "thing", "12468", SALSA_CI_PATH);
        assert_eq!(
            &create[..7],
            [
                "glab",
                "api",
                "--hostname",
                "salsa.debian.org",
                "-X",
                "POST",
                "projects"
            ]
        );
        assert!(create.contains(&"namespace_id=12468".to_string()));
        assert!(create.contains(&"path=thing".to_string()));
        assert!(create.contains(&"visibility=public".to_string()));
        assert!(create.contains(&"ci_config_path=debian/salsa-ci.yml".to_string()));
        assert_eq!(
            mr_config_argv(
                "/h/.mrconfig",
                "src/x/thing",
                "git clone u thing",
                "git pull"
            ),
            [
                "mr",
                "-c",
                "/h/.mrconfig",
                "config",
                "src/x/thing",
                "checkout=git clone u thing",
                "update=git pull"
            ]
        );
    }

    #[test]
    fn salsa_json_helpers() {
        // The search is a substring match: pick the exact full_path.
        let ns = r#"[{"id": 1, "kind": "group", "full_path": "michel-team"},
                     {"id": 12468, "kind": "user", "full_path": "michel"}]"#;
        assert_eq!(namespace_id(ns, "michel"), Some(12468));
        assert_eq!(namespace_id(ns, "nobody"), None);
        assert_eq!(namespace_id("[]", "michel"), None);
        assert_eq!(
            project_ssh_url(
                r#"{"id": 7, "ssh_url_to_repo": "git@salsa.debian.org:michel/thing.git"}"#
            )
            .as_deref(),
            Some("git@salsa.debian.org:michel/thing.git")
        );
        assert_eq!(project_ssh_url(r#"{"message": "403 Forbidden"}"#), None);
        assert_eq!(repo_name_from_url("https://h/g/thing.git"), "thing");
        assert_eq!(repo_name_from_url("git@h:g/thing"), "thing");
        assert_eq!(repo_name_from_url("/tmp/thing.git/"), "thing");
        assert_eq!(
            repo_name_from_url("https://git.sr.ht/~technomancy/antifennel"),
            "antifennel"
        );
        let dch = gbp_dch_new_upstream_argv("0.3.1-1", "medium");
        assert!(dch.starts_with(&gbp_dch_release_argv("medium")));
        assert_eq!(&dch[dch.len() - 2..], ["-N", "0.3.1-1"]);
        assert_eq!(
            gbp_export_orig_argv(),
            [
                "gbp",
                "export-orig",
                "--pristine-tar",
                "--pristine-tar-commit"
            ]
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

    fn fork() -> GitLabProject {
        GitLabProject::from_remote_url("git@salsa.debian.org:michel/paperwm.git").unwrap()
    }

    #[test]
    fn gitlab_project_parses_remote_url_forms() {
        let p = fork();
        assert_eq!(p.host, "salsa.debian.org");
        assert_eq!(p.path, "michel/paperwm");
        assert_eq!(p.api_id(), "michel%2Fpaperwm");
        let nested = GitLabProject::from_remote_url(
            "https://salsa.debian.org/gnome-team/shell-extensions/paperwm.git",
        )
        .unwrap();
        assert_eq!(nested.path, "gnome-team/shell-extensions/paperwm");
        // ssh with a user and a port: the port is not part of the path.
        let port = GitLabProject::from_remote_url("ssh://git@host.example:2222/g/r/").unwrap();
        assert_eq!(
            (port.host.as_str(), port.path.as_str()),
            ("host.example", "g/r")
        );
        assert_eq!(GitLabProject::from_remote_url("https://host.example"), None);
        assert_eq!(GitLabProject::from_remote_url("nonsense"), None);
    }

    #[test]
    fn glab_ci_config_path_argvs() {
        assert_eq!(
            glab_project_argv(&fork()),
            [
                "glab",
                "api",
                "--hostname",
                "salsa.debian.org",
                "projects/michel%2Fpaperwm"
            ]
        );
        assert_eq!(
            glab_set_ci_config_path_argv(&fork(), SALSA_CI_PATH),
            [
                "glab",
                "api",
                "--hostname",
                "salsa.debian.org",
                "-X",
                "PUT",
                "projects/michel%2Fpaperwm",
                "-f",
                "ci_config_path=debian/salsa-ci.yml"
            ]
        );
        assert_eq!(
            glab_ci_list_sha_argv(&fork(), "abc"),
            [
                "glab",
                "ci",
                "list",
                "-R",
                "git@salsa.debian.org:michel/paperwm.git",
                "--sha",
                "abc",
                "-F",
                "json"
            ]
        );
    }

    #[test]
    fn ci_config_path_unset_is_none() {
        assert_eq!(
            ci_config_path(r#"{"id": 1, "ci_config_path": "debian/salsa-ci.yml"}"#).as_deref(),
            Some("debian/salsa-ci.yml")
        );
        // A remote include is a real, different setting.
        assert_eq!(
            ci_config_path(r#"{"ci_config_path": "recipes/debian.yml@salsa-ci-team/pipeline"}"#)
                .as_deref(),
            Some("recipes/debian.yml@salsa-ci-team/pipeline")
        );
        // Unset comes back as null or "" depending on the GitLab version.
        assert_eq!(ci_config_path(r#"{"ci_config_path": null}"#), None);
        assert_eq!(ci_config_path(r#"{"ci_config_path": ""}"#), None);
        assert_eq!(ci_config_path(r#"{"id": 1}"#), None);
        assert_eq!(ci_config_path("not json"), None);
    }

    #[test]
    fn glab_pipeline_jobs_argv_targets_the_project() {
        assert_eq!(
            glab_pipeline_jobs_argv(&fork(), 1111431),
            [
                "glab",
                "api",
                "--hostname",
                "salsa.debian.org",
                "projects/michel%2Fpaperwm/pipelines/1111431/jobs?per_page=100"
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
