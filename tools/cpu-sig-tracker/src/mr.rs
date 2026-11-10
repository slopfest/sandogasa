// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `mr` subcommand: open the CentOS Stream merge request for a Proposed
//! Update's branch, from the package's dist-git checkout.
//!
//! The steps are the ones a person would take: fork the CentOS Stream
//! project into their own namespace (`centpkg fork`, which also adds
//! the fork as a remote), push the branch there, and open the merge
//! request against the CentOS Stream branch. So, as with dbranch,
//! `--explain` narrates each step and shows the command before running
//! it, and `--dry-run` shows them without running anything.
//!
//! The merge request is written from the branch's commits in CentOS
//! Stream's template: their messages as the summary of changes, and the
//! `Resolves:` lines naming the Jira issues, which the template
//! requires, so the issues have to be filed first (`jira --file`).

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use sandogasa_cbs::remotes::{Remotes, Role, project_path, role_of};
use sandogasa_cli::explain::Ui;
use sandogasa_gitlab::NewMergeRequest;

/// The GitLab group CentOS Stream's packages live in.

#[derive(clap::Args)]
pub struct MrArgs {
    /// The branch to propose (default: the one checked out)
    pub branch: Option<String>,

    /// The package's dist-git checkout (default: the current
    /// directory)
    #[arg(short = 'C', long, value_name = "DIR", help_heading = "Selection")]
    pub repo: Option<PathBuf>,

    // Added to the summary of changes after the commits' messages: what
    // does not belong in dist-git history, such as what was left out
    // and how the change was tested.
    #[command(flatten, next_help_heading = "Merge request")]
    pub notes: sandogasa_cli::NotesArgs,

    /// CentOS Stream branch to merge into (default: the one the
    /// branch tracks, else its cNs- prefix)
    #[arg(long, value_name = "BRANCH", help_heading = "Merge request")]
    pub target: Option<String>,

    /// Merge request title (default: the subject of the branch's one
    /// commit; needed when it has several)
    #[arg(long, value_name = "TEXT", help_heading = "Merge request")]
    pub title: Option<String>,

    /// Show each step and its command without running anything
    #[arg(long, help_heading = "Writing")]
    pub dry_run: bool,

    /// Open the merge request without asking
    #[arg(short = 'y', long, help_heading = "Writing")]
    pub yes: bool,

    /// Narrate each step and show its command, pausing before each
    #[arg(long, help_heading = "Output")]
    pub explain: bool,
}

/// A commit on the branch: its subject and the rest of its message.
#[derive(Debug, Clone, PartialEq)]
pub struct BranchCommit {
    pub subject: String,
    pub body: String,
}

/// The remote among `remotes` (`(name, url)`) that points at a fork of
/// `package` in someone's own namespace, as `centpkg fork` names it
/// (`<user>/centos_rpms_<package>`), with the fork's project path.
pub fn fork_remote(remotes: &[(String, String)], package: &str) -> Option<(String, String)> {
    remotes.iter().find_map(|(name, url)| {
        let path = project_path(url)?;
        let (role, of) = role_of(&path)?;
        (role == Role::Fork && of == package).then(|| (name.clone(), path))
    })
}

/// Where a new fork's copy stands, from its project's `import_status`.
#[derive(Debug, PartialEq)]
pub enum ForkCopy {
    Ready,
    Copying,
    Failed,
}

/// What `import_status` says of a fork: ready once `finished` (or with
/// none at all, for a project GitLab did not import), failed on
/// `failed`, still copying otherwise.
pub fn fork_copy(import_status: Option<&str>) -> ForkCopy {
    match import_status {
        None | Some("finished" | "none") => ForkCopy::Ready,
        Some("failed") => ForkCopy::Failed,
        Some(_) => ForkCopy::Copying,
    }
}

/// Wait, a few minutes at most, for GitLab to finish copying the fork
/// at `path`.
fn wait_for_fork(base: &str, path: &str, verbose: bool) -> Result<(), String> {
    const TRIES: u32 = 100;
    let client = crate::gitlab::reader(base, path).map_err(|e| e.to_string())?;
    for i in 0..TRIES {
        let status = client
            .project()
            .map_err(|e| format!("cannot read the fork {path}: {e}"))?
            .and_then(|p| p.import_status);
        match fork_copy(status.as_deref()) {
            ForkCopy::Ready => return Ok(()),
            ForkCopy::Failed => {
                return Err(format!(
                    "GitLab failed to copy the fork {path}; delete it and try again"
                ));
            }
            ForkCopy::Copying => {
                if verbose || i == 0 {
                    eprintln!(
                        "waiting for GitLab to finish copying {path} ({})",
                        status.as_deref().unwrap_or("?")
                    );
                }
                std::thread::sleep(std::time::Duration::from_secs(3));
            }
        }
    }
    Err(format!(
        "GitLab is still copying the fork {path} after five minutes; run this again later"
    ))
}

/// The CentOS Stream branch `branch` is for: the one it tracks
/// (`origin/c9s`), else its own prefix (`c9s-CVE-2026-119741`).
pub fn target_branch(branch: &str, upstream: Option<&str>) -> Option<String> {
    let is_stream = |b: &str| {
        b.strip_prefix('c')
            .and_then(|r| r.strip_suffix('s'))
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    };
    if let Some(t) = upstream.and_then(|u| u.split_once('/')).map(|(_, b)| b)
        && is_stream(t)
    {
        return Some(t.to_string());
    }
    branch
        .split(['-', '_', '/'])
        .next()
        .filter(|p| is_stream(p))
        .map(str::to_string)
}

