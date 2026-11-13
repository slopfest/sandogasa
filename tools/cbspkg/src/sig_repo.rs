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
    /// CentOS Stream merge request URLs (Proposed Updates) of one
    /// package, or a package name
    #[arg(value_name = "MR_URL|PACKAGE", required = true, num_args = 1..)]
    pub targets: Vec<String>,

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

    /// Create the repo, and clone it into a -C directory that does not
    /// exist yet, without asking
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
    /// GitLab's state for it: `opened`, `merged` or `closed`.
    pub state: String,
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

/// Where a merge request is pushed from: the checkout of the package
/// at the directory given, or nothing yet, for a clone of the SIG's
/// repo to be made there.
#[derive(Debug, PartialEq)]
enum Checkout {
    Ready,
    Clone,
}

/// Which [`Checkout`] `repo` is for `package`. A directory that does
/// not exist, or is empty, can be cloned into; any other has to be a
/// checkout of the package, so a merge request is never pushed from
/// some other repository the command happened to run in. Checked before
/// GitLab is changed: git run in a directory that is not there fails
/// as if git itself were missing.
fn checkout_of(repo: &Path, package: &str) -> Result<Checkout, String> {
    let vacant = match std::fs::read_dir(repo) {
        Ok(mut entries) => entries.next().is_none(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => return Err(format!("cannot read {}: {e}", repo.display())),
    };
    if vacant {
        return Ok(Checkout::Clone);
    }
    if remotes(repo).is_ok_and(|(r, _)| r.package() == Some(package)) {
        return Ok(Checkout::Ready);
    }
    Err(format!(
        "{} is not a checkout of {package}: run sig-repo in one, or pass -C with one, \
         or with a directory that does not exist yet to clone the SIG's repo into",
        repo.display()
    ))
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

/// The package a CentOS Stream merge request URL is for, and its
/// project and iid.
fn stream_mr(mr_url: &str) -> Result<(String, String, u64), Box<dyn std::error::Error>> {
    let (_, project, iid) = sandogasa_gitlab::parse_mr_url(mr_url)?;
    let package = project
        .strip_prefix("redhat/centos-stream/rpms/")
        .filter(|p| !p.is_empty() && !p.contains('/'))
        .ok_or_else(|| format!("{mr_url} is not a CentOS Stream merge request"))?
        .to_string();
    Ok((package, project, iid))
}

/// The CentOS Stream merge requests at `mr_urls`, all of one package,
/// read from GitLab in one request: the package, and each URL with its
/// merge request, `None` for one GitLab does not have.
#[allow(clippy::type_complexity)]
fn read_stream_mrs(
    base: &str,
    mr_urls: &[String],
) -> Result<
    (
        String,
        Vec<(String, Option<sandogasa_gitlab::MergeRequest>)>,
    ),
    Box<dyn std::error::Error>,
> {
    let mut package: Option<String> = None;
    let mut project = String::new();
    let mut wanted: Vec<(String, u64)> = Vec::new();
    for url in mr_urls {
        let (p, proj, iid) = stream_mr(url)?;
        match &package {
            Some(first) if *first != p => {
                return Err(format!(
                    "{url} is a merge request for {p}, not {first}: one package per run"
                )
                .into());
            }
            Some(_) => {}
            None => package = Some(p),
        }
        project = proj;
        if !wanted.iter().any(|(_, i)| *i == iid) {
            wanted.push((url.clone(), iid));
        }
    }
    let package = package.ok_or("no merge request given")?;
    let iids: Vec<u64> = wanted.iter().map(|(_, i)| *i).collect();
    let mut mrs = Client::new(base, &project, "")?.merge_requests(&iids)?;
    let read = wanted
        .into_iter()
        .map(|(url, iid)| {
            let at = mrs.iter().position(|m| m.iid == iid);
            (url, at.map(|at| mrs.swap_remove(at)))
        })
        .collect();
    Ok((package, read))
}

/// The change a merge request carries, or why it has none: no
/// `Resolves: RHEL-…` line to name the SIG's branch after.
fn change_from(mr: sandogasa_gitlab::MergeRequest) -> Result<Change, String> {
    let key = first_resolved(mr.description.as_deref().unwrap_or("")).ok_or_else(|| {
        format!(
            "{} has no `Resolves: RHEL-…` line to name the SIG's branch after",
            mr.web_url
        )
    })?;
    Ok(Change {
        branch: sandogasa_cbs::proposed_branch(&mr.target_branch, &key),
        mr_url: mr.web_url,
        iid: mr.iid,
        head: mr.sha,
        state: mr.state,
    })
}

/// `changes`, refused when two would be pushed as the same branch.
fn distinct(changes: Vec<Change>) -> Result<Vec<Change>, String> {
    for (n, c) in changes.iter().enumerate() {
        if let Some(other) = changes[..n].iter().find(|o| o.branch == c.branch) {
            return Err(format!(
                "{} and {} would both be pushed as {}",
                other.mr_url, c.mr_url, c.branch
            ));
        }
    }
    Ok(changes)
}

/// The changes the CentOS Stream merge requests at `mr_urls` carry, all
/// for one package, in the order given. Any one GitLab does not have,
/// or that names no RHEL issue, is an error.
fn changes_of(
    base: &str,
    mr_urls: &[String],
) -> Result<(String, Vec<Change>), Box<dyn std::error::Error>> {
    let (package, read) = read_stream_mrs(base, mr_urls)?;
    let mut changes = Vec::new();
    for (url, mr) in read {
        let mr = mr.ok_or_else(|| format!("{url} is not there"))?;
        changes.push(change_from(mr)?);
    }
    Ok((package, distinct(changes)?))
}

/// The CentOS Stream merge requests of `package` that `issues` (the
/// SIG's open tracking issues) name, each URL once, in the order the
/// issues list them.
pub fn tracked_mrs(issues: &[sandogasa_gitlab::Issue], base: &str, package: &str) -> Vec<String> {
    let prefix = format!("{base}/redhat/centos-stream/rpms/{package}/-/merge_requests/");
    let mut urls: Vec<String> = Vec::new();
    for issue in issues {
        let d = issue.description.as_deref().unwrap_or("");
        for (at, _) in d.match_indices(&prefix) {
            let digits: String = d[at + prefix.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            let url = format!("{prefix}{digits}");
            if !digits.is_empty() && !urls.contains(&url) {
                urls.push(url);
            }
        }
    }
    urls
}

/// The branch to leave the checkout on: of the branches carried, the
/// newest Stream release's, the last given of several for it.
pub fn newest_carried(sig: Sig, changes: &[Change]) -> Option<&str> {
    changes
        .iter()
        .filter_map(|c| Some((sig.own_branch_major(&c.branch)?, c.branch.as_str())))
        .fold(
            None,
            |best: Option<(u32, &str)>, (major, branch)| match best {
                Some((m, _)) if m > major => best,
                _ => Some((major, branch)),
            },
        )
        .map(|(_, branch)| branch)
}

/// Check out `branch` in the checkout at `repo`. Left alone, saying
/// so: when it is checked out already; when the checkout has
/// uncommitted changes; when the local branch is behind or apart from
/// the SIG's, as one left from an earlier revision of the merge request
/// is; and when git refuses, as it does for an untracked file the branch
/// has. Everything is pushed by then, so none of these fails the run.
fn check_out_newest(
    ui: &Ui,
    repo: &Path,
    sig: Sig,
    branch: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let current = git(repo, &["branch", "--show-current"])?;
    if current == branch {
        println!("The checkout is on {branch} already.");
        return Ok(());
    }
    if !git(repo, &["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
        println!(
            "The checkout has uncommitted changes, so it stays on {current}: \
             `git checkout {branch}` once they are dealt with."
        );
        return Ok(());
    }
    if !ui.dry_run {
        let (remotes, _) = remotes(repo)?;
        if let Some(remote) = remotes.find(Role::Sig(sig)) {
            let local = git(repo, &["rev-parse", &format!("refs/heads/{branch}")]);
            let theirs = git(
                repo,
                &[
                    "rev-parse",
                    &format!("refs/remotes/{}/{branch}", remote.name),
                ],
            );
            if let (Ok(local), Ok(theirs)) = (local, theirs)
                && local != theirs
            {
                println!(
                    "The local {branch} is not at {}'s, so the checkout stays on {current}: \
                     `git checkout {branch} && git merge --ff-only {}/{branch}` to catch it up.",
                    remote.name, remote.name
                );
                return Ok(());
            }
        }
    }
    ui.step(&format!("Check out {branch}, the newest branch carried"));
    let code = ui.run_status(
        &["git", "checkout", "--quiet", branch].map(String::from),
        repo,
    )?;
    match code {
        _ if ui.dry_run => {}
        0 => println!("Checked out {branch}."),
        _ => println!("git would not check out {branch}, so the checkout stays on {current}."),
    }
    Ok(())
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
        Err(e) => eprintln!("warning: could not set {path}'s merge method to {method}: {e}"),
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
        eprintln!("No terminal to confirm at, and no --yes: the merge method is left as it is.");
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
            eprintln!(
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
        eprintln!("No terminal to confirm at, and no --yes: the default branch is left as it is.");
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
        Err(e) => eprintln!("warning: could not make {newest} the default branch: {e}"),
    }
    Ok(read)
}

/// Push the merge request's commits to the SIG's repo as its branch.
///
/// A SIG branch is the merge request's head plus any SIG-only commits
/// on top, which never go to CentOS Stream: a BuildRequires pin working
/// around CBS's buildroots, say. So a branch the head is inside of is
/// carried already, and is left as it is. When the merge request has
/// moved on and the branch has commits its new head lacks, nothing is
/// pushed, as that would need a force push: the commits are listed, and
/// the branch's name is returned, to be rebased by hand.
fn push_change(
    ui: &Ui,
    repo: &Path,
    base: &str,
    path: &str,
    package: &str,
    change: &Change,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let argv = |args: &[&str]| args.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    let remote = ensure_sig_remote(ui, repo, base, Sig::ProposedUpdates, path)?;
    let stream = format!("{base}/{}.git", sandogasa_cbs::stream_project(package));
    let mr_ref = format!("refs/merge-requests/{}/head", change.iid);
    let tracking = format!("refs/remotes/{remote}/{}", change.branch);
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
        if git(repo, &["rev-parse", "--verify", "--quiet", &tracking]).is_err() {
            ui.run_required(&argv(&["git", "fetch", &remote, &change.branch]), repo)?;
        }
        track_locally(ui, repo, &remote, &change.branch)?;
        return Ok(None);
    }
    let on_sig_already = on_sig.as_deref().is_some_and(|l| !l.trim().is_empty());
    if on_sig_already {
        // Reading the SIG's branch, to see whether the head is in it.
        ui.step(&format!(
            "Fetch {path}'s {}, to see whether it carries the merge request's head with \
             SIG-only commits on top",
            change.branch
        ));
        ui.show_command(&argv(&["git", "fetch", &remote, &change.branch]));
        if let Err(e) = git(repo, &["fetch", "--quiet", &remote, &change.branch]) {
            println!(
                "Could not fetch {} from {path} ({e}), so nothing is pushed.",
                change.branch
            );
            return Ok(Some(change.branch.clone()));
        }
        if let Some(head) = change.head.as_deref()
            && git(repo, &["merge-base", "--is-ancestor", head, &tracking]).is_ok()
        {
            let carried = git(
                repo,
                &["log", "--format=%h %s", &format!("{head}..{tracking}")],
            )?;
            println!(
                "{} in {path} is the merge request's head with {} SIG-only commit(s) on top:",
                change.branch,
                carried.lines().count()
            );
            for line in carried.lines() {
                println!("  {line}");
            }
            track_locally(ui, repo, &remote, &change.branch)?;
            return Ok(None);
        }
    }
    ui.step(&format!(
        "Fetch the merge request's commits from CentOS Stream ({mr_ref})"
    ));
    // Fetched for real in a dry run too: it changes only FETCH_HEAD, and
    // says whether the push would have to drop SIG-only commits.
    ui.show_command(&argv(&["git", "fetch", &stream, &mr_ref]));
    if let Err(e) = git(repo, &["fetch", "--quiet", &stream, &mr_ref]) {
        println!("Could not fetch {mr_ref} from CentOS Stream ({e}), so nothing is pushed.");
        return Ok(Some(change.branch.clone()));
    }
    if on_sig_already {
        let lacking = git(
            repo,
            &["log", "--format=%h %s", &format!("FETCH_HEAD..{tracking}")],
        )?;
        if !lacking.is_empty() {
            println!(
                "{} in {path} has commits the merge request's head does not:",
                change.branch
            );
            for line in lacking.lines() {
                println!("  {line}");
            }
            println!(
                "They are SIG-only commits, or the merge request's earlier commits if it was \
                 rewritten. Pushing its head would drop them, which takes a force push, so \
                 nothing is pushed. By hand, after `git fetch {stream} {mr_ref}`: if they are \
                 only the merge request's, `git push --force-with-lease {remote} \
                 FETCH_HEAD:refs/heads/{branch}`; if any are SIG-only, `git checkout {branch} \
                 && git rebase -i FETCH_HEAD`, keeping only those, then \
                 `git push --force-with-lease {remote} {branch}`.",
                branch = change.branch
            );
            return Ok(Some(change.branch.clone()));
        }
    }
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
    track_locally(ui, repo, &remote, &change.branch)?;
    Ok(None)
}

/// A local `branch` tracking `remote`'s, for the cbspkg commands that
/// work from the checked-out branch (add-pkg, build), unless there is
/// one. The run checks out the newest of them at the end.
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

/// Whether to clone the SIG's repo into `repo` to push from: no when
/// it is a checkout of the package, yes when there is nothing there yet
/// and cloning is agreed to (or only shown, in a dry run). For merge
/// requests given by URL it is asked before anything on GitLab changes;
/// for those found in tracking issues, once the repo is known to exist,
/// since only an existing repo has issues.
fn confirm_clone(
    args: &SigRepoArgs,
    repo: &Path,
    path: &str,
    package: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    if checkout_of(repo, package)? == Checkout::Ready {
        return Ok(false);
    }
    if !args.dry_run
        && !confirmed(
            args,
            &format!("Clone {path} into {}?", repo.display()),
            &format!("cloning {path} into {}", repo.display()),
        )?
    {
        return Err(format!("{path} not cloned: pass -C with a checkout of {package}").into());
    }
    Ok(true)
}

/// The changes the merge requests `issues` track for `package` carry.
/// One that is gone, closed without being merged, or names no RHEL
/// issue is left out, saying why: a tracking issue outlives a merge
/// request replaced by another.
fn tracked_changes(
    base: &str,
    path: &str,
    package: &str,
    issues: &[sandogasa_gitlab::Issue],
) -> Result<Vec<Change>, Box<dyn std::error::Error>> {
    let urls = tracked_mrs(issues, base, package);
    if urls.is_empty() {
        println!("{path} has no open tracking issue naming a merge request of {package}.");
        return Ok(Vec::new());
    }
    sandogasa_cli::require_tools(&[("git", "sudo dnf install git-core", Some("--version"))])?;
    let (_, read) = read_stream_mrs(base, &urls)?;
    println!("{path}'s tracking issues name:");
    let mut kept = Vec::new();
    for (url, mr) in read {
        let Some(mr) = mr else {
            println!("  {url} (not there: skipped)");
            continue;
        };
        if mr.state == "closed" {
            println!("  {url} (closed without being merged in CentOS Stream: skipped)");
            continue;
        }
        match change_from(mr) {
            Ok(change) => {
                println!("  {url} ({}, as {})", change.state, change.branch);
                kept.push(change);
            }
            Err(why) => println!("  {url} (skipped: {why})"),
        }
    }
    Ok(distinct(kept)?)
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
    let urls = args.targets.iter().filter(|t| t.contains("://")).count();
    let (sig, package, mut changes) = if urls > 0 {
        if urls < args.targets.len() {
            return Err("give merge request URLs or one package, not both".into());
        }
        if args.sig.is_some_and(|s| s != SigName::ProposedUpdates) {
            return Err("a CentOS Stream merge request is a Proposed Update: drop --sig".into());
        }
        sandogasa_cli::require_tools(&[("git", "sudo dnf install git-core", Some("--version"))])?;
        let (package, changes) = changes_of(base, &args.targets)?;
        (Sig::ProposedUpdates, package, changes)
    } else {
        let [package] = args.targets.as_slice() else {
            return Err("one package at a time".into());
        };
        let sig = args
            .sig
            .ok_or("say which SIG the repo is for: --sig hyperscale or --sig proposed-updates")?;
        (Sig::from(sig), package.clone(), Vec::new())
    };
    // Proposed Updates for the package, by its merge requests, rather
    // than one by one.
    let from_issues = urls == 0 && sig == Sig::ProposedUpdates;
    let repo = args.repo.clone().unwrap_or_else(|| PathBuf::from("."));
    let path = format!("{}/{package}", sig.gitlab_group());
    let mut clone = !changes.is_empty() && confirm_clone(args, &repo, &path, &package)?;
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
    // The SIG's open tracking issues: where a package's merge requests
    // are found, and what each one carried is reported against. Not
    // being able to read them is an error when they say what to carry,
    // and only leaves the report out otherwise.
    let issues = if from_issues || !changes.is_empty() {
        match Client::new(base, &path, token.as_deref().unwrap_or(""))?
            .list_issues("cpu-sig-tracker", Some("opened"))
        {
            Ok(issues) => Some(issues),
            Err(e) if from_issues => {
                return Err(format!("cannot read {path}'s tracking issues: {e}").into());
            }
            Err(e) => {
                eprintln!("warning: cannot read {path}'s tracking issues ({e})");
                None
            }
        }
    } else {
        None
    };
    if from_issues {
        changes = tracked_changes(base, &path, &package, issues.as_deref().unwrap_or(&[]))?;
        // Without a checkout to push from, the repo's settings are still
        // seen to; only the carrying is left out.
        if !changes.is_empty() {
            match checkout_of(&repo, &package) {
                Ok(_) => clone = confirm_clone(args, &repo, &path, &package)?,
                Err(why) => {
                    println!("{why}; the merge requests are not carried.");
                    changes.clear();
                }
            }
        }
    }
    if clone {
        let host = base.split_once("://").map_or(base, |(_, h)| h);
        ui.step(&format!(
            "Clone the SIG's repo into {}, to push the merge request from",
            repo.display()
        ));
        ui.run_required(
            &[
                "git".to_string(),
                "clone".to_string(),
                format!("git+ssh://git@{host}/{path}.git"),
                repo.display().to_string(),
            ],
            Path::new("."),
        )?;
    }
    // In a checkout of the package, the SIG's repo is a remote of it, by
    // its part (#102); outside one there is nothing to add it to.
    let in_checkout = remotes(&repo).is_ok_and(|(r, _)| r.package() == Some(package.as_str()));
    if changes.is_empty() && in_checkout {
        ensure_sig_remote(&ui, &repo, base, sig, &path)?;
    }
    // Branches whose SIG-only commits need rebasing by hand: the rest
    // of the run goes on, and it fails at the end.
    let mut stuck: Vec<String> = Vec::new();
    for change in &changes {
        if clone && args.dry_run {
            println!(
                "{} would then be pushed from {} as {}.",
                change.mr_url,
                repo.display(),
                change.branch
            );
        } else if let Some(branch) = push_change(&ui, &repo, base, &path, &package, change)? {
            stuck.push(branch);
        }
    }
    // A branch left as it was, for rebasing by hand, is not the newest
    // carried: it lacks the merge request's head.
    let carried: Vec<Change> = changes
        .iter()
        .filter(|c| !stuck.contains(&c.branch))
        .cloned()
        .collect();
    let pushed =
        newest_carried(sig, &carried).or_else(|| carried.last().map(|c| c.branch.as_str()));
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
    if let Some(newest) = pushed
        && !(clone && args.dry_run)
    {
        check_out_newest(&ui, &repo, sig, newest)?;
    }
    // The tracking issue next, unless the SIG has one for it.
    if let Some(issues) = issues {
        if !changes.is_empty() {
            println!();
        }
        for change in &changes {
            match tracking_issue(&issues, &change.mr_url) {
                Some(i) => println!("{} is already tracked in {}.", change.branch, i.web_url),
                None => println!(
                    "Next, the SIG's tracking issue for {}: cpu-sig-tracker file-issue {}",
                    change.branch, change.mr_url
                ),
            }
        }
    }
    if stuck.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "not pushed, as the SIG branch has commits the merge request's head lacks, or could \
             not be read: {}",
            stuck.join(", ")
        )
        .into())
    }
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
            targets: vec![target.to_string()],
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
        no_tracking_issues(&mut server);
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
        no_tracking_issues(&mut server);
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
        no_tracking_issues(&mut server);
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
        no_tracking_issues(&mut server);
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
                "/api/v4/projects/redhat%2Fcentos-stream%2Frpms%2Frsync/merge_requests",
            )
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(
                serde_json::json!([{
                    "iid": 7, "title": "Fix CVE", "state": "opened",
                    "web_url": mr_url, "source_branch": "fix", "target_branch": "c10s",
                    "description": "Resolves: RHEL-303721\n", "sha": head,
                }])
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

    #[test]
    #[serial_test::serial]
    fn a_merge_request_needs_the_packages_checkout_or_room_for_one() {
        let fx = crate::test_support::Fixture::proposed("c10s", &[]);
        assert_eq!(checkout_of(&fx.repo, "rsync"), Ok(Checkout::Ready));
        let missing = fx.tmp.path().join("rpms/rsync");
        assert_eq!(checkout_of(&missing, "rsync"), Ok(Checkout::Clone));
        let empty = fx.tmp.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        assert_eq!(checkout_of(&empty, "rsync"), Ok(Checkout::Clone));
        // Another package's checkout, or no checkout at all.
        let err = checkout_of(&fx.repo, "git").unwrap_err();
        assert!(err.contains("is not a checkout of git"), "{err}");
        let err = checkout_of(&fx.tmp.path().join("bin"), "rsync").unwrap_err();
        assert!(err.contains("a directory that does not exist yet"), "{err}");
    }

    /// The Proposed Update of the test above, with `-C` naming a
    /// directory that is not there yet: the SIG's repo is cloned into
    /// it and the merge request pushed from the clone. A dry run only
    /// says it would.
    #[test]
    #[serial_test::serial]
    fn a_merge_request_is_pushed_from_a_fresh_clone_of_the_sigs_repo() {
        use crate::test_support::{Fixture, git};
        let fx = Fixture::proposed("c10s", &[]);
        let mut server = mockito::Server::new();
        let stream_root = fx.tmp.path().join("stream");
        let stream = stream_root.join("redhat/centos-stream/rpms/rsync.git");
        std::fs::create_dir_all(&stream).unwrap();
        git(&stream, &["init", "--quiet", "--bare"]);
        // Stream by its https URL, the SIG's repo by its SSH one, both
        // local, for any repository: the clone has no config of its own.
        let host = server.url().trim_start_matches("http://").to_string();
        let gitconfig = fx.tmp.path().join("gitconfig");
        let config = format!(
            "[url \"{stream}/\"]\n\tinsteadOf = {base}/\n\
             [url \"{tmp}/host:\"]\n\tinsteadOf = git+ssh://git@{host}/\n",
            stream = stream_root.display(),
            base = server.url(),
            tmp = fx.tmp.path().display(),
        );
        std::fs::write(&gitconfig, config).unwrap();
        // SAFETY: serial test; the fixture restores the variable on drop.
        unsafe { std::env::set_var("GIT_CONFIG_GLOBAL", &gitconfig) };
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

        let mr_url = format!(
            "{}/redhat/centos-stream/rpms/rsync/-/merge_requests/7",
            server.url()
        );
        server
            .mock(
                "GET",
                "/api/v4/projects/redhat%2Fcentos-stream%2Frpms%2Frsync/merge_requests",
            )
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(
                serde_json::json!([{
                    "iid": 7, "title": "Fix CVE", "state": "opened",
                    "web_url": mr_url, "source_branch": "fix", "target_branch": "c10s",
                    "description": "Resolves: RHEL-303721\n", "sha": head,
                }])
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
        let clone = fx.tmp.path().join("rpms/rsync");
        let mut run = args(&mr_url, None);
        run.repo = Some(clone.clone());

        // A dry run clones nothing.
        run.dry_run = true;
        run_with(&run, &server.url(), Some("t".into())).unwrap();
        assert!(!clone.exists());

        run.dry_run = false;
        run_with(&run, &server.url(), Some("t".into())).unwrap();
        let branch = "c10s-proposed-RHEL-303721";
        let on_sig = git(
            &clone,
            &["ls-remote", "origin", &format!("refs/heads/{branch}")],
        );
        assert!(on_sig.starts_with(&head), "{on_sig}");
        assert_eq!(
            git(
                &clone,
                &[
                    "rev-parse",
                    "--abbrev-ref",
                    &format!("{branch}@{{upstream}}")
                ]
            ),
            format!("origin/{branch}")
        );
    }

    /// The SIG repos the tests use, with no tracking issue open.
    fn no_tracking_issues(server: &mut mockito::Server) {
        for package in ["rpm", "rsync"] {
            server
                .mock(
                    "GET",
                    format!("/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2F{package}/issues")
                        .as_str(),
                )
                .match_query(mockito::Matcher::Any)
                .with_status(200)
                .with_body("[]")
                .create();
        }
    }

    fn issue(iid: u64, description: &str) -> sandogasa_gitlab::Issue {
        serde_json::from_value(serde_json::json!({
            "iid": iid, "title": "t", "description": description,
            "state": "opened", "web_url": format!("https://gitlab.com/i/{iid}"),
        }))
        .unwrap()
    }

    #[test]
    fn a_packages_merge_requests_are_found_in_its_tracking_issues() {
        let mr = "https://gitlab.com/redhat/centos-stream/rpms/git/-/merge_requests/";
        let issues = [
            issue(2, &format!("Proposed update: {mr}33\nSee also {mr}33.")),
            issue(
                1,
                &format!(
                    "{mr}32, and rsync's https://gitlab.com/redhat/centos-stream/rpms/rsync/-/merge_requests/7"
                ),
            ),
            issue(3, &format!("{mr} (not filed yet)")),
        ];
        assert_eq!(
            tracked_mrs(&issues, "https://gitlab.com", "git"),
            [format!("{mr}33"), format!("{mr}32")]
        );
        assert!(tracked_mrs(&issues, "https://gitlab.com", "bash").is_empty());
    }

    fn change(branch: &str) -> Change {
        Change {
            mr_url: format!("u/{branch}"),
            iid: 1,
            branch: branch.to_string(),
            head: None,
            state: "opened".to_string(),
        }
    }

    #[test]
    fn the_newest_branch_carried_is_the_newest_releases_last() {
        let sig = Sig::ProposedUpdates;
        let c = |names: &[&str]| names.iter().map(|n| change(n)).collect::<Vec<_>>();
        assert_eq!(
            newest_carried(sig, &c(&["c9s-proposed-RHEL-2", "c10s-proposed-RHEL-1"])),
            Some("c10s-proposed-RHEL-1")
        );
        assert_eq!(
            newest_carried(sig, &c(&["c10s-proposed-RHEL-1", "c9s-proposed-RHEL-2"])),
            Some("c10s-proposed-RHEL-1")
        );
        assert_eq!(
            newest_carried(sig, &c(&["c10s-proposed-RHEL-1", "c10s-proposed-RHEL-3"])),
            Some("c10s-proposed-RHEL-3")
        );
        assert_eq!(newest_carried(sig, &[]), None);
    }

    #[test]
    fn merge_requests_of_one_package_or_a_package_alone() {
        let stream = "https://gitlab.com/redhat/centos-stream/rpms";
        let refused = |targets: &[&str]| {
            let mut run = args(targets[0], None);
            run.targets = targets.iter().map(|t| t.to_string()).collect();
            run_with(&run, "http://127.0.0.1:9", None)
                .unwrap_err()
                .to_string()
        };
        let err = refused(&[
            &format!("{stream}/git/-/merge_requests/33"),
            &format!("{stream}/rsync/-/merge_requests/7"),
        ]);
        assert!(
            err.contains("for rsync, not git: one package per run"),
            "{err}"
        );
        let err = refused(&[&format!("{stream}/git/-/merge_requests/33"), "git"]);
        assert!(err.contains("URLs or one package, not both"), "{err}");
        let err = refused(&["git", "rsync"]);
        assert!(err.contains("one package at a time"), "{err}");
    }

    #[test]
    #[serial_test::serial]
    fn the_newest_branch_is_checked_out_unless_work_is_in_progress() {
        let fx = crate::test_support::Fixture::proposed("c10s", &[]);
        fx.git(&["branch", "c10s-proposed-RHEL-1"]);
        let ui = Ui {
            explain: false,
            dry_run: false,
            quiet: true,
        };
        std::fs::write(fx.repo.join("rsync.spec"), "Name: changed\n").unwrap();
        check_out_newest(&ui, &fx.repo, Sig::ProposedUpdates, "c10s-proposed-RHEL-1").unwrap();
        assert_eq!(fx.git(&["branch", "--show-current"]), "c10s");
        fx.git(&["checkout", "--quiet", "--", "rsync.spec"]);
        check_out_newest(&ui, &fx.repo, Sig::ProposedUpdates, "c10s-proposed-RHEL-1").unwrap();
        assert_eq!(
            fx.git(&["branch", "--show-current"]),
            "c10s-proposed-RHEL-1"
        );
        // On it already: nothing to do.
        check_out_newest(&ui, &fx.repo, Sig::ProposedUpdates, "c10s-proposed-RHEL-1").unwrap();
        // A local branch behind the SIG's, from an earlier revision, is
        // not checked out.
        fx.git(&["checkout", "--quiet", "c10s"]);
        fx.git(&["commit", "--quiet", "--allow-empty", "-m", "newer"]);
        fx.git(&["push", "--quiet", "sig", "c10s:c10s-proposed-RHEL-1"]);
        fx.git(&["fetch", "--quiet", "sig"]);
        check_out_newest(&ui, &fx.repo, Sig::ProposedUpdates, "c10s-proposed-RHEL-1").unwrap();
        assert_eq!(fx.git(&["branch", "--show-current"]), "c10s");
        // An untracked file the branch has makes git refuse: no error.
        fx.git(&["branch", "--quiet", "-f", "c10s-proposed-RHEL-2", "c10s"]);
        fx.git(&["checkout", "--quiet", "-b", "other", "c10s~1"]);
        std::fs::write(fx.repo.join("blocker"), "mine\n").unwrap();
        fx.git(&["checkout", "--quiet", "c10s-proposed-RHEL-2"]);
        std::fs::write(fx.repo.join("blocker"), "theirs\n").unwrap();
        fx.git(&["add", "blocker"]);
        fx.git(&["commit", "--quiet", "-m", "blocker"]);
        fx.git(&["checkout", "--quiet", "other"]);
        std::fs::write(fx.repo.join("blocker"), "mine\n").unwrap();
        check_out_newest(&ui, &fx.repo, Sig::ProposedUpdates, "c10s-proposed-RHEL-2").unwrap();
        assert_eq!(fx.git(&["branch", "--show-current"]), "other");
    }

    /// git's two Proposed Updates, found from the SIG's tracking issues
    /// rather than named: c9s and c10s are both pushed, a closed merge
    /// request is skipped, and the checkout ends on the c10s branch.
    #[test]
    #[serial_test::serial]
    fn a_packages_proposed_updates_are_carried_from_its_tracking_issues() {
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
        let mut heads = Vec::new();
        for (iid, release) in [(6, "c9s"), (7, "c10s")] {
            fx.git(&["checkout", "--quiet", "-b", &format!("mr{iid}"), "c10s"]);
            std::fs::write(fx.repo.join("fix.patch"), format!("{release}\n")).unwrap();
            fx.git(&["add", "."]);
            fx.git(&["commit", "--quiet", "-m", &format!("Fix CVE in {release}")]);
            heads.push(fx.git(&["rev-parse", "HEAD"]));
            fx.git(&[
                "push",
                "--quiet",
                &stream.display().to_string(),
                &format!("mr{iid}:refs/merge-requests/{iid}/head"),
            ]);
        }
        fx.git(&["checkout", "--quiet", "c10s"]);
        let url = server.url();
        let mr = |iid: u64| format!("{url}/redhat/centos-stream/rpms/rsync/-/merge_requests/{iid}");
        server
            .mock(
                "GET",
                "/api/v4/projects/redhat%2Fcentos-stream%2Frpms%2Frsync/merge_requests",
            )
            .match_query(mockito::Matcher::Regex(
                "iids%5B%5D=6&iids%5B%5D=7&iids%5B%5D=5&iids%5B%5D=8".into(),
            ))
            .with_status(200)
            .with_body(
                serde_json::json!([
                    {"iid": 7, "title": "Fix", "state": "opened", "web_url": mr(7),
                     "source_branch": "a", "target_branch": "c10s",
                     "description": "Resolves: RHEL-303721\n", "sha": heads[1]},
                    {"iid": 6, "title": "Fix", "state": "opened", "web_url": mr(6),
                     "source_branch": "b", "target_branch": "c9s",
                     "description": "Resolves: RHEL-303722\n", "sha": heads[0]},
                    // Replaced by !6: it would be pushed as the same branch.
                    {"iid": 5, "title": "Old", "state": "closed", "web_url": mr(5),
                     "source_branch": "c", "target_branch": "c9s",
                     "description": "Resolves: RHEL-303722\n"},
                ])
                .to_string(),
            )
            .expect(2)
            .create();
        let path = "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Frsync";
        server
            .mock("GET", format!("{path}/issues").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(
                serde_json::json!([
                    {"iid": 2, "title": "c9s", "state": "opened", "web_url": "i2",
                     "description": format!("Proposed update: {}", mr(6))},
                    {"iid": 1, "title": "c10s", "state": "opened", "web_url": "i1",
                     "description": format!("Proposed update: {}", mr(7))},
                    {"iid": 3, "title": "old", "state": "opened", "web_url": "i3",
                     "description": format!("Proposed update: {}, then {}", mr(5), mr(8))},
                ])
                .to_string(),
            )
            .create();
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
            .with_body(
                r#"[{"name": "c10s-proposed-RHEL-303721"}, {"name": "c9s-proposed-RHEL-303722"}]"#,
            )
            .create();
        let mut run = args("rsync", Some(SigName::ProposedUpdates));
        // From somewhere that is not a checkout: the repo is seen to,
        // nothing is carried.
        run.repo = Some(fx.bin.clone());
        run_with(&run, &server.url(), Some("t".into())).unwrap();
        assert!(
            fx.git(&["ls-remote", "sig", "refs/heads/c9s-proposed-RHEL-303722"])
                .is_empty()
        );
        run.repo = Some(fx.repo.clone());
        run_with(&run, &server.url(), Some("t".into())).unwrap();

        for (branch, head) in [
            ("c9s-proposed-RHEL-303722", &heads[0]),
            ("c10s-proposed-RHEL-303721", &heads[1]),
        ] {
            let on_sig = fx.git(&["ls-remote", "sig", &format!("refs/heads/{branch}")]);
            assert!(on_sig.starts_with(head.as_str()), "{branch}: {on_sig}");
        }
        assert!(
            fx.git(&["ls-remote", "sig", "refs/heads/c9s-proposed-RHEL-1"])
                .is_empty()
        );
        assert_eq!(
            fx.git(&["branch", "--show-current"]),
            "c10s-proposed-RHEL-303721"
        );
    }

    #[test]
    #[serial_test::serial]
    fn tracking_issues_that_cannot_be_read_stop_a_package_run() {
        let _fx = crate::test_support::Fixture::proposed("c10s", &[]);
        let mut server = mockito::Server::new();
        let path = "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Frsync";
        server
            .mock("GET", path)
            .with_status(200)
            .with_body(
                r#"{"id": 9, "path_with_namespace": "CentOS/proposed_updates/rpms/rsync",
                    "web_url": "w", "ssh_url_to_repo": "s", "http_url_to_repo": "h",
                    "default_branch": "c10s"}"#,
            )
            .create();
        server
            .mock("GET", format!("{path}/issues").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(502)
            .create();
        let run = args("rsync", Some(SigName::ProposedUpdates));
        let err = run_with(&run, &server.url(), Some("t".into()))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("cannot read CentOS/proposed_updates/rpms/rsync's tracking issues"),
            "{err}"
        );
    }

    /// A SIG branch carrying a SIG-only commit on top of the merge
    /// request's head (git's c9s pin for CBS's buildroot) is left as it
    /// is. Once the merge request moves on without that commit, nothing
    /// is pushed, and the run says to rebase by hand.
    #[test]
    #[serial_test::serial]
    fn a_sig_only_commit_on_top_of_the_merge_request_is_carried() {
        use crate::test_support::{Fixture, git};
        let fx = Fixture::proposed("c10s", &[]);
        let mut server = mockito::Server::new();
        no_tracking_issues(&mut server);
        let stream_root = fx.tmp.path().join("stream");
        let stream = stream_root.join("redhat/centos-stream/rpms/rsync.git");
        std::fs::create_dir_all(&stream).unwrap();
        git(&stream, &["init", "--quiet", "--bare"]);
        fx.git(&[
            "config",
            &format!("url.{}/.insteadOf", stream_root.display()),
            &format!("{}/", server.url()),
        ]);
        let mr_commit = |message: &str, file: &str| {
            std::fs::write(fx.repo.join(file), format!("{message}\n")).unwrap();
            fx.git(&["add", "."]);
            fx.git(&["commit", "--quiet", "-m", message]);
            let head = fx.git(&["rev-parse", "HEAD"]);
            fx.git(&[
                "push",
                "--quiet",
                "--force",
                &stream.display().to_string(),
                "HEAD:refs/merge-requests/7/head",
            ]);
            head
        };
        fx.git(&["checkout", "--quiet", "-b", "mr", "c10s"]);
        let head = mr_commit("Fix CVE", "fix.patch");
        let url = server.url();
        let mr_url = format!("{url}/redhat/centos-stream/rpms/rsync/-/merge_requests/7");
        let serve_mr = |server: &mut mockito::Server, sha: &str| {
            server
                .mock(
                    "GET",
                    "/api/v4/projects/redhat%2Fcentos-stream%2Frpms%2Frsync/merge_requests",
                )
                .match_query(mockito::Matcher::Any)
                .with_status(200)
                .with_body(
                    serde_json::json!([{
                        "iid": 7, "title": "Fix CVE", "state": "opened",
                        "web_url": mr_url, "source_branch": "fix", "target_branch": "c10s",
                        "description": "Resolves: RHEL-303721\n", "sha": sha,
                    }])
                    .to_string(),
                )
                .create()
        };
        let mr = serve_mr(&mut server, &head);
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
        let branch = "c10s-proposed-RHEL-303721";
        let mut run = args(&mr_url, None);
        run.repo = Some(fx.repo.clone());
        run_with(&run, &server.url(), Some("t".into())).unwrap();

        // The SIG-only pin, on top of the merge request's head.
        fx.git(&["checkout", "--quiet", branch]);
        std::fs::write(fx.repo.join("pin"), "ruby(release) < 3.1\n").unwrap();
        fx.git(&["add", "pin"]);
        fx.git(&[
            "commit",
            "--quiet",
            "-m",
            "Pin Ruby 3 for CBS's el9 buildroot",
        ]);
        let pinned = fx.git(&["rev-parse", "HEAD"]);
        fx.git(&["push", "--quiet", "sig", branch]);
        run_with(&run, &server.url(), Some("t".into())).unwrap();
        let on_sig = |fx: &Fixture| fx.git(&["ls-remote", "sig", &format!("refs/heads/{branch}")]);
        assert!(on_sig(&fx).starts_with(&pinned), "{}", on_sig(&fx));

        // The merge request moves on without the pin.
        fx.git(&["checkout", "--quiet", "mr"]);
        let moved = mr_commit("Fix CVE, take 2", "fix2.patch");
        mr.remove();
        serve_mr(&mut server, &moved);
        fx.git(&["checkout", "--quiet", branch]);
        let err = run_with(&run, &server.url(), Some("t".into()))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("has commits the merge request's head lacks"),
            "{err}"
        );
        assert!(err.contains(branch), "{err}");
        assert!(on_sig(&fx).starts_with(&pinned), "{}", on_sig(&fx));
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
        no_tracking_issues(&mut server);
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
        no_tracking_issues(&mut server);
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
