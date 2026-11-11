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
    // Only [sigs] matters here: a config that does not parse is a
    // warning, as the token is read without it too.
    let config = crate::config::load_or_default().unwrap_or_else(|e| {
        eprintln!(
            "warning: cannot read {} ({e}); going by the SIGs' own merge methods",
            crate::config::config_file().describe_sources()
        );
        Default::default()
    });
    match run_configured(
        args,
        &crate::config::gitlab_base(),
        crate::config::gitlab_token(),
        &config,
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
    merge_method: Option<&str>,
) -> Result<bool, Box<dyn std::error::Error>> {
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
        check_mirror(ui, args, base, token, &path, &stream)?;
        return Ok(false);
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
    if let Some(method) = merge_method {
        ui.step(&merge_method_step(method));
    }
    if mirrored {
        ui.step(&format!(
            "Make it a pull mirror of {stream}'s {MIRRORED_BRANCHES} branches, never \
             overwriting a branch the SIG adds, starting no pipelines (Settings → Repository \
             → Mirroring repositories, \"Mirror branches matching\")"
        ));
        ui.step("Wait for GitLab's first mirror update");
    }
    if args.dry_run {
        // Narrated as for a new repo: nothing to check afterwards.
        return Ok(true);
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
    if let Some(method) = merge_method {
        set_merge_method(&Client::new(base, &path, token)?, &path, method);
    }
    if mirrored {
        let client = Client::new(base, &path, token)?;
        client.set_pull_mirror(&PullMirror::branches_matching(&stream, MIRRORED_BRANCHES))?;
        client.start_pull_mirror()?;
        wait_for_mirror(&client, &path)?;
        println!("{path} mirrors {stream}'s {MIRRORED_BRANCHES} branches.");
    }
    Ok(true)
}

/// The `--explain` step for setting a repo's merge method, with where
/// it is on the web.
fn merge_method_step(method: &str) -> String {
    format!(
        "Set its merge method to {method} (Settings → Merge requests → Merge method){}",
        if method == "ff" {
            ", so merge requests are rebased and history stays linear"
        } else {
            ""
        }
    )
}

/// Set the repo's merge method; a refusal (usually the Maintainer role
/// missing) is a warning, as the rest of the run still stands.
fn set_merge_method(client: &Client, path: &str, method: &str) {
    match client.update_project(&sandogasa_gitlab::ProjectUpdate {
        merge_method: Some(method.to_string()),
        ..Default::default()
    }) {
        Ok(()) => println!("{path} now merges with {method}."),
        Err(e) => println!("warning: could not set {path}'s merge method to {method}: {e}"),
    }
}

/// For an existing repo: whether merge requests land the way the SIG
/// wants ([`crate::config::merge_method`]), offering to change it when
/// not, as for the default branch. `current` is the repo's method as
/// [`ensure_default_branch`] read it: without a token, a repo that
/// could not be read, or no method to want, this is skipped; a run with
/// no terminal to confirm at and no `--yes` leaves it as it is, saying
/// so.
fn ensure_merge_method(
    ui: &Ui,
    args: &SigRepoArgs,
    base: &str,
    token: Option<&str>,
    path: &str,
    current: Option<Option<String>>,
    wanted: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (Some(token), Some(current), Some(wanted)) = (token, current, wanted) else {
        return Ok(());
    };
    if current.as_deref() == Some(wanted) {
        println!("{path} merges with {wanted} already.");
        return Ok(());
    }
    println!(
        "{path} merges with {}; the SIG's repos merge with {wanted}.",
        current.as_deref().unwrap_or("GitLab's default")
    );
    ui.step(&merge_method_step(wanted));
    if args.dry_run {
        return Ok(());
    }
    if !args.yes && !std::io::stdin().is_terminal() {
        println!("No terminal to confirm at, and no --yes: the merge method is left as it is.");
        return Ok(());
    }
    if confirmed(
        args,
        &format!("Make {path} merge with {wanted}?"),
        "changing the merge method",
    )? {
        set_merge_method(&Client::new(base, path, token)?, path, wanted);
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

/// The branch a SIG repo should open on: the one this run pushed
/// (`pushed`), when it is the SIG's own and of the newest Stream
/// release among them, as a new proposal is; else the newest of the
/// SIG's own branches ([`Sig::newest_own_branch`]).
pub fn default_branch_for<'a, T: Ord>(
    sig: Sig,
    branches: &'a [(String, T)],
    pushed: Option<&'a str>,
) -> Option<&'a str> {
    let newest = sig.newest_own_branch(branches);
    let major = |b: &str| sig.own_branch_major(b);
    match (pushed, newest) {
        (Some(p), Some(n)) if major(p).is_some() && major(p) >= major(n) => Some(p),
        (Some(p), None) if major(p).is_some() => Some(p),
        (_, n) => n,
    }
}

/// Make the SIG's repo open on its newest own branch
/// ([`default_branch_for`]), asking first: a new proposal's branch
/// becomes the default once pushed, so each run checks again, and a
/// dry run counts the branch it would push. Changing the default branch
/// needs the Maintainer role, so without a token this is skipped; a
/// repo that cannot be read or changed, or a run with no terminal to
/// confirm at and no `--yes`, leaves it as it is, saying so. The repo's
/// merge method as read along the way, for [`ensure_merge_method`];
/// `None` when nothing was read.
fn ensure_default_branch(
    ui: &Ui,
    args: &SigRepoArgs,
    base: &str,
    token: Option<&str>,
    sig: Sig,
    path: &str,
    pushed: Option<&str>,
) -> Result<Option<Option<String>>, Box<dyn std::error::Error>> {
    let Some(token) = token else {
        return Ok(None);
    };
    ui.step(&format!(
        "Find {path}'s newest SIG branch, the one it should open on, through GitLab's API"
    ));
    let client = Client::new(base, path, token)?;
    let read = client.project_status().and_then(|status| {
        Ok((
            status.default_branch,
            status.merge_method,
            client.list_branches()?,
        ))
    });
    let (current, merge_method, branches) = match read {
        Ok(read) => read,
        Err(e) => {
            println!(
                "Cannot read {path}'s settings ({e}); its default branch and merge method are \
                 left as they are. Run sig-repo again to retry."
            );
            return Ok(None);
        }
    };
    let read = Some(merge_method);
    let dated: Vec<(String, Option<chrono::DateTime<chrono::Utc>>)> = branches
        .iter()
        .map(|b| {
            let at = b
                .commit
                .as_ref()
                .and_then(|c| c.committed_date.as_deref())
                .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
                .map(|d| d.with_timezone(&chrono::Utc));
            (b.name.clone(), at)
        })
        .collect();
    let Some(newest) = default_branch_for(sig, &dated, pushed) else {
        println!(
            "{path} has no SIG branch yet; it opens on {} for now.",
            current.as_deref().unwrap_or("GitLab's default")
        );
        return Ok(read);
    };
    if current.as_deref() == Some(newest) {
        println!("{path} opens on {newest} already.");
        return Ok(read);
    }
    println!(
        "{path} opens on {}; its newest SIG branch is {newest}.",
        current.as_deref().unwrap_or("no branch")
    );
    ui.step(&format!(
        "Make {newest} the default branch (Settings → Repository → Branch defaults)"
    ));
    if args.dry_run {
        return Ok(read);
    }
    if !args.yes && !std::io::stdin().is_terminal() {
        println!("No terminal to confirm at, and no --yes: the default branch is left as it is.");
        return Ok(read);
    }
    if !confirmed(
        args,
        &format!("Make {newest} {path}'s default branch?"),
        "changing the default branch",
    )? {
        return Ok(read);
    }
    match client.update_project(&sandogasa_gitlab::ProjectUpdate {
        default_branch: Some(newest.to_string()),
        ..Default::default()
    }) {
        Ok(()) => println!("{path} now opens on {newest}."),
        // Usually the Maintainer role missing: the rest of the run
        // still stands.
        Err(e) => println!("warning: could not make {newest} the default branch: {e}"),
    }
    Ok(read)
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

/// `run`, with the GitLab instance and token passed in, and no config.
#[cfg(test)]
pub fn run_with(
    args: &SigRepoArgs,
    base: &str,
    token: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    run_configured(args, base, token, &crate::config::Config::default())
}

/// `run`, with the GitLab instance, token and config passed in.
pub fn run_configured(
    args: &SigRepoArgs,
    base: &str,
    token: Option<String>,
    config: &crate::config::Config,
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
    let merge_method = crate::config::merge_method(config, sig)?;
    let created = ensure_repo(
        &ui,
        args,
        base,
        token.as_deref(),
        sig,
        &package,
        merge_method.as_deref(),
    )?;
    let repo = args.repo.clone().unwrap_or_else(|| PathBuf::from("."));
    let path = format!("{}/{package}", sig.gitlab_group());
    // In a checkout of the package, the SIG's repo is a remote of it, by
    // its part (#102); outside one there is nothing to add it to.
    let in_checkout = remotes(&repo).is_ok_and(|(r, _)| r.package() == Some(package.as_str()));
    if change.is_none() && in_checkout {
        ensure_sig_remote(&ui, &repo, base, sig, &path)?;
    }
    if let Some(change) = &change {
        push_change(&ui, &repo, base, &path, &package, change)?;
    }
    let pushed = change.as_ref().map(|c| c.branch.as_str());
    let current = ensure_default_branch(&ui, args, base, token.as_deref(), sig, &path, pushed)?;
    if !created {
        ensure_merge_method(
            &ui,
            args,
            base,
            token.as_deref(),
            &path,
            current,
            merge_method.as_deref(),
        )?;
    }
    if let Some(change) = change {
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
        let fast_forward = server
            .mock("PUT", path)
            .match_body(mockito::Matcher::Json(
                serde_json::json!({"merge_method": "ff"}),
            ))
            .with_status(200)
            .with_body("{}")
            .expect(1)
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
        fast_forward.assert();
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

    /// rsync's SIG repo as it was on 2026-11-10: opening on Stream's
    /// c10s, with the two proposals and the stray branches it had. The
    /// newest proposal becomes the default; a dry run, and a repo
    /// already on it, change nothing.
    #[test]
    fn the_repo_is_made_to_open_on_its_newest_sig_branch() {
        let mut server = mockito::Server::new();
        let path = "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Frsync";
        let project = |default: &str| {
            format!(
                r#"{{"id": 9, "path_with_namespace": "CentOS/proposed_updates/rpms/rsync",
                    "web_url": "w", "ssh_url_to_repo": "s", "http_url_to_repo": "h",
                    "default_branch": "{default}"}}"#
            )
        };
        let get = server
            .mock("GET", path)
            .with_status(200)
            .with_body(project("c10s"))
            .create();
        let branch = |name: &str, at: &str| serde_json::json!({"name": name, "commit": {"committed_date": at}});
        server
            .mock("GET", format!("{path}/repository/branches").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(
                serde_json::json!([
                    branch("RHEL-169141", "2026-11-09T12:00:00.000+00:00"),
                    branch("c10s", "2026-11-08T12:00:00.000+00:00"),
                    branch("c10s-proposed-RHEL-303721", "2026-11-05T12:00:00.000+01:00"),
                    branch("c9s", "2026-11-09T12:00:00.000+00:00"),
                    branch("c9s-proposed-RHEL-303761", "2026-11-07T12:00:00.000+00:00"),
                ])
                .to_string(),
            )
            .create();
        server
            .mock("GET", format!("{path}/mirror/pull").as_str())
            .with_status(200)
            .with_body(r#"{"enabled": true, "mirror_branch_regex": "^c[0-9]+s$"}"#)
            .create();
        let set = server
            .mock("PUT", path)
            .match_body(mockito::Matcher::Json(serde_json::json!({
                "default_branch": "c10s-proposed-RHEL-303721"
            })))
            .with_status(200)
            .with_body("{}")
            .expect(1)
            .create();
        let mut dry = args("rsync", Some(SigName::ProposedUpdates));
        dry.dry_run = true;
        run_with(&dry, &server.url(), Some("t".to_string())).unwrap();
        run_with(
            &args("rsync", Some(SigName::ProposedUpdates)),
            &server.url(),
            Some("t".to_string()),
        )
        .unwrap();
        set.assert();
        // Already there: nothing to change.
        get.remove();
        server
            .mock("GET", path)
            .with_status(200)
            .with_body(project("c10s-proposed-RHEL-303721"))
            .create();
        run_with(
            &args("rsync", Some(SigName::ProposedUpdates)),
            &server.url(),
            Some("t".to_string()),
        )
        .unwrap();
        set.assert();
    }

    /// The next rsync CVE: its branch is pushed with a head commit older
    /// than the existing proposal's, and still becomes the default; one
    /// for an older release does not.
    #[test]
    fn the_branch_just_pushed_becomes_the_default() {
        let b = |name: &str, at: u32| (name.to_string(), at);
        let rsync = [
            b("c10s-proposed-RHEL-303721", 9),
            b("c9s-proposed-RHEL-303761", 9),
            b("c10s", 9),
        ];
        let pu = Sig::ProposedUpdates;
        assert_eq!(
            default_branch_for(pu, &rsync, Some("c10s-proposed-RHEL-400000")),
            Some("c10s-proposed-RHEL-400000"),
            "pushed, even before it is listed (a dry run)"
        );
        assert_eq!(
            default_branch_for(pu, &rsync, Some("c9s-proposed-RHEL-400001")),
            Some("c10s-proposed-RHEL-303721")
        );
        assert_eq!(
            default_branch_for(pu, &rsync, None),
            Some("c10s-proposed-RHEL-303721")
        );
        assert_eq!(
            default_branch_for(pu, &[b("c10s", 1)], Some("c10s-proposed-RHEL-1")),
            Some("c10s-proposed-RHEL-1")
        );
        assert_eq!(default_branch_for(pu, &[b("c10s", 1)], Some("c10s")), None);
    }

    /// Without the Maintainer role GitLab refuses the change; the run
    /// still succeeds, saying so.
    #[test]
    fn a_refused_default_branch_change_is_a_warning() {
        let mut server = mockito::Server::new();
        let path = "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Frsync";
        server
            .mock("GET", path)
            .with_status(200)
            .with_body(
                r#"{"id": 9, "path_with_namespace": "CentOS/proposed_updates/rpms/rsync",
                    "web_url": "w", "ssh_url_to_repo": "s", "http_url_to_repo": "h",
                    "default_branch": "c10s", "merge_method": "ff"}"#,
            )
            .create();
        server
            .mock("GET", format!("{path}/repository/branches").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(r#"[{"name": "c10s-proposed-RHEL-303721"}]"#)
            .create();
        let refused = server.mock("PUT", path).with_status(403).create();
        run_with(
            &args("rsync", Some(SigName::ProposedUpdates)),
            &server.url(),
            Some("t".to_string()),
        )
        .unwrap();
        refused.assert();
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
        let fast_forward = server
            .mock("PUT", "/api/v4/projects/CentOS%2FHyperscale%2Frpms%2Fwprof")
            .match_body(mockito::Matcher::Json(
                serde_json::json!({"merge_method": "ff"}),
            ))
            .with_status(200)
            .with_body("{}")
            .expect(1)
            .create();
        run_with(
            &args("wprof", Some(SigName::Hyperscale)),
            &server.url(),
            Some("t".into()),
        )
        .unwrap();
        created.assert();
        mirror.assert();
        fast_forward.assert();
    }

    /// The SIG's existing repo for rpm, merging with `method`, with no
    /// branches yet.
    fn existing_repo(server: &mut mockito::Server, group: &str, method: &str) -> mockito::Mock {
        let path = format!("/api/v4/projects/CentOS%2F{group}%2Frpms%2Frpm");
        server
            .mock("GET", format!("{path}/repository/branches").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body("[]")
            .create();
        server
            .mock("GET", path.as_str())
            .with_status(200)
            .with_body(format!(
                r#"{{"id": 3, "path_with_namespace": "CentOS/{group}/rpms/rpm",
                    "web_url": "w", "ssh_url_to_repo": "s", "http_url_to_repo": "h",
                    "merge_method": "{method}"}}"#
            ))
            .create()
    }

    /// An existing repo merging with a merge commit: a dry run says so
    /// and changes nothing, a run with --yes sets the SIG's method, and
    /// one already on it is left alone.
    #[test]
    fn an_existing_repo_is_made_to_merge_the_sigs_way() {
        let mut server = mockito::Server::new();
        let path = "/api/v4/projects/CentOS%2FHyperscale%2Frpms%2Frpm";
        let get = existing_repo(&mut server, "Hyperscale", "merge");
        let set = server
            .mock("PUT", path)
            .match_body(mockito::Matcher::Json(
                serde_json::json!({"merge_method": "ff"}),
            ))
            .with_status(200)
            .with_body("{}")
            .expect(1)
            .create();
        let mut dry = args("rpm", Some(SigName::Hyperscale));
        dry.dry_run = true;
        run_with(&dry, &server.url(), Some("t".into())).unwrap();
        run_with(
            &args("rpm", Some(SigName::Hyperscale)),
            &server.url(),
            Some("t".into()),
        )
        .unwrap();
        set.assert();
        get.remove();
        existing_repo(&mut server, "Hyperscale", "ff");
        run_with(
            &args("rpm", Some(SigName::Hyperscale)),
            &server.url(),
            Some("t".into()),
        )
        .unwrap();
        set.assert();
    }

    /// The config's method for a SIG wins over the SIG's own, and one
    /// GitLab does not know is an error.
    #[test]
    fn a_configured_merge_method_is_the_one_set() {
        let mut server = mockito::Server::new();
        let path = "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Frpm";
        existing_repo(&mut server, "proposed_updates", "merge");
        let set = server
            .mock("PUT", path)
            .match_body(mockito::Matcher::Json(
                serde_json::json!({"merge_method": "rebase_merge"}),
            ))
            .with_status(200)
            .with_body("{}")
            .expect(1)
            .create();
        let pu = args("rpm", Some(SigName::ProposedUpdates));
        let config: crate::config::Config =
            toml::from_str("[sigs.proposed_updates]\nmerge_method = \"rebase_merge\"\n").unwrap();
        run_configured(&pu, &server.url(), Some("t".into()), &config).unwrap();
        set.assert();
        let bad: crate::config::Config =
            toml::from_str("[sigs.proposed_updates]\nmerge_method = \"squash\"\n").unwrap();
        let err = run_configured(&pu, &server.url(), Some("t".into()), &bad).unwrap_err();
        assert!(err.to_string().contains("squash"), "{err}");
    }

    /// Without the Maintainer role GitLab refuses the change; the run
    /// still succeeds, saying so.
    #[test]
    fn a_refused_merge_method_change_is_a_warning() {
        let mut server = mockito::Server::new();
        let path = "/api/v4/projects/CentOS%2FHyperscale%2Frpms%2Frpm";
        existing_repo(&mut server, "Hyperscale", "merge");
        let refused = server.mock("PUT", path).with_status(403).create();
        run_with(
            &args("rpm", Some(SigName::Hyperscale)),
            &server.url(),
            Some("t".into()),
        )
        .unwrap();
        refused.assert();
    }

    /// A Proposed Update carried from its merge request: rsync's
    /// RHEL-303721 fix, fetched from CentOS Stream (a local bare repo, by
    /// a URL rewrite) and pushed to the SIG's repo as its branch, with a
    /// local branch tracking it. Run again, nothing is fetched or pushed.
    #[test]
    #[serial_test::serial]
    fn a_merge_request_is_pushed_to_the_sigs_repo_once() {
        use crate::test_support::{Fixture, git};
        let fx = Fixture::proposed("c10s", &[]);
        let mut server = mockito::Server::new();
        let stream_root = fx.tmp.path().join("stream");
        let stream = stream_root.join("redhat/centos-stream/rpms/rsync.git");
        std::fs::create_dir_all(&stream).unwrap();
        git(&stream, &["init", "--quiet", "--bare"]);
        fx.git(&[
            "config",
            &format!("url.{}/.insteadOf", stream_root.display()),
            &format!("{}/", server.url()),
        ]);
        // The merge request's commit, on Stream only.
        fx.git(&["checkout", "--quiet", "-b", "mr"]);
        std::fs::write(fx.repo.join("fix.patch"), "fix\n").unwrap();
        fx.git(&["add", "."]);
        fx.git(&["commit", "--quiet", "-m", "Fix CVE"]);
        let head = fx.git(&["rev-parse", "HEAD"]);
        fx.git(&[
            "push",
            "--quiet",
            &stream.display().to_string(),
            "mr:refs/merge-requests/7/head",
        ]);
        fx.git(&["checkout", "--quiet", "c10s"]);
        fx.git(&["branch", "--quiet", "-D", "mr"]);

        let mr_url = format!(
            "{}/redhat/centos-stream/rpms/rsync/-/merge_requests/7",
            server.url()
        );
        server
            .mock(
                "GET",
                "/api/v4/projects/redhat%2Fcentos-stream%2Frpms%2Frsync/merge_requests/7",
            )
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "iid": 7, "title": "Fix CVE", "state": "opened",
                    "web_url": mr_url, "source_branch": "fix", "target_branch": "c10s",
                    "description": "Resolves: RHEL-303721\n", "sha": head,
                })
                .to_string(),
            )
            .create();
        let path = "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Frsync";
        server
            .mock("GET", path)
            .with_status(200)
            .with_body(
                r#"{"id": 9, "path_with_namespace": "CentOS/proposed_updates/rpms/rsync",
                    "web_url": "w", "ssh_url_to_repo": "s", "http_url_to_repo": "h",
                    "default_branch": "c10s-proposed-RHEL-303721"}"#,
            )
            .create();
        server
            .mock("GET", format!("{path}/repository/branches").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(r#"[{"name": "c10s-proposed-RHEL-303721"}]"#)
            .create();
        let mut run = args(&mr_url, None);
        run.repo = Some(fx.repo.clone());
        run_with(&run, &server.url(), Some("t".into())).unwrap();

        let branch = "c10s-proposed-RHEL-303721";
        let on_sig = fx.git(&["ls-remote", "sig", &format!("refs/heads/{branch}")]);
        assert!(on_sig.starts_with(&head), "{on_sig}");
        assert_eq!(
            fx.git(&[
                "rev-parse",
                "--abbrev-ref",
                &format!("{branch}@{{upstream}}")
            ]),
            format!("sig/{branch}")
        );
        // At the head already: the stream repo is not needed again.
        std::fs::remove_dir_all(&stream_root).unwrap();
        run_with(&run, &server.url(), Some("t".into())).unwrap();

        // A Hyperscale --sig for a merge request is refused.
        run.sig = Some(SigName::Hyperscale);
        let err = run_with(&run, &server.url(), None).unwrap_err();
        assert!(err.to_string().contains("drop --sig"), "{err}");
    }

    /// `run` reads the config, warning about one that does not parse,
    /// and reports a failure as an exit status.
    #[test]
    #[serial_test::serial]
    fn run_warns_about_an_unreadable_config_and_fails_cleanly() {
        let fx = crate::test_support::Fixture::proposed("c10s", &[]);
        fx.config("[sigs\n");
        assert_eq!(run(&args("rsync", None)), ExitCode::FAILURE);
    }

    /// The SIG's repo is added as a remote named after the SIG to a
    /// checkout of the package that has none for it.
    #[test]
    #[serial_test::serial]
    fn a_checkout_of_the_package_gets_the_sigs_repo_as_a_remote() {
        let fx = crate::test_support::Fixture::proposed("c10s", &[]);
        fx.git(&["remote", "remove", "sig"]);
        fx.git(&[
            "remote",
            "add",
            "origin",
            "git+ssh://git@gitlab.example/redhat/centos-stream/rpms/rsync.git",
        ]);
        let mut server = mockito::Server::new();
        server
            .mock(
                "GET",
                "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Frsync",
            )
            .with_status(200)
            .with_body(
                r#"{"id": 9, "path_with_namespace": "CentOS/proposed_updates/rpms/rsync",
                    "web_url": "w", "ssh_url_to_repo": "s", "http_url_to_repo": "h"}"#,
            )
            .create();
        let mut run = args("rsync", Some(SigName::ProposedUpdates));
        run.repo = Some(fx.repo.clone());
        run_with(&run, &server.url(), None).unwrap();
        let host = server.url().replace("http://", "");
        assert_eq!(
            fx.git(&["remote", "get-url", "proposed_updates"]),
            format!("git+ssh://git@{host}/CentOS/proposed_updates/rpms/rsync.git")
        );
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
