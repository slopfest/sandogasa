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

use sandogasa_cli::explain::Ui;
use sandogasa_gitlab::NewMergeRequest;

/// The GitLab group CentOS Stream's packages live in.
const STREAM_GROUP: &str = "redhat/centos-stream/";

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

/// The project path a git remote URL points at, `.git` dropped:
/// `git+ssh://git@gitlab.com/redhat/centos-stream/rpms/rsync.git`,
/// `git@gitlab.com:redhat/centos-stream/rpms/rsync.git` and
/// `https://gitlab.com/redhat/centos-stream/rpms/rsync` all give
/// `redhat/centos-stream/rpms/rsync`.
pub fn project_path(url: &str) -> Option<String> {
    let rest = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/')?.1,
        // scp-like: user@host:path
        None => url.split_once(':')?.1,
    };
    let path = rest.trim_end_matches('/').trim_end_matches(".git");
    (!path.is_empty()).then(|| path.to_string())
}

/// The remote among `remotes` (`(name, url)`) that points at a fork of
/// `package` in someone's own namespace, as `centpkg fork` names it
/// (`<user>/centos_rpms_<package>`), with the fork's project path.
pub fn fork_remote(remotes: &[(String, String)], package: &str) -> Option<(String, String)> {
    let suffix = format!("/centos_rpms_{package}");
    remotes.iter().find_map(|(name, url)| {
        let path = project_path(url)?;
        (path.ends_with(&suffix) && !path.starts_with(STREAM_GROUP)).then(|| (name.clone(), path))
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

/// The merge request's description, in CentOS Stream's template: the
/// commits' messages, trailers dropped, as the summary of changes,
/// followed by `notes` (`--notes`), and the ticket lines after the
/// template's instructions.
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
    format!(
        "# Merge Request Required Information\n\n\
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

fn run_inner(args: &MrArgs) -> Result<(), Box<dyn std::error::Error>> {
    let repo = args.repo.clone().unwrap_or_else(|| PathBuf::from("."));
    // Read first, so a mistyped path fails before anything is asked.
    let notes = args.notes.read()?;
    let ui = Ui {
        explain: args.explain,
        dry_run: args.dry_run,
        quiet: false,
    };
    let branch = match &args.branch {
        Some(b) => b.clone(),
        None => git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"])?,
    };

    // Where it goes: the CentOS Stream project the checkout's origin is.
    let origin = git(&repo, &["remote", "get-url", "origin"])?;
    let upstream_path = project_path(&origin)
        .filter(|p| p.starts_with(STREAM_GROUP))
        .ok_or_else(|| {
            format!("origin is {origin}, not a CentOS Stream project ({STREAM_GROUP}…)")
        })?;
    let package = upstream_path
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string();
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
            &format!("origin/{target}..{branch}"),
        ],
    )?;
    let commits = parse_log(&log);
    if commits.is_empty() {
        return Err(format!("{branch} has nothing beyond origin/{target}").into());
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

    ui.step(&format!("Push {branch} to your fork"));
    ui.run_required(
        &[
            "git".to_string(),
            "push".to_string(),
            "-u".to_string(),
            remote.clone(),
            branch.clone(),
        ],
        &repo,
    )?;

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
    println!(
        "Next, the SIG's tracking issue: cpu-sig-tracker file-issue {}",
        mr.web_url
    );
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
                "# Merge Request Required Information\n\n## Summary of Changes\n\n\
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
    fn a_new_fork_is_pushed_to_only_once_gitlab_has_copied_it() {
        assert_eq!(fork_copy(Some("finished")), ForkCopy::Ready);
        assert_eq!(fork_copy(None), ForkCopy::Ready);
        assert_eq!(fork_copy(Some("none")), ForkCopy::Ready);
        assert_eq!(fork_copy(Some("scheduled")), ForkCopy::Copying);
        assert_eq!(fork_copy(Some("started")), ForkCopy::Copying);
        assert_eq!(fork_copy(Some("failed")), ForkCopy::Failed);
    }
}
