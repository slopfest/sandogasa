// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `sig-repo`: make sure a SIG has its repo for a package, and with a
//! CentOS Stream merge request, carry the change there.
//!
//! A SIG's package repo (`CentOS/proposed_updates/rpms/<pkg>`,
//! `CentOS/Hyperscale/rpms/<pkg>`) is a pull mirror of the package's
//! CentOS Stream repo: GitLab keeps Stream's branches current, and the
//! SIG's own branches sit beside them, never overwritten. CBS builds
//! from it. So when the repo is missing, it is created that way, and
//! for a Proposed Update the merge request's commits are pushed to it
//! as `<release>-proposed-<RHEL key>`, the branch CBS builds the update
//! from.
//!
//! The GitLab side is API requests, described in words under
//! `--explain` and `--dry-run`; the git side is shown as the commands a
//! person would type.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use sandogasa_cbs::Sig;
use sandogasa_cbs::remotes::{Remotes, Role};
use sandogasa_cli::explain::Ui;
use sandogasa_gitlab::{Client, GroupClient, PullMirror};

/// The branches a SIG's mirror takes from CentOS Stream's repo: Stream's
/// release branches, not its topic or automation branches.
pub const MIRRORED_BRANCHES: &str = "^c[0-9]+s$";

#[derive(clap::Args)]
pub struct SigRepoArgs {
    /// A CentOS Stream merge request URL (a Proposed Update), or a
    /// package name
    #[arg(value_name = "MR_URL|PACKAGE")]
    pub target: String,

    /// Show each step without doing anything
    #[arg(long)]
    pub dry_run: bool,

    /// Narrate each step and show its command, pausing before each
    #[arg(long)]
    pub explain: bool,

    /// The dist-git checkout to push from (default: the current
    /// directory)
    #[arg(short = 'C', long, value_name = "DIR")]
    pub repo: Option<PathBuf>,

    /// The SIG, for a package name (a merge request is always a
    /// Proposed Update)
    #[arg(long, value_enum)]
    pub sig: Option<SigName>,

    /// Create the repo without asking
    #[arg(short = 'y', long)]
    pub yes: bool,
}

/// The SIGs, as `--sig` names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SigName {
    Hyperscale,
    ProposedUpdates,
}

/// A Proposed Update's change, from its CentOS Stream merge request.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub mr_url: String,
    pub iid: u64,
    /// The SIG's branch for it: `c10s-proposed-RHEL-303721`.
    pub branch: String,
    /// The merge request's head commit, when GitLab says.
    pub head: Option<String>,
}

/// How far a pull mirror's update has got, from its `update_status`.
#[derive(Debug, PartialEq)]
pub enum MirrorUpdate {
    Done,
    Failed,
    Running,
}

impl From<SigName> for Sig {
    fn from(s: SigName) -> Sig {
        match s {
            SigName::Hyperscale => Sig::Hyperscale,
            SigName::ProposedUpdates => Sig::ProposedUpdates,
        }
    }
}

/// The first RHEL issue a merge request resolves, from its description's
/// `Resolves:` lines, which CentOS Stream's template requires.
pub fn first_resolved(description: &str) -> Option<String> {
    description.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("Resolves:")?;
        let key = rest.split([',', ' ']).map(str::trim).find(|w| {
            w.strip_prefix("RHEL-")
                .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        })?;
        Some(key.to_string())
    })
}

/// Whether `git ls-remote` output for one branch names `head`.
pub fn at_head(ls_remote: &str, head: &str) -> bool {
    ls_remote
        .split_whitespace()
        .next()
        .is_some_and(|sha| !head.is_empty() && sha.eq_ignore_ascii_case(head))
}

/// The open tracking issue (cpu-sig-tracker's) that names the merge
/// request at `mr_url`, among `issues`.
pub fn tracking_issue<'a>(
    issues: &'a [sandogasa_gitlab::Issue],
    mr_url: &str,
) -> Option<&'a sandogasa_gitlab::Issue> {
    // The URL, ending where its number does: `…/merge_requests/9` is not
    // `…/merge_requests/90`.
    let names = |d: &str| {
        d.match_indices(mr_url).any(|(at, _)| {
            !d[at + mr_url.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
        })
    };
    issues
        .iter()
        .find(|i| i.description.as_deref().is_some_and(names))
}