/// Whether `line` is a git trailer (`Resolves: RHEL-1`,
/// `Signed-off-by: …`).
fn is_trailer(line: &str) -> bool {
    line.split_once(": ").is_some_and(|(key, _)| {
        !key.is_empty()
            && key.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

/// A commit body without its trailing trailers: every final paragraph
/// made only of them, as `Resolves:` often stands apart from the
/// `Assisted-by:` and `Signed-off-by:` below it.
fn without_trailers(body: &str) -> String {
    let mut paragraphs: Vec<&str> = body.trim().split("\n\n").collect();
    while paragraphs
        .last()
        .is_some_and(|p| p.lines().all(|l| is_trailer(l.trim())))
    {
        paragraphs.pop();
    }
    paragraphs.join("\n\n").trim().to_string()
}

/// The ticket lines the template asks for, from the commits' trailers,
/// each once, in order: `Resolves: RHEL-…`, `Related: …`, `Reverts: …`.
pub fn ticket_lines(commits: &[BranchCommit]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in commits {
        for line in c.body.lines().map(str::trim) {
            let is_ticket = ["Resolves:", "Related:", "Reverts:"]
                .iter()
                .any(|k| line.starts_with(k));
            if is_ticket && !out.iter().any(|l| l == line) {
                out.push(line.to_string());
            }
        }
    }
    out
}

/// The AI-assistance disclosure that opens the description, naming each
/// agent and model the commits' `Assisted-by: <agent>:<model>` trailers
/// name; `None` when no commit has one, as for a fix reused from someone
/// else and committed without the trailer.
pub fn disclosure(commits: &[BranchCommit]) -> Option<String> {
    let mut assisted: Vec<String> = Vec::new();
    for line in commits.iter().flat_map(|c| c.body.lines()) {
        let Some(value) = line.trim().strip_prefix("Assisted-by:") else {
            continue;
        };
        let value = value.trim();
        let named = match value.split_once(':') {
            Some((agent, model)) if !agent.trim().is_empty() && !model.trim().is_empty() => {
                format!("{}, {}", agent.trim(), model.trim())
            }
            _ => value.to_string(),
        };
        if !named.is_empty() && !assisted.contains(&named) {
            assisted.push(named);
        }
    }
    (!assisted.is_empty()).then(|| {
        format!(
            "*Disclosure: this change was written with AI assistance ({}) and reviewed by me. \
             If you would rather implement it yourself, feel free to close this MR.*",
            assisted.join("; ")
        )
    })
}

/// The merge request's description, in CentOS Stream's template: the
/// commits' messages, trailers dropped, as the summary of changes,
/// followed by `notes` (`--notes`), and the ticket lines after the
/// template's instructions. When the commits name an AI assistant, the
/// [`disclosure`] comes first, before anything a reviewer reads.
pub fn description(commits: &[BranchCommit], notes: Option<&str>, tickets: &[String]) -> String {
    let mut summary: Vec<String> = match commits {
        [one] => vec![without_trailers(&one.body)],
        many => many
            .iter()
            .map(|c| {
                let body = without_trailers(&c.body);
                if body.is_empty() {
                    format!("- {}", c.subject)
                } else {
                    format!("- {}\n\n{}", c.subject, indent(&body))
                }
            })
            .collect(),
    };
    if let Some(notes) = notes {
        summary.push(notes.trim().to_string());
    }
    let opening = disclosure(commits)
        .map(|d| format!("{d}\n\n"))
        .unwrap_or_default();
    format!(
        "{opening}# Merge Request Required Information\n\n\
         ## Summary of Changes\n\n{}\n\n\
         ## Approved Development Ticket(s)\n\
         All submissions to CentOS Stream must reference a ticket in \
         [Red Hat Jira](https://issues.redhat.com/).\n\n\
         <details><summary>Click for formatting instructions</summary>\n\
         Please follow the CentOS Stream [contribution documentation]\
         (https://docs.centos.org/centos-stream-docs/contributors-guide/) for how to file \
         this ticket and have it approved.\n\n\
         List tickets each on their own line of this description using the format \
         \"Resolves: RHEL-76229\", \"Related: RHEL-76229\" or \"Reverts: RHEL-76229\", as \
         appropriate.\n\
         </details>\n\n{}\n",
        summary.join("\n\n"),
        tickets.join("\n")
    )
}

/// `text` indented for a Markdown list item.
fn indent(text: &str) -> String {
    text.lines()
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("  {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The commits a `git log --format=%s%x00%b%x1e` printed, oldest first.
pub fn parse_log(out: &str) -> Vec<BranchCommit> {
    let mut commits: Vec<BranchCommit> = out
        .split('\x1e')
        .filter_map(|rec| {
            let (subject, body) = rec.trim_start_matches('\n').split_once('\0')?;
            Some(BranchCommit {
                subject: subject.trim().to_string(),
                body: body.trim().to_string(),
            })
        })
        .collect();
    commits.reverse();
    commits
}

pub fn run(args: &MrArgs) -> ExitCode {
    match run_inner(args) {
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
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The checkout's remotes, `(name, fetch URL)`.
fn remotes(repo: &Path) -> Result<Vec<(String, String)>, String> {
    Ok(git(repo, &["remote", "-v"])?
        .lines()
        .filter(|l| l.ends_with("(fetch)"))
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            Some((parts.next()?.to_string(), parts.next()?.to_string()))
        })
        .collect())
}

/// What the checkout has to be switched back to after pushing a branch
/// it does not have checked out: the branch it was on, or the commit a
/// detached HEAD pointed at. `None` when `branch` is already checked out.
///
/// CentOS Stream's dist-git clones run `centpkg pre-push-check` as a
/// pre-push hook, which reads the spec from the pushed commit but the
/// `sources` file from the working tree. Pushed from a checkout of
/// another branch, it compares the spec with the wrong `sources` and
/// cancels the push ("Source file 'rsync-3.5.0.tar.gz' was neither
/// listed in the 'sources' file …"), so `mr` checks the branch out for
/// the push. A checkout with uncommitted changes is not switched: that
/// is refused before anything else runs.
pub fn checkout_to_restore(
    current: &str,
    head: &str,
    branch: &str,
    dirty: bool,
) -> Result<Option<String>, String> {
    if current == branch {
        return Ok(None);
    }
    let on = if current == "HEAD" {
        format!("a detached HEAD ({head})")
    } else {
        current.to_string()
    };
    if dirty {
        return Err(format!(
            "the checkout is on {on} with uncommitted changes, and pushing {branch} needs it \
             checked out: the pre-push hook (centpkg pre-push-check) reads the sources file \
             from the working tree. Commit or stash the changes, or check out {branch} first"
        ));
    }
    Ok(Some(if current == "HEAD" {
        head.to_string()
    } else {
        current.to_string()
    }))
}

/// Push `branch` to `remote` with it checked out, switching back to
/// `restore` afterwards (see [`checkout_to_restore`]), whether or not
/// the push succeeded.
fn push_checked_out(
    ui: &Ui,
    repo: &Path,
    remote: &str,
    branch: &str,
    restore: Option<&str>,
) -> Result<(), String> {
    let argv = |args: &[&str]| args.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    if restore.is_some() {
        ui.step(&format!(
            "Check out {branch} for the push: the pre-push hook (centpkg pre-push-check) \
             checks the spec against the sources file in the working tree"
        ));
        ui.run_required(&argv(&["git", "checkout", "--quiet", branch]), repo)
            .map_err(|e| e.to_string())?;
    }
    ui.step(&format!("Push {branch} to your fork"));
    let pushed = ui
        .run_required(&argv(&["git", "push", "-u", remote, branch]), repo)
        .map_err(|e| e.to_string());
    if let Some(back) = restore {
        ui.step(&format!("Switch the checkout back to {back}"));
        ui.run_required(&argv(&["git", "checkout", "--quiet", back]), repo)
            .map_err(|e| format!("{e} (the checkout is still on {branch})"))?;
    }
    pushed.map(|_| ())
}

fn run_inner(args: &MrArgs) -> Result<(), Box<dyn std::error::Error>> {
    let repo = args.repo.clone().unwrap_or_else(|| PathBuf::from("."));
    // Read first, so a mistyped path fails before anything is asked.
    let notes = args.notes.read()?;
    let ui = Ui {
        explain: args.explain,
        dry_run: args.dry_run,
        quiet: false,
    };
    let current = git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let branch = args.branch.clone().unwrap_or_else(|| current.clone());
    // Before anything is forked or asked: whether the push has to
    // switch the checkout, and whether it can.
    let restore = checkout_to_restore(
        &current,
        &git(&repo, &["rev-parse", "HEAD"])?,
        &branch,
        !git(&repo, &["status", "--porcelain", "--untracked-files=no"])?.is_empty(),
    )?;

    // Where it goes: the CentOS Stream project one of the remotes points
    // at, whatever it is called; `origin` can be a SIG's repo (#102).
    let found = Remotes::parse(&git(&repo, &["remote", "-v"])?);
    let stream = found.find(Role::Stream).cloned().ok_or_else(|| {
        let add = found.package().map_or_else(
            || {
                "git remote add stream https://gitlab.com/redhat/centos-stream/rpms/<package>.git"
                    .to_string()
            },
            |p| {
                format!(
                    "git remote add stream https://gitlab.com/redhat/centos-stream/rpms/{p}.git"
                )
            },
        );
        format!(
            "no remote points at the package's CentOS Stream repo \
             (redhat/centos-stream/rpms/…), where the merge request goes; add one:\n  {add}"
        )
    })?;
    let upstream_path = stream.path.clone();
    let package = stream.package.clone();
    let tracking = git(
        &repo,
        &[
            "rev-parse",
            "--abbrev-ref",
            &format!("{branch}@{{upstream}}"),
        ],
    )
    .ok();
    let target = match &args.target {
        Some(t) => t.clone(),
        None => target_branch(&branch, tracking.as_deref()).ok_or_else(|| {
            format!("cannot tell which CentOS Stream branch {branch} is for: pass --target")
        })?,
    };

    // What it says: the branch's commits beyond the target.
    let log = git(
        &repo,
        &[
            "log",
            "--format=%s%x00%b%x1e",
            &format!("{}/{target}..{branch}", stream.name),
        ],
    )?;
    let commits = parse_log(&log);
    if commits.is_empty() {
        return Err(format!("{branch} has nothing beyond {}/{target}", stream.name).into());
    }
    let tickets = ticket_lines(&commits);
    if tickets.is_empty() {
        return Err(format!(
            "no Resolves: line on {branch}: CentOS Stream needs a Jira issue for the merge \
             request. Find or file it with `cpu-sig-tracker jira {package} --branch {target} \
             --cve …`, and add the line to the commit message"
        )
        .into());
    }
    let title = match (&args.title, commits.as_slice()) {
        (Some(t), _) => t.clone(),
        (None, [one]) => one.subject.clone(),
        (None, many) => {
            return Err(format!(
                "{branch} has {} commits: pass --title for the merge request",
                many.len()
            )
            .into());
        }
    };
    let body = description(&commits, notes.as_deref(), &tickets);

    // Anything already proposed for the same thing, so a duplicate is
    // seen before ours is opened; it is not a reason to stop.
    let base = crate::utils::gitlab_base();
    let upstream = crate::gitlab::reader(&base, &upstream_path)?;
    let topic = crate::sync_updates::cve_ids(&title)
        .into_iter()
        .next()
        .or_else(|| {
            tickets
                .first()
                .and_then(|t| t.split_whitespace().nth(1).map(str::to_string))
        });
    if let Some(topic) = &topic {
        for mr in upstream.open_merge_requests(&target, Some(topic))? {
            eprintln!(
                "note: already open against {target} about {topic}: {} ({})",
                mr.web_url, mr.title
            );
        }
    }

    // The fork, made by centpkg when the checkout has none.
    let fork = match fork_remote(&remotes(&repo)?, &package) {
        Some(found) => Some(found),
        None => {
            // centpkg has no --version; --help exits 0.
            sandogasa_cli::require_tools(&[(
                "centpkg",
                "sudo dnf install centpkg",
                Some("--help"),
            )])?;
            ui.step(&format!(
                "Fork {upstream_path} into your own namespace, and add the fork as a remote \
                 named after your GitLab user (centpkg needs an API token in \
                 ~/.config/rpkg/centpkg.conf)"
            ));
            ui.run_required(&["centpkg".to_string(), "fork".to_string()], &repo)?;
            let made = fork_remote(&remotes(&repo)?, &package);
            // GitLab copies a new fork in the background: a push before
            // the copy is done is refused (#93).
            if let Some((_, path)) = &made
                && !args.dry_run
            {
                ui.step(&format!("Wait for GitLab to finish copying {path}"));
                wait_for_fork(&crate::utils::gitlab_base(), path, args.explain)?;
            }
            made
        }
    };
    let (remote, fork_path) = match (fork, args.dry_run) {
        (Some(f), _) => f,
        (None, true) => (
            "<your-user>".to_string(),
            format!("<your-user>/centos_rpms_{package}"),
        ),
        (None, false) => {
            return Err(format!(
                "no remote points at a fork of {package} (…/centos_rpms_{package}), even \
                 after centpkg fork"
            )
            .into());
        }
    };

    // Shown whatever the narration: under --explain (a config default
    // can turn it on) the steps say what is done, not what the merge
    // request will say.
    println!(
        "\nMerge request from {fork_path}:{branch} into {upstream_path}:{target}\n\n\
         Title: {title}\n\n{body}"
    );
    if !args.dry_run && !args.yes {
        if !std::io::stdin().is_terminal() {
            return Err(
                "opening the merge request needs a terminal to confirm at, or --yes".into(),
            );
        }
        if !sandogasa_cli::confirm("Push the branch and open this merge request?", true)? {
            println!("Nothing pushed or opened.");
            return Ok(());
        }
    }

    push_checked_out(&ui, &repo, &remote, &branch, restore.as_deref())?;

    ui.step(&format!(
        "Open the merge request into {upstream_path}:{target}, through GitLab's API (the \
         web page GitLab printed on the push does the same)"
    ));
    if args.dry_run {
        return Ok(());
    }
    let target_project = upstream
        .project()?
        .ok_or_else(|| format!("GitLab has no project {upstream_path}"))?;
    let mr = crate::gitlab::client(&base, &fork_path)?.create_merge_request(
        &NewMergeRequest::new(&branch, target_project.id, &target, &title, &body),
    )?;
    println!("\nOpened {}", mr.web_url);
    let file_issue = format!("cpu-sig-tracker file-issue {}", mr.web_url);
    match crate::file_issue::sig_repo_exists(&package) {
        Ok(false) => {
            let key = tickets
                .iter()
                .find_map(|t| t.strip_prefix("Resolves: "))
                .map(str::trim);
            println!(
                "Next: {}\nthen the SIG's tracking issue: {file_issue}",
                crate::file_issue::missing_sig_repo_help(
                    &mr.web_url,
                    &package,
                    &target,
                    Some(&branch),
                    key
                )
            );
        }
        _ => println!("Next, the SIG's tracking issue: {file_issue}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_url_names_its_project() {
        for url in [
            "git+ssh://git@gitlab.com/redhat/centos-stream/rpms/rsync.git",
            "git@gitlab.com:redhat/centos-stream/rpms/rsync.git",
            "https://gitlab.com/redhat/centos-stream/rpms/rsync",
            "https://gitlab.com/redhat/centos-stream/rpms/rsync.git/",
        ] {
            assert_eq!(
                project_path(url).as_deref(),
                Some("redhat/centos-stream/rpms/rsync"),
                "{url}"
            );
        }
    }

    #[test]
    fn the_fork_is_the_remote_centpkg_added() {
        let remotes = vec![
            (
                "origin".to_string(),
                "git+ssh://git@gitlab.com/redhat/centos-stream/rpms/rsync.git".to_string(),
            ),
            (
                "salimma".to_string(),
                "git+ssh://git@gitlab.com/salimma/centos_rpms_rsync.git".to_string(),
            ),
        ];
        assert_eq!(
            fork_remote(&remotes, "rsync"),
            Some(("salimma".into(), "salimma/centos_rpms_rsync".into()))
        );
        assert_eq!(fork_remote(&remotes, "xz"), None);
        assert_eq!(fork_remote(&remotes[..1], "rsync"), None);
    }

    #[test]
    fn the_target_is_the_tracked_branch_or_the_prefix() {
        assert_eq!(
            target_branch("c9s-CVE-2026-119741", Some("origin/c9s")).as_deref(),
            Some("c9s")
        );
        // The worktree branch tracks a scratch path, not origin: the
        // prefix decides.
        assert_eq!(
            target_branch("c10s-CVE-2026-119741", Some("/tmp/x/rsync-c10s")).as_deref(),
            Some("c10s")
        );
        assert_eq!(target_branch("c10s-fix", None).as_deref(), Some("c10s"));
        assert_eq!(target_branch("my-fix", None), None);
        assert_eq!(target_branch("cs-fix", None), None);
    }

    fn rsync_commit() -> BranchCommit {
        BranchCommit {
            subject: "Fix denial of service via crafted checksums (CVE-2026-119741)".into(),
            body: "Backport upstream commit a7404a9a.\n\nsum2 keeps its SUM_LENGTH size.\n\n\
                   Resolves: RHEL-303720\n\nAssisted-by: Claude Code:claude-opus-5-5\n\
                   Signed-off-by: Michel Lind <salimma@centosproject.org>"
                .into(),
        }
    }

    #[test]
    fn the_description_follows_the_template_with_the_tickets_at_the_end() {
        let commits = vec![rsync_commit()];
        let tickets = ticket_lines(&commits);
        assert_eq!(tickets, ["Resolves: RHEL-303720"]);
        let d = description(&commits, None, &tickets);
        assert!(
            d.starts_with(
                "*Disclosure: this change was written with AI assistance (Claude Code, \
                 claude-opus-5-5) and reviewed by me. If you would rather implement it \
                 yourself, feel free to close this MR.*\n\n\
                 # Merge Request Required Information\n\n## Summary of Changes\n\n\
                 Backport upstream commit a7404a9a.\n\nsum2 keeps its SUM_LENGTH size.\n\n\
                 ## Approved Development Ticket(s)\n"
            ),
            "{d}"
        );
        assert!(d.ends_with("</details>\n\nResolves: RHEL-303720\n"), "{d}");
        // Trailers stay out of the summary, the ticket included.
        assert!(!d.contains("Signed-off-by"), "{d}");
        assert_eq!(d.matches("RHEL-303720").count(), 1, "{d}");
    }

    #[test]
    fn the_disclosure_names_each_assistant_once_and_only_when_there_is_one() {
        let plain = BranchCommit {
            subject: "Bump release".into(),
            body: "Resolves: RHEL-303720\n\nSigned-off-by: A <a@example.org>".into(),
        };
        // A reused fix committed without the trailer: no disclosure.
        assert_eq!(disclosure(std::slice::from_ref(&plain)), None);
        let d = description(std::slice::from_ref(&plain), None, &[]);
        assert!(d.starts_with("# Merge Request Required Information"), "{d}");

        let other = BranchCommit {
            subject: "Add a test".into(),
            body: "Assisted-by: Other Agent:model-x".into(),
        };
        let line = disclosure(&[rsync_commit(), plain, rsync_commit(), other]).unwrap();
        assert!(
            line.contains("(Claude Code, claude-opus-5-5; Other Agent, model-x)"),
            "{line}"
        );
    }

    #[test]
    fn notes_end_the_summary_before_the_tickets() {
        let commits = vec![rsync_commit()];
        let tickets = ticket_lines(&commits);
        let notes = "Testing: the upstream tests fail without the patch.\n\n\
                     Not included: the --inplace overflow fix (9dc0ff63).\n";
        let d = description(&commits, Some(notes), &tickets);
        assert!(
            d.contains(
                "sum2 keeps its SUM_LENGTH size.\n\n\
                 Testing: the upstream tests fail without the patch.\n\n\
                 Not included: the --inplace overflow fix (9dc0ff63).\n\n\
                 ## Approved Development Ticket(s)\n"
            ),
            "{d}"
        );
        assert!(d.ends_with("</details>\n\nResolves: RHEL-303720\n"), "{d}");
    }

    #[test]
    fn several_commits_are_listed_by_subject() {
        let commits = vec![
            rsync_commit(),
            BranchCommit {
                subject: "Bump release".into(),
                body: "Related: RHEL-1\nResolves: RHEL-303720".into(),
            },
        ];
        assert_eq!(
            ticket_lines(&commits),
            ["Resolves: RHEL-303720", "Related: RHEL-1"]
        );
        let d = description(&commits, None, &ticket_lines(&commits));
        assert!(
            d.contains(
                "- Fix denial of service via crafted checksums (CVE-2026-119741)\n\n  \
                 Backport upstream commit a7404a9a.\n\n  sum2 keeps its SUM_LENGTH size.\n\n\
                 - Bump release\n"
            ),
            "{d}"
        );
    }

    #[test]
    fn git_log_records_come_back_oldest_first() {
        let out = "second\0body two\n\x1e\nfirst\0body one\n\nResolves: RHEL-1\n\x1e";
        let commits = parse_log(out);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].subject, "first");
        assert_eq!(commits[0].body, "body one\n\nResolves: RHEL-1");
        assert_eq!(commits[1].subject, "second");
    }

    #[test]
    fn a_branch_not_checked_out_is_checked_out_for_the_push() {
        assert_eq!(
            checkout_to_restore("c10s-x", "abc", "c10s-x", true),
            Ok(None)
        );
        assert_eq!(
            checkout_to_restore("c9s-x", "abc", "c10s-x", false),
            Ok(Some("c9s-x".into()))
        );
        // A detached HEAD goes back to its commit.
        assert_eq!(
            checkout_to_restore("HEAD", "abc123", "c10s-x", false),
            Ok(Some("abc123".into()))
        );
        let err = checkout_to_restore("c9s-x", "abc", "c10s-x", true).unwrap_err();
        assert!(
            err.contains("on c9s-x with uncommitted changes") && err.contains("check out c10s-x"),
            "{err}"
        );
    }

    /// The rsync c10s run: pushed from a checkout of the c9s branch,
    /// the pre-push hook read c9s's `sources`. Here a hook records which
    /// `sources` it saw, and the checkout must end up where it started.
    #[test]
    fn the_pre_push_hook_sees_the_pushed_branchs_working_tree() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, remote) = (dir.path().join("repo"), dir.path().join("fork.git"));
        let run = |cwd: &Path, args: &[&str]| {
            let out = Command::new("git")
                .args(["-c", "user.name=T", "-c", "user.email=t@example.org"])
                .args([
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "init.defaultBranch=c9s-x",
                ])
                .args(args)
                .current_dir(cwd)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        run(dir.path(), &["init", "--quiet", "--bare", "fork.git"]);
        run(dir.path(), &["init", "--quiet", "repo"]);
        std::fs::write(repo.join("sources"), "rsync-3.2.7\n").unwrap();
        run(&repo, &["add", "sources"]);
        run(&repo, &["commit", "--quiet", "-m", "c9s"]);
        run(&repo, &["checkout", "--quiet", "-b", "c10s-x"]);
        std::fs::write(repo.join("sources"), "rsync-3.5.0\n").unwrap();
        run(&repo, &["commit", "--quiet", "-am", "c10s"]);
        run(&repo, &["checkout", "--quiet", "c9s-x"]);
        let hook = repo.join(".git/hooks/pre-push");
        std::fs::write(&hook, "#!/bin/sh\ncat sources > ../hook-saw\n").unwrap();
        let mut perms = std::fs::metadata(&hook).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&hook, perms).unwrap();

        let ui = Ui {
            explain: false,
            dry_run: false,
            quiet: true,
        };
        let restore = checkout_to_restore(
            &run(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]),
            &run(&repo, &["rev-parse", "HEAD"]),
            "c10s-x",
            false,
        )
        .unwrap();
        push_checked_out(
            &ui,
            &repo,
            remote.to_str().unwrap(),
            "c10s-x",
            restore.as_deref(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("hook-saw")).unwrap(),
            "rsync-3.5.0\n"
        );
        assert_eq!(run(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]), "c9s-x");
        assert_eq!(
            run(
                dir.path(),
                &["--git-dir=fork.git", "log", "--format=%s", "-1", "c10s-x"]
            ),
            "c10s"
        );
    }

    #[test]
    fn a_new_fork_is_pushed_to_only_once_gitlab_has_copied_it() {
        assert_eq!(fork_copy(Some("finished")), ForkCopy::Ready);
        assert_eq!(fork_copy(None), ForkCopy::Ready);
        assert_eq!(fork_copy(Some("none")), ForkCopy::Ready);
        assert_eq!(fork_copy(Some("scheduled")), ForkCopy::Copying);
        assert_eq!(fork_copy(Some("started")), ForkCopy::Copying);
        assert_eq!(fork_copy(Some("failed")), ForkCopy::Failed);
    }

    // ---- run_inner on a local fixture checkout, against a mock GitLab ----

    use crate::test_support::{EnvGuard, install_fake_bin};
    use serde_json::json;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const STREAM: &str = "https://gitlab.com/redhat/centos-stream/rpms/rsync.git";
    const BRANCH: &str = "c9s-CVE-2026-12345";

    /// Run git in `cwd` for a fixture, its output trimmed.
    fn fixture_git(cwd: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=T", "-c", "user.email=t@example.org"])
            .args(["-c", "commit.gpgsign=false", "-c", "init.defaultBranch=c9s"])
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A dist-git checkout of rsync with `remote` as the Stream remote,
    /// `c9s` at `remote/c9s`, and a branch with one commit per message
    /// beyond it, left checked out on `c9s`.
    fn checkout(dir: &Path, remote: (&str, &str), messages: &[&str]) -> PathBuf {
        let repo = dir.join("rsync");
        fixture_git(dir, &["init", "--quiet", "rsync"]);
        std::fs::write(repo.join("rsync.spec"), "Name: rsync\n").unwrap();
        fixture_git(&repo, &["add", "rsync.spec"]);
        fixture_git(&repo, &["commit", "--quiet", "-m", "base"]);
        fixture_git(&repo, &["remote", "add", remote.0, remote.1]);
        let tracking = format!("refs/remotes/{}/c9s", remote.0);
        fixture_git(&repo, &["update-ref", &tracking, "HEAD"]);
        fixture_git(&repo, &["checkout", "--quiet", "-b", BRANCH]);
        for (i, m) in messages.iter().enumerate() {
            std::fs::write(repo.join("rsync.spec"), format!("Name: rsync\n# {i}\n")).unwrap();
            fixture_git(&repo, &["commit", "--quiet", "-am", m]);
        }
        fixture_git(&repo, &["checkout", "--quiet", "c9s"]);
        repo
    }

    const FIX: &str = "Fix a crash (CVE-2026-12345)\n\nBackport upstream's fix.\n\n\
                       Resolves: RHEL-100\n\nAssisted-by: Claude Code:claude-opus-5-5";

    fn args(repo: &Path) -> MrArgs {
        MrArgs {
            branch: Some(BRANCH.to_string()),
            repo: Some(repo.to_path_buf()),
            notes: sandogasa_cli::NotesArgs::default(),
            target: None,
            title: None,
            dry_run: false,
            yes: false,
            explain: false,
        }
    }

    /// Point the tool at `server` (when given), with fake tools from
    /// `bin` first on PATH and git reading no developer config.
    fn guard(server: Option<&MockServer>, bin: &Path) -> EnvGuard {
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let base = server.map_or_else(|| "http://127.0.0.1:9".to_string(), |s| s.uri());
        EnvGuard::new(&[
            ("PATH", &path),
            ("CPU_SIG_TRACKER_GITLAB_BASE", &base),
            ("GITLAB_TOKEN", "test-token"),
            ("XDG_CONFIG_HOME", &bin.to_string_lossy()),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
        ])
    }

    fn project(id: u64, path: &str, import_status: &str) -> serde_json::Value {
        json!({
            "id": id,
            "path_with_namespace": path,
            "web_url": format!("https://gitlab.example/{path}"),
            "ssh_url_to_repo": format!("git@gitlab.example:{path}.git"),
            "http_url_to_repo": format!("https://gitlab.example/{path}.git"),
            "import_status": import_status,
        })
    }

    fn merge_request(iid: u64, title: &str) -> serde_json::Value {
        json!({
            "iid": iid,
            "title": title,
            "state": "opened",
            "web_url": format!("https://gitlab.example/redhat/centos-stream/rpms/rsync/-/merge_requests/{iid}"),
            "source_branch": BRANCH,
            "target_branch": "c9s",
        })
    }

    #[test]
    #[serial_test::serial]
    fn a_dry_run_shows_the_merge_request_without_forking_or_opening_it() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = runtime.block_on(MockServer::start());
        runtime.block_on(async {
            // One already open about the same CVE: noted, not a stop.
            Mock::given(method("GET"))
                .and(path(
                    "/api/v4/projects/redhat%2Fcentos-stream%2Frpms%2Frsync/merge_requests",
                ))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!([merge_request(3, "CVE-2026-12345 too")])),
                )
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(500))
                .expect(0)
                .mount(&server)
                .await;
        });
        let dir = tempfile::tempdir().unwrap();
        let repo = checkout(dir.path(), ("stream", STREAM), &[FIX]);
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        // Only probed: a dry run forks nothing.
        install_fake_bin(&bin, "centpkg", &[("--help", "usage: centpkg")]);
        let _g = guard(Some(&server), &bin);

        let mut a = args(&repo);
        a.dry_run = true;
        a.notes.notes = Some("Tested with the reproducer.".into());
        run_inner(&a).expect("dry run");
        // Nothing switched or pushed.
        assert_eq!(
            fixture_git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "c9s"
        );
    }

    #[test]
    #[serial_test::serial]
    fn yes_forks_pushes_and_opens_the_merge_request() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = runtime.block_on(MockServer::start());
        runtime.block_on(async {
            Mock::given(method("GET"))
                .and(path(
                    "/api/v4/projects/redhat%2Fcentos-stream%2Frpms%2Frsync/merge_requests",
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
                .mount(&server)
                .await;
            // The new fork, already copied.
            Mock::given(method("GET"))
                .and(path("/api/v4/projects/me%2Fcentos_rpms_rsync"))
                .respond_with(ResponseTemplate::new(200).set_body_json(project(
                    9,
                    "me/centos_rpms_rsync",
                    "finished",
                )))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path(
                    "/api/v4/projects/redhat%2Fcentos-stream%2Frpms%2Frsync",
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(project(
                    5,
                    "redhat/centos-stream/rpms/rsync",
                    "none",
                )))
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path(
                    "/api/v4/projects/me%2Fcentos_rpms_rsync/merge_requests",
                ))
                .and(body_partial_json(json!({
                    "source_branch": BRANCH,
                    "target_project_id": 5,
                    "target_branch": "c9s",
                    "title": "Fix a crash (CVE-2026-12345)",
                })))
                .respond_with(
                    ResponseTemplate::new(201)
                        .set_body_json(merge_request(11, "Fix a crash (CVE-2026-12345)")),
                )
                .expect(1)
                .mount(&server)
                .await;
            // No SIG repo yet: the next step says how to make one.
            Mock::given(method("GET"))
                .and(path(
                    "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Frsync",
                ))
                .respond_with(ResponseTemplate::new(404))
                .mount(&server)
                .await;
        });
        let dir = tempfile::tempdir().unwrap();
        let repo = checkout(dir.path(), ("stream", STREAM), &[FIX]);
        let fork = dir.path().join("fork.git");
        fixture_git(dir.path(), &["init", "--quiet", "--bare", "fork.git"]);
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        // centpkg fork adds the fork as a remote; pushes go to a local
        // bare repository instead of GitLab.
        let centpkg = bin.join("centpkg");
        std::fs::write(
            &centpkg,
            format!(
                "#!/bin/sh\ncase \"$1\" in\n--help) exit 0;;\nfork) \
                 git remote add me https://gitlab.com/me/centos_rpms_rsync.git && \
                 git remote set-url --push me {}; exit $?;;\nesac\nexit 1\n",
                fork.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            &centpkg,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let _g = guard(Some(&server), &bin);

        let mut a = args(&repo);
        a.yes = true;
        run_inner(&a).expect("opens the merge request");
        assert_eq!(
            fixture_git(
                dir.path(),
                &["--git-dir=fork.git", "log", "--format=%s", "-1", BRANCH]
            ),
            "Fix a crash (CVE-2026-12345)"
        );
        // Switched back to where it was.
        assert_eq!(
            fixture_git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "c9s"
        );
    }

    #[test]
    #[serial_test::serial]
    fn a_fork_gitlab_failed_to_copy_is_not_pushed_to() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = runtime.block_on(MockServer::start());
        runtime.block_on(async {
            Mock::given(method("GET"))
                .and(path(
                    "/api/v4/projects/redhat%2Fcentos-stream%2Frpms%2Frsync/merge_requests",
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/v4/projects/me%2Fcentos_rpms_rsync"))
                .respond_with(ResponseTemplate::new(200).set_body_json(project(
                    9,
                    "me/centos_rpms_rsync",
                    "failed",
                )))
                .mount(&server)
                .await;
        });
        let dir = tempfile::tempdir().unwrap();
        let repo = checkout(dir.path(), ("stream", STREAM), &[FIX]);
        // The fork is a remote already; push it nowhere real.
        fixture_git(
            &repo,
            &[
                "remote",
                "add",
                "me",
                "https://gitlab.com/me/centos_rpms_rsync.git",
            ],
        );
        fixture_git(
            &repo,
            &["remote", "set-url", "--push", "me", "/nonexistent"],
        );
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let _g = guard(Some(&server), &bin);
        // A fork GitLab failed to copy is not waited for.
        let err = wait_for_fork(&server.uri(), "me/centos_rpms_rsync", true).unwrap_err();
        assert!(err.contains("failed to copy the fork"), "{err}");
        // The fork remote already there is used as it is, without asking
        // GitLab; a push that fails is an error, with the checkout
        // restored.
        let mut a = args(&repo);
        a.yes = true;
        assert!(run_inner(&a).is_err());
        assert_eq!(
            fixture_git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "c9s"
        );
    }

    #[test]
    #[serial_test::serial]
    fn what_the_merge_request_needs_is_checked_before_gitlab_is_asked() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        // No server: anything reaching GitLab would fail to connect.
        let _g = guard(None, &bin);
        let err_of = |a: &MrArgs| run_inner(a).unwrap_err().to_string();

        // Not a checkout at all.
        let plain = dir.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        assert!(err_of(&args(&plain)).contains("git rev-parse"));

        // Only a SIG's repo as a remote: says how to add Stream's.
        let sig = dir.path().join("sig");
        std::fs::create_dir(&sig).unwrap();
        let repo = checkout(
            &sig,
            (
                "origin",
                "https://gitlab.com/CentOS/proposed_updates/rpms/rsync.git",
            ),
            &[FIX],
        );
        let err = err_of(&args(&repo));
        assert!(
            err.contains(
                "git remote add stream https://gitlab.com/redhat/centos-stream/rpms/rsync.git"
            ),
            "{err}"
        );

        // A branch with no Stream branch in its name, nor tracking one.
        let one = dir.path().join("one");
        std::fs::create_dir(&one).unwrap();
        let repo = checkout(&one, ("stream", STREAM), &[FIX]);
        fixture_git(&repo, &["branch", "my-fix", BRANCH]);
        let mut a = args(&repo);
        a.branch = Some("my-fix".into());
        assert!(err_of(&a).contains("pass --target"));

        // Nothing beyond the target.
        a.branch = Some("c9s".into());
        assert!(err_of(&a).contains("has nothing beyond stream/c9s"));

        // A commit without a Resolves: line.
        let bare = dir.path().join("bare");
        std::fs::create_dir(&bare).unwrap();
        let repo = checkout(&bare, ("stream", STREAM), &["Fix a crash"]);
        let err = err_of(&args(&repo));
        assert!(err.contains("no Resolves: line"), "{err}");

        // Several commits need a title.
        let two = dir.path().join("two");
        std::fs::create_dir(&two).unwrap();
        let repo = checkout(
            &two,
            ("stream", STREAM),
            &[FIX, "Fix a typo\n\nRelated: RHEL-100"],
        );
        let err = err_of(&args(&repo));
        assert!(err.contains("has 2 commits: pass --title"), "{err}");

        // Uncommitted changes on another branch stop it.
        std::fs::write(repo.join("rsync.spec"), "dirty\n").unwrap();
        let err = err_of(&args(&repo));
        assert!(err.contains("uncommitted changes"), "{err}");
        assert_eq!(run(&args(&repo)), ExitCode::FAILURE);
    }
}