/// What a pull mirror's `update_status` says: done once `finished`,
/// failed on `failed` or `canceled`, still running otherwise
/// (`none`, `scheduled`, `started`).
pub fn mirror_update(status: Option<&str>) -> MirrorUpdate {
    match status {
        Some("finished") => MirrorUpdate::Done,
        Some("failed" | "canceled") => MirrorUpdate::Failed,
        _ => MirrorUpdate::Running,
    }
}

pub fn run(args: &SigRepoArgs) -> ExitCode {
    match run_with(
        args,
        &crate::config::gitlab_base(),
        crate::config::gitlab_token(),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Run `git` in `repo` to read something, its output trimmed.
fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The checkout's remotes, by the part each plays, and every remote's
/// name.
fn remotes(repo: &Path) -> Result<(Remotes, Vec<String>), String> {
    let remotes = Remotes::parse(&git(repo, &["remote", "-v"])?);
    let names = git(repo, &["remote"])?
        .lines()
        .map(str::to_string)
        .collect();
    Ok((remotes, names))
}

/// The remote for `sig`'s repo, adding it, named after the SIG, when no
/// remote points at it.
fn ensure_sig_remote(
    ui: &Ui,
    repo: &Path,
    base: &str,
    sig: Sig,
    path: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let (remotes, names) = remotes(repo)?;
    if let Some(r) = remotes.find(Role::Sig(sig)) {
        return Ok(r.name.clone());
    }
    let name = Remotes::name_to_add(Role::Sig(sig), &names)?;
    let host = base.split_once("://").map_or(base, |(_, h)| h);
    ui.step(&format!("Add the SIG's repo as the remote `{name}`"));
    let url = format!("git+ssh://git@{host}/{path}.git");
    ui.run_required(
        &["git", "remote", "add", name, &url].map(String::from),
        repo,
    )?;
    Ok(name.to_string())
}

/// The change a CentOS Stream merge request carries, read from GitLab.
fn change_of(base: &str, mr_url: &str) -> Result<(String, Change), Box<dyn std::error::Error>> {
    let (_, project, iid) = sandogasa_gitlab::parse_mr_url(mr_url)?;
    let package = project
        .strip_prefix("redhat/centos-stream/rpms/")
        .filter(|p| !p.is_empty() && !p.contains('/'))
        .ok_or_else(|| format!("{mr_url} is not a CentOS Stream merge request"))?
        .to_string();
    let mr = Client::new(base, &project, "")?.merge_request(iid)?;
    let key = first_resolved(mr.description.as_deref().unwrap_or("")).ok_or_else(|| {
        format!("{mr_url} has no `Resolves: RHEL-…` line to name the SIG's branch after")
    })?;
    Ok((
        package,
        Change {
            mr_url: mr.web_url,
            iid,
            branch: sandogasa_cbs::proposed_branch(&mr.target_branch, &key),
            head: mr.sha,
        },
    ))
}

/// Wait for a new mirror's first update, checking every 3 seconds for
/// up to ten minutes.
fn wait_for_mirror(client: &Client, path: &str) -> Result<(), String> {
    const TRIES: u32 = 200;
    for i in 0..TRIES {
        let state = client
            .pull_mirror()
            .map_err(|e| format!("cannot read {path}'s pull mirror: {e}"))?;
        let status = state.as_ref().and_then(|s| s.update_status.clone());
        match mirror_update(status.as_deref()) {
            MirrorUpdate::Done => return Ok(()),
            MirrorUpdate::Failed => {
                let why = state
                    .and_then(|s| s.last_error)
                    .unwrap_or_else(|| "GitLab gave no reason".to_string());
                return Err(format!(
                    "the first mirror update of {path} failed: {why}; see Settings → \
                     Repository → Mirroring repositories"
                ));
            }
            MirrorUpdate::Running => {
                if i == 0 {
                    eprintln!("waiting for GitLab to mirror {path}");
                }
                std::thread::sleep(std::time::Duration::from_secs(3));
            }
        }
    }
    Err(format!(
        "GitLab is still mirroring {path} after ten minutes; run this again later"
    ))
}

/// Make sure the SIG's repo for `package` exists: a pull mirror of the
/// Stream repo's `c<N>s` branches when Stream has the package, an empty
/// project otherwise (a Hyperscale package tracking Fedora, or a new
/// one). An existing mirror without that branch filter is offered it.
fn ensure_repo(
    ui: &Ui,
    args: &SigRepoArgs,
    base: &str,
    token: Option<&str>,
    sig: Sig,
    package: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let group = sig.gitlab_group();
    let path = format!("{group}/{package}");
    let stream_path = sandogasa_cbs::stream_project(package);
    let stream = format!("{base}/{stream_path}.git");
    ui.step(&format!(
        "Look for the SIG's repo {path}, through GitLab's API"
    ));
    if Client::new(base, &path, token.unwrap_or(""))?
        .project()?
        .is_some()
    {
        println!("{path} exists.");
        return check_mirror(ui, args, base, token, &path, &stream);
    }
    // A Proposed Update is always of a Stream package; a Hyperscale one
    // need not be.
    let mirrored = sig == Sig::ProposedUpdates
        || Client::new(base, &stream_path, token.unwrap_or(""))?
            .project()?
            .is_some();
    ui.step(&format!(
        "Create {path}, public, through GitLab's API (New project{} on the web)",
        if mirrored {
            " → Import project → Repository by URL"
        } else {
            " → Create blank project"
        }
    ));
    if mirrored {
        ui.step(&format!(
            "Make it a pull mirror of {stream}'s {MIRRORED_BRANCHES} branches, never \
             overwriting a branch the SIG adds, starting no pipelines (Settings → Repository \
             → Mirroring repositories, \"Mirror branches matching\")"
        ));
        ui.step("Wait for GitLab's first mirror update");
    }
    if args.dry_run {
        return Ok(());
    }
    let token = token.ok_or(
        "creating the SIG's repo needs a GitLab token: set GITLAB_TOKEN, or run `cbspkg config`",
    )?;
    let what = if mirrored {
        format!("Create {path} as a pull mirror of {stream}?")
    } else {
        format!("Create {path}, empty, as CentOS Stream has no {package}?")
    };
    if !confirmed(args, &what, &format!("{path} does not exist; creating it"))? {
        return Err(format!("{path} not created").into());
    }
    let project = GroupClient::new(base, group, token)?.create_project(package, "public")?;
    println!("Created {}", project.web_url);
    if mirrored {
        let client = Client::new(base, &path, token)?;
        client.set_pull_mirror(&PullMirror::branches_matching(&stream, MIRRORED_BRANCHES))?;
        client.start_pull_mirror()?;
        wait_for_mirror(&client, &path)?;
        println!("{path} mirrors {stream}'s {MIRRORED_BRANCHES} branches.");
    }
    Ok(())
}

/// Whether to go ahead: yes with `--yes`, else asked at a terminal,
/// else an error saying `doing` needs one.
fn confirmed(args: &SigRepoArgs, question: &str, doing: &str) -> Result<bool, String> {
    if args.yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err(format!("{doing} needs a terminal to confirm at, or --yes"));
    }
    sandogasa_cli::confirm(question, true).map_err(|e| e.to_string())
}

/// For an existing repo: whether its pull mirror takes only the
/// branches it should, offering to set the filter when not. Reading a
/// mirror's settings needs the Maintainer role, so without a token, or
/// without the role, this says so and moves on.
fn check_mirror(
    ui: &Ui,
    args: &SigRepoArgs,
    base: &str,
    token: Option<&str>,
    path: &str,
    stream: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(token) = token else {
        return Ok(());
    };
    ui.step(&format!(
        "Check that {path} mirrors only {MIRRORED_BRANCHES} branches, through GitLab's API"
    ));
    let client = Client::new(base, path, token)?;
    let state = match client.pull_mirror() {
        Ok(Some(s)) if s.enabled == Some(true) => s,
        // No mirror, or not one we may read: not ours to change.
        Ok(_) | Err(_) => return Ok(()),
    };
    if state.mirror_branch_regex.as_deref() == Some(MIRRORED_BRANCHES) {
        return Ok(());
    }
    let url = state.url.clone().unwrap_or_else(|| stream.to_string());
    println!(
        "{path} mirrors {}, not only its {MIRRORED_BRANCHES} branches.",
        state
            .mirror_branch_regex
            .as_deref()
            .map_or("every branch".to_string(), |r| format!(
                "branches matching {r}"
            ))
    );
    ui.step(&format!(
        "Mirror only {MIRRORED_BRANCHES} branches (Settings → Repository → Mirroring \
         repositories, \"Mirror branches matching\")"
    ));
    if args.dry_run
        || !confirmed(
            args,
            &format!("Mirror only {MIRRORED_BRANCHES} branches of {url}?"),
            "changing the mirror",
        )?
    {
        return Ok(());
    }
    client.set_pull_mirror(&PullMirror::branches_matching(&url, MIRRORED_BRANCHES))?;
    println!(
        "{path} now mirrors only {MIRRORED_BRANCHES} branches. Branches it already copied stay; \
         delete those you do not want by hand."
    );
    Ok(())
}

/// Push the merge request's commits to the SIG's repo as its branch.
fn push_change(
    ui: &Ui,
    repo: &Path,
    base: &str,
    path: &str,
    package: &str,
    change: &Change,
) -> Result<(), Box<dyn std::error::Error>> {
    let argv = |args: &[&str]| args.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    let remote = ensure_sig_remote(ui, repo, base, Sig::ProposedUpdates, path)?;
    let stream = format!("{base}/{}.git", sandogasa_cbs::stream_project(package));
    let mr_ref = format!("refs/merge-requests/{}/head", change.iid);
    // Already there, from an earlier run: nothing to fetch or push. The
    // check reads the SIG's branch, changing nothing.
    let branch_ref = format!("refs/heads/{}", change.branch);
    ui.step(&format!(
        "Check whether {path}'s {} is at the merge request's head already",
        change.branch
    ));
    ui.show_command(&argv(&["git", "ls-remote", &remote, &branch_ref]));
    let on_sig = git(repo, &["ls-remote", &remote, &branch_ref]).ok();
    if change
        .head
        .as_deref()
        .is_some_and(|h| at_head(on_sig.as_deref().unwrap_or(""), h))
    {
        println!(
            "{} in {path} is at the merge request's head already.",
            change.branch
        );
        // The remote-tracking branch the local one tracks, if this
        // checkout has never fetched it.
        let tracking = format!("refs/remotes/{remote}/{}", change.branch);
        if git(repo, &["rev-parse", "--verify", "--quiet", &tracking]).is_err() {
            ui.run_required(&argv(&["git", "fetch", &remote, &change.branch]), repo)?;
        }
        return track_locally(ui, repo, &remote, &change.branch);
    }
    ui.step(&format!(
        "Fetch the merge request's commits from CentOS Stream ({mr_ref})"
    ));
    ui.run_required(&argv(&["git", "fetch", &stream, &mr_ref]), repo)?;
    ui.step(&format!(
        "Push them to the SIG's repo as {}. --no-verify skips CentOS Stream's pre-push hook, \
         which checks the spec against the working tree's sources file rather than the \
         pushed commit's; these commits already passed it on their way to the merge request",
        change.branch
    ));
    // --no-verify: Stream's pre-push hook checks the spec against the
    // working tree's `sources`, not the pushed commit's, and these
    // commits already passed it; see DEVELOPMENT.md.
    ui.run_required(
        &argv(&[
            "git",
            "push",
            "--no-verify",
            &remote,
            &format!("FETCH_HEAD:refs/heads/{}", change.branch),
        ]),
        repo,
    )?;
    if !ui.dry_run {
        println!("Pushed the merge request to {path} as {}.", change.branch);
    }
    track_locally(ui, repo, &remote, &change.branch)
}

/// A local `branch` tracking `remote`'s, for the cbspkg commands that
/// work from the checked-out branch (add-pkg, build), unless there is
/// one; the checkout stays where it is.
fn track_locally(
    ui: &Ui,
    repo: &Path,
    remote: &str,
    branch: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let has_local = git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok();
    if has_local {
        if !ui.dry_run {
            println!("{branch} exists locally already; left as it is.");
        }
        return Ok(());
    }
    ui.step(&format!(
        "Make a local {branch} tracking it, to check out when building"
    ));
    ui.run_required(
        &[
            "git",
            "branch",
            "--track",
            branch,
            &format!("{remote}/{branch}"),
        ]
        .map(String::from),
        repo,
    )?;
    if !ui.dry_run {
        println!("Made the local branch {branch}: `git checkout {branch}` to work on it.");
    }
    Ok(())
}

/// `run`, with the GitLab instance and token passed in.
pub fn run_with(
    args: &SigRepoArgs,
    base: &str,
    token: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let ui = Ui {
        explain: args.explain,
        dry_run: args.dry_run,
        quiet: false,
    };
    let is_url = args.target.contains("://");
    let (sig, package, change) = if is_url {
        if args.sig.is_some_and(|s| s != SigName::ProposedUpdates) {
            return Err("a CentOS Stream merge request is a Proposed Update: drop --sig".into());
        }
        sandogasa_cli::require_tools(&[("git", "sudo dnf install git-core", Some("--version"))])?;
        let (package, change) = change_of(base, &args.target)?;
        (Sig::ProposedUpdates, package, Some(change))
    } else {
        let sig = args
            .sig
            .ok_or("say which SIG the repo is for: --sig hyperscale or --sig proposed-updates")?;
        (Sig::from(sig), args.target.clone(), None)
    };
    ensure_repo(&ui, args, base, token.as_deref(), sig, &package)?;
    let repo = args.repo.clone().unwrap_or_else(|| PathBuf::from("."));
    let path = format!("{}/{package}", sig.gitlab_group());
    // In a checkout of the package, the SIG's repo is a remote of it, by
    // its part (#102); outside one there is nothing to add it to.
    let in_checkout = remotes(&repo).is_ok_and(|(r, _)| r.package() == Some(package.as_str()));
    if change.is_none() && in_checkout {
        ensure_sig_remote(&ui, &repo, base, sig, &path)?;
    }
    if let Some(change) = change {
        push_change(&ui, &repo, base, &path, &package, &change)?;
        // The tracking issue next, unless the SIG has one for it.
        let issues = Client::new(base, &path, token.as_deref().unwrap_or(""))?
            .list_issues("cpu-sig-tracker", Some("opened"))
            .unwrap_or_default();
        match tracking_issue(&issues, &change.mr_url) {
            Some(i) => println!("\nTracked in {}.", i.web_url),
            None => println!(
                "\nNext, the SIG's tracking issue: cpu-sig-tracker file-issue {}",
                change.mr_url
            ),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sigs_branch_is_named_after_the_first_resolved_issue() {
        let d = "Backport upstream a7404a9a.\n\nResolves: RHEL-303720\nResolves: RHEL-303761\n";
        assert_eq!(first_resolved(d).as_deref(), Some("RHEL-303720"));
        assert_eq!(
            first_resolved("Resolves: RHEL-1, RHEL-2").as_deref(),
            Some("RHEL-1")
        );
        assert_eq!(first_resolved("Related: RHEL-9\n"), None);
        assert_eq!(first_resolved("Resolves: RHEL-\n"), None);
    }

    #[test]
    fn a_mirror_update_is_done_failed_or_running() {
        assert_eq!(mirror_update(Some("finished")), MirrorUpdate::Done);
        assert_eq!(mirror_update(Some("failed")), MirrorUpdate::Failed);
        assert_eq!(mirror_update(Some("canceled")), MirrorUpdate::Failed);
        for running in [None, Some("none"), Some("scheduled"), Some("started")] {
            assert_eq!(mirror_update(running), MirrorUpdate::Running, "{running:?}");
        }
    }

    fn args(target: &str, sig: Option<SigName>) -> SigRepoArgs {
        SigRepoArgs {
            target: target.to_string(),
            dry_run: false,
            explain: false,
            repo: None,
            sig,
            yes: true,
        }
    }

    /// rsync's first run: no SIG repo yet, so it is created in the
    /// SIG's group and made a pull mirror of the Stream repo.
    #[test]
    fn a_missing_repo_is_created_as_a_pull_mirror_of_stream() {
        let mut server = mockito::Server::new();
        let path = "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Frsync";
        server.mock("GET", path).with_status(404).create();
        server
            .mock("GET", "/api/v4/groups/CentOS%2Fproposed_updates%2Frpms")
            .with_status(200)
            .with_body(r#"{"id": 77, "full_path": "CentOS/proposed_updates/rpms"}"#)
            .create();
        let created = server
            .mock("POST", "/api/v4/projects")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "path": "rsync", "namespace_id": 77, "visibility": "public"
            })))
            .with_status(201)
            .with_body(
                r#"{"id": 9, "path_with_namespace": "CentOS/proposed_updates/rpms/rsync",
                    "web_url": "https://gitlab.example/CentOS/proposed_updates/rpms/rsync",
                    "ssh_url_to_repo": "git@gitlab.example:CentOS/proposed_updates/rpms/rsync.git",
                    "http_url_to_repo": "https://gitlab.example/CentOS/proposed_updates/rpms/rsync.git"}"#,
            )
            .create();
        let mirror = format!("{path}/mirror/pull");
        let configured = server
            .mock("PUT", mirror.as_str())
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "enabled": true,
                "url": format!("{}/redhat/centos-stream/rpms/rsync.git", server.url()),
                "mirror_overwrites_diverged_branches": false,
            })))
            .with_status(200)
            .with_body("{}")
            .create();
        let started = server
            .mock("POST", mirror.as_str())
            .with_status(200)
            .create();
        server
            .mock("GET", mirror.as_str())
            .with_status(200)
            .with_body(r#"{"update_status": "finished"}"#)
            .create();

        run_with(
            &args("rsync", Some(SigName::ProposedUpdates)),
            &server.url(),
            Some("t".to_string()),
        )
        .unwrap();
        created.assert();
        configured.assert();
        started.assert();
    }

    #[test]
    fn an_existing_repo_is_left_alone_and_a_dry_run_changes_nothing() {
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/api/v4/projects/CentOS%2FHyperscale%2Frpms%2Frpm")
            .with_status(200)
            .with_body(
                r#"{"id": 3, "path_with_namespace": "CentOS/Hyperscale/rpms/rpm",
                    "web_url": "w", "ssh_url_to_repo": "s", "http_url_to_repo": "h"}"#,
            )
            .create();
        let writes = server
            .mock("POST", mockito::Matcher::Any)
            .expect(0)
            .create();
        run_with(&args("rpm", Some(SigName::Hyperscale)), &server.url(), None).unwrap();

        server
            .mock("GET", "/api/v4/projects/CentOS%2FHyperscale%2Frpms%2Fnew")
            .with_status(404)
            .create();
        server
            .mock(
                "GET",
                "/api/v4/projects/redhat%2Fcentos-stream%2Frpms%2Fnew",
            )
            .with_status(404)
            .create();
        let mut dry = args("new", Some(SigName::Hyperscale));
        dry.dry_run = true;
        run_with(&dry, &server.url(), None).unwrap();
        writes.assert();
    }

    #[test]
    fn a_package_needs_its_sig_and_creating_needs_a_token() {
        let err = run_with(&args("rsync", None), "http://127.0.0.1:9", None).unwrap_err();
        assert!(err.to_string().contains("--sig"), "{err}");

        let mut server = mockito::Server::new();
        server
            .mock(
                "GET",
                "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Frsync",
            )
            .with_status(404)
            .create();
        let err = run_with(
            &args("rsync", Some(SigName::ProposedUpdates)),
            &server.url(),
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("GITLAB_TOKEN"), "{err}");
    }

    /// wprof's case: a Hyperscale package CentOS Stream does not have.
    /// Its repo is created empty, with no mirror to set up.
    #[test]
    fn a_hyperscale_package_stream_lacks_gets_an_empty_repo() {
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/api/v4/projects/CentOS%2FHyperscale%2Frpms%2Fwprof")
            .with_status(404)
            .create();
        server
            .mock(
                "GET",
                "/api/v4/projects/redhat%2Fcentos-stream%2Frpms%2Fwprof",
            )
            .with_status(404)
            .create();
        server
            .mock("GET", "/api/v4/groups/CentOS%2FHyperscale%2Frpms")
            .with_status(200)
            .with_body(r#"{"id": 5, "full_path": "CentOS/Hyperscale/rpms"}"#)
            .create();
        let created = server
            .mock("POST", "/api/v4/projects")
            .with_status(201)
            .with_body(
                r#"{"id": 11, "path_with_namespace": "CentOS/Hyperscale/rpms/wprof",
                    "web_url": "w", "ssh_url_to_repo": "s", "http_url_to_repo": "h"}"#,
            )
            .create();
        let mirror = server
            .mock("PUT", mockito::Matcher::Regex("mirror/pull".into()))
            .expect(0)
            .create();
        run_with(
            &args("wprof", Some(SigName::Hyperscale)),
            &server.url(),
            Some("t".into()),
        )
        .unwrap();
        created.assert();
        mirror.assert();
    }

    /// rsync's mirror, set up to copy every branch: offered, and with
    /// --yes given, the c<N>s filter.
    #[test]
    fn an_existing_mirror_of_every_branch_is_narrowed() {
        let mut server = mockito::Server::new();
        let path = "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Frsync";
        server
            .mock("GET", path)
            .with_status(200)
            .with_body(
                r#"{"id": 9, "path_with_namespace": "CentOS/proposed_updates/rpms/rsync",
                    "web_url": "w", "ssh_url_to_repo": "s", "http_url_to_repo": "h"}"#,
            )
            .create();
        let mirror = format!("{path}/mirror/pull");
        server
            .mock("GET", mirror.as_str())
            .with_status(200)
            .with_body(
                r#"{"enabled": true, "url": "https://gitlab.com/redhat/centos-stream/rpms/rsync.git",
                    "update_status": "finished", "mirror_branch_regex": null}"#,
            )
            .create();
        let narrowed = server
            .mock("PUT", mirror.as_str())
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "mirror_branch_regex": MIRRORED_BRANCHES,
                "url": "https://gitlab.com/redhat/centos-stream/rpms/rsync.git",
            })))
            .with_status(200)
            .with_body("{}")
            .create();
        run_with(
            &args("rsync", Some(SigName::ProposedUpdates)),
            &server.url(),
            Some("t".into()),
        )
        .unwrap();
        narrowed.assert();
    }

    #[test]
    fn a_branch_at_the_merge_requests_head_is_not_pushed_again() {
        let head = "e4441b13be38e220451c8186abca5b27c44cd651";
        assert!(at_head(
            &format!("{head}\trefs/heads/c10s-proposed-RHEL-303721\n"),
            head
        ));
        assert!(!at_head("3afcfa97\trefs/heads/x\n", head));
        // No such branch yet: ls-remote prints nothing.
        assert!(!at_head("", head));
    }

    #[test]
    fn the_tracking_issue_is_the_one_naming_the_merge_request() {
        let issues: Vec<sandogasa_gitlab::Issue> = serde_json::from_value(serde_json::json!([
            {"iid": 2, "title": "t", "state": "opened", "web_url": "https://g/i/2",
             "description": "- **MR**: [x](https://gitlab.com/redhat/centos-stream/rpms/rsync/-/merge_requests/91) — opened"},
            {"iid": 1, "title": "t", "state": "opened", "web_url": "https://g/i/1",
             "description": "- **MR**: [x](https://gitlab.com/redhat/centos-stream/rpms/rsync/-/merge_requests/90) — opened"},
        ]))
        .unwrap();
        let found = tracking_issue(
            &issues,
            "https://gitlab.com/redhat/centos-stream/rpms/rsync/-/merge_requests/90",
        );
        assert_eq!(found.map(|i| i.iid), Some(1));
        assert!(tracking_issue(&issues, "https://gitlab.com/x/-/merge_requests/9").is_none());
        // MR 9 of the same project is neither 90 nor 91.
        assert!(
            tracking_issue(
                &issues,
                "https://gitlab.com/redhat/centos-stream/rpms/rsync/-/merge_requests/9"
            )
            .is_none()
        );
    }
}
