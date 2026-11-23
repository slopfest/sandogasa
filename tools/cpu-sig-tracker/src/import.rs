// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `import` subcommand: start a Proposed Update from another
//! distribution's fix (#233), AlmaLinux's first.
//!
//! For a CVE RHEL has not fixed yet, AlmaLinux sometimes ships its own
//! fix first. Its rpms repository keeps one branch per release (`a9`,
//! `a10`), and a fix is a commit adding a patch file and its spec
//! hunks: the `PatchNNNN:` line with the comments above it, a Release
//! with `.alma.N`, and a changelog entry. Taking it into a Proposed
//! Update is the same work for every package, so this does it from the
//! package's Proposed Updates checkout: for each release, a branch off
//! CentOS Stream's (`c9s` from `a9`), the patch files copied, the spec
//! edited the way its packager would (the patch numbered as that spec
//! numbers patches, Release raised by one, a changelog entry on top),
//! and a commit crediting AlmaLinux's author and citing its commit.
//!
//! The branches are named `cNs-proposed-<id>`: the RHEL issue when one
//! exists, else the CVE, else the advisory grouping several CVEs. No
//! Jira issue is needed for a fix that is not proposed to CentOS Stream.
//!
//! As with `mr`, the steps are git commands a person could run, so
//! `--explain` narrates each and `--dry-run` shows them without making
//! a branch or a commit. Fetching AlmaLinux's branches and Stream's is
//! done either way: it only updates remote-tracking refs, and the plan
//! cannot be shown without them.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use sandogasa_cbs::remotes::{Remotes, Role};
use sandogasa_cbs::{Sig, proposed_branch};
use sandogasa_cli::explain::Ui;
use sandogasa_distgit::spec;

/// Where a fix is taken from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Source {
    /// AlmaLinux's rpms repositories (git.almalinux.org).
    Almalinux,
}

#[derive(clap::Args)]
pub struct ImportArgs {
    /// The package (its source package name)
    pub package: String,

    /// AlmaLinux commit(s) to take (default: those on its a9/a10
    /// branches naming the CVE, since its last import)
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "SHA,...",
        help_heading = "Selection"
    )]
    pub commit: Vec<String>,

    /// The CVE(s) the fix is for (CSV or repeated)
    #[arg(
        long,
        required = true,
        value_delimiter = ',',
        value_name = "CVE,...",
        help_heading = "Selection"
    )]
    pub cve: Vec<String>,

    /// Where the fix comes from
    #[arg(
        long,
        value_enum,
        default_value = "almalinux",
        help_heading = "Selection"
    )]
    pub from: Source,

    /// CentOS Stream releases to take it into (default: each one the
    /// fix is found for, of c9s and c10s)
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "cNs,...",
        help_heading = "Selection"
    )]
    pub release: Vec<String>,

    /// Advisory grouping several CVEs (ALSA-…, RHSA-…), naming the
    /// branch when there is no RHEL issue
    #[arg(long, value_name = "ID", help_heading = "Branch")]
    pub advisory: Option<String>,

    /// RHEL issue the fix is for, naming the branch and its Resolves:
    /// line
    #[arg(long, value_name = "KEY", help_heading = "Branch")]
    pub rhel_key: Option<String>,

    /// AlmaLinux checkout of the package (default: <package> under
    /// [checkouts] almalinux, ~/src/almalinux/rpms, else its URL)
    #[arg(long, value_name = "DIR", help_heading = "Checkouts")]
    pub alma_dir: Option<PathBuf>,

    /// The package's Proposed Updates checkout, cloned when missing
    /// (default: <package> under [checkouts] proposed_updates,
    /// ~/src/centos/proposed_updates/rpms)
    #[arg(short = 'C', long, value_name = "DIR", help_heading = "Checkouts")]
    pub repo: Option<PathBuf>,

    /// Show each step and its command without making anything
    #[arg(long, help_heading = "Writing")]
    pub dry_run: bool,

    /// Make the branches and commits without asking
    #[arg(short = 'y', long, help_heading = "Writing")]
    pub yes: bool,

    /// Narrate each step and show its command, pausing before each
    #[arg(long, help_heading = "Output")]
    pub explain: bool,
}

/// What another distribution's fix adds to a spec: each patch file
/// with the comment lines above its tag, and its changelog lines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Fix {
    /// `(comments, file)`, comments without their `# `.
    pub patches: Vec<(Vec<String>, String)>,
    /// The changelog entry's lines, as written (`- …`).
    pub changelog: Vec<String>,
}

/// A fix commit found on an AlmaLinux branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundCommit {
    pub sha: String,
    pub author: String,
    pub email: String,
    pub subject: String,
    pub body: String,
}

/// The spec as it is after a fix is applied, the EVR its changelog
/// entry names, and the patch tag lines written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub spec: String,
    pub evr: String,
    pub tags: Vec<String>,
}

/// The name the branches carry after `cNs-proposed-`: the RHEL issue
/// when there is one, else the one CVE, else the advisory grouping
/// several (`ALSA-2026:1234` as `ALSA-2026-1234`, since git refuses a
/// `:` in a branch name), else the first CVE.
pub fn branch_id(
    rhel_key: Option<&str>,
    cves: &[String],
    advisory: Option<&str>,
) -> Result<String, String> {
    if let Some(key) = rhel_key {
        return Ok(key.to_string());
    }
    match (cves, advisory) {
        ([one], _) => Ok(one.clone()),
        ([_, _, ..], Some(adv)) => Ok(adv.replace(':', "-")),
        ([first, ..], None) => Ok(first.clone()),
        ([], _) => Err("no CVE given".to_string()),
    }
}

/// What AlmaLinux's commit adds to the spec, from the spec `before` and
/// `after` it: each patch tag it adds, with the comment lines right
/// above it (AlmaLinux's own "AlmaLinux Patch" header left out), and its
/// changelog entry's lines, when it adds one.
pub fn alma_fix(before: &str, after: &str) -> Fix {
    let before_patches: Vec<&str> = before.lines().filter_map(patch_file).collect();
    let lines: Vec<&str> = after.lines().collect();
    let mut patches = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(file) = patch_file(line) else {
            continue;
        };
        if before_patches.contains(&file) {
            continue;
        }
        let mut comments: Vec<String> = lines[..i]
            .iter()
            .rev()
            .take_while(|l| l.trim_start().starts_with('#'))
            .map(|l| {
                l.trim_start()
                    .trim_start_matches('#')
                    .trim_start()
                    .to_string()
            })
            .filter(|c| !c.eq_ignore_ascii_case("AlmaLinux Patch"))
            .collect();
        comments.reverse();
        patches.push((comments, file.to_string()));
    }
    let entry = |spec: &str| -> Vec<String> {
        spec.lines()
            .skip_while(|l| l.trim_end() != "%changelog")
            .skip(1)
            .skip_while(|l| !l.starts_with('*'))
            .take_while(|l| !l.trim().is_empty())
            .map(str::to_string)
            .collect()
    };
    let (old, new) = (entry(before), entry(after));
    let changelog = if new.first() != old.first() {
        new.into_iter().skip(1).collect()
    } else {
        Vec::new()
    };
    Fix { patches, changelog }
}

/// The patch file a `Patch…:` tag line names.
fn patch_file(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("Patch")?;
    let (number, file) = rest.split_once(':')?;
    number
        .chars()
        .all(|c| c.is_ascii_digit())
        .then(|| file.trim())
        .filter(|f| !f.is_empty())
}

/// The fix commits on an AlmaLinux branch, from `git log` written as
/// `%H%x00%an%x00%ae%x00%s%x00%b%x1e`, newest first: those since its
/// last import from upstream (a commit whose subject starts with
/// `import `, as AlmaLinux's release bot writes them) that name one of
/// `cves`.
pub fn fix_commits(log: &str, cves: &[String]) -> Vec<FoundCommit> {
    parse_log(log)
        .into_iter()
        .take_while(|c| !c.subject.starts_with("import "))
        .filter(|c| {
            let text = format!("{}\n{}", c.subject, c.body);
            cves.iter().any(|cve| names_cve(&text, cve))
        })
        .collect()
}

/// Whether `text` names `cve` whole: `CVE-2026-1` is not named by
/// `CVE-2026-125396`.
fn names_cve(text: &str, cve: &str) -> bool {
    text.match_indices(cve).any(|(at, _)| {
        !text[at + cve.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit())
    })
}

/// The commits of a `git log` written as
/// `%H%x00%an%x00%ae%x00%s%x00%b%x1e`.
fn parse_log(log: &str) -> Vec<FoundCommit> {
    log.split('\x1e')
        .filter_map(|record| {
            let fields: Vec<&str> = record.trim_start_matches('\n').splitn(5, '\0').collect();
            let [sha, author, email, subject, body] = fields[..] else {
                return None;
            };
            Some(FoundCommit {
                sha: sha.to_string(),
                author: author.to_string(),
                email: email.to_string(),
                subject: subject.to_string(),
                body: body.trim().to_string(),
            })
        })
        .collect()
}

/// `stream_spec` with `fix` applied: each patch added as the spec
/// numbers patches, Release raised by one, and a changelog entry on top
/// dated `date`, by `who`, with the fix's lines and, with a RHEL issue,
/// its `Resolves:` line. A spec that applies patches one by one in
/// `%prep` is refused, since where a new one goes there is a choice.
pub fn apply_fix(
    stream_spec: &str,
    fix: &Fix,
    date: &str,
    who: &str,
    rhel_key: Option<&str>,
) -> Result<Applied, String> {
    if !spec::applies_patches_itself(stream_spec) {
        return Err(
            "the spec applies its patches one by one in %prep: add the patch there by hand"
                .to_string(),
        );
    }
    let mut out = stream_spec.to_string();
    let mut tags = Vec::new();
    for (comments, file) in &fix.patches {
        let (next, tag) = spec::add_patch(&out, comments, file)?;
        out = next;
        tags.push(tag);
    }
    let (next, release) = spec::bump_release(&out)?;
    let version = spec::parse_version(&next).ok_or("the spec has no Version:")?;
    let evr = match spec::parse_field(&next, "Epoch") {
        Some(epoch) => format!("{epoch}:{version}-{release}"),
        None => format!("{version}-{release}"),
    };
    let mut lines = fix.changelog.clone();
    if let Some(key) = rhel_key {
        lines.push(format!("- Resolves: {key}"));
    }
    let spec = spec::add_changelog_entry(&next, date, who, &evr, &lines)?;
    Ok(Applied { spec, evr, tags })
}

/// The commit message for a fix taken from AlmaLinux's commit `found`
/// at `url`: its subject and body as AlmaLinux wrote them, where it
/// came from, and the RHEL issue it resolves, when there is one.
pub fn commit_message(found: &FoundCommit, url: &str, rhel_key: Option<&str>) -> String {
    let mut msg = found.subject.clone();
    if !found.body.is_empty() {
        msg.push_str(&format!("\n\n{}", found.body));
    }
    msg.push_str(&format!("\n\nTaken from AlmaLinux: {url}"));
    if let Some(key) = rhel_key {
        msg.push_str(&format!("\n\nResolves: {key}"));
    }
    msg.push('\n');
    msg
}

/// The top `%changelog` entry of `spec`, header included.
fn changelog_entry(spec: &str) -> Vec<&str> {
    spec.lines()
        .skip_while(|l| l.trim_end() != "%changelog")
        .skip(1)
        .take_while(|l| !l.trim().is_empty())
        .collect()
}

/// AlmaLinux's web page of commit `sha` of `package`.
pub fn alma_commit_url(package: &str, sha: &str) -> String {
    format!("https://git.almalinux.org/rpms/{package}/commit/{sha}")
}

/// The CentOS Stream release (`c9s`) and AlmaLinux branch (`a9`) of a
/// major.
fn branches(major: u32) -> (String, String) {
    (format!("c{major}s"), format!("a{major}"))
}

/// The majors asked for with `--release` (`c9s` → 9), or 9 and 10.
fn majors(release: &[String]) -> Result<Vec<u32>, String> {
    if release.is_empty() {
        return Ok(vec![9, 10]);
    }
    release
        .iter()
        .map(|r| {
            r.strip_prefix('c')
                .and_then(|r| r.strip_suffix('s'))
                .and_then(|n| n.parse().ok())
                .ok_or_else(|| format!("{r} is not a CentOS Stream release (c9s, c10s)"))
        })
        .collect()
}

/// `dir` under the home directory.
fn home_dir(dir: &str) -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|h| h.join(dir))
        .ok_or_else(|| "cannot tell the home directory (HOME is not set)".to_string())
}

/// `package`'s checkout under `configured` (a directory, `~/` for the
/// home directory), else under `default` in the home directory.
fn checkout_dir(configured: Option<&str>, default: &str, package: &str) -> Result<PathBuf, String> {
    let base = match configured {
        Some(dir) => match dir.strip_prefix("~/") {
            Some(rest) => home_dir(rest)?,
            None => PathBuf::from(dir),
        },
        None => home_dir(default)?,
    };
    Ok(base.join(package))
}

/// Offer to make the missing Proposed Updates checkout `repo` of
/// `package`: `cbspkg sig-repo`, which creates the SIG's repo when
/// GitLab has none and clones it, then `mr register` when ~/.mrconfig
/// exists, so the clone is kept up to date with the others. Under
/// `--dry-run` the commands are only shown. `Ok(false)` when nothing
/// was made.
fn make_checkout(
    ui: &Ui,
    package: &str,
    repo: &Path,
    yes: bool,
) -> Result<bool, Box<dyn std::error::Error>> {
    let dir = repo.display().to_string();
    let mut sig_repo = vec![
        "cbspkg".to_string(),
        "sig-repo".to_string(),
        package.to_string(),
        "--sig".to_string(),
        "proposed-updates".to_string(),
        "-C".to_string(),
        dir.clone(),
    ];
    if yes {
        sig_repo.push("--yes".to_string());
    }
    let mrconfig = dirs::home_dir().is_some_and(|h| h.join(".mrconfig").exists());
    // Run inside the clone: given a directory that is not there, mr
    // registers the one it was run from instead.
    let register = vec!["mr".to_string(), "register".to_string()];
    eprintln!(
        "{dir} is not a checkout of {package}'s Proposed Updates repo yet. It can be made with:"
    );
    eprintln!("  {}", sandogasa_cli::explain::command_line(&sig_repo));
    if mrconfig {
        eprintln!(
            "  (cd {dir} && {})",
            sandogasa_cli::explain::command_line(&register)
        );
    }
    if ui.dry_run {
        return Ok(false);
    }
    if !yes {
        if !std::io::stdin().is_terminal() {
            return Err(format!(
                "no checkout at {dir}, and no terminal to ask at: pass --yes to make it"
            )
            .into());
        }
        if !ui.confirm("Make it? (cbspkg asks again before creating anything on GitLab)") {
            return Ok(false);
        }
    }
    sandogasa_cli::require_tools(&[("cbspkg", "cargo install cbspkg", None)])?;
    if let Some(parent) = repo.parent() {
        std::fs::create_dir_all(parent)?;
    }
    ui.step(&format!("Clone {package}'s Proposed Updates repo"));
    ui.run_required(&sig_repo, Path::new("."))?;
    if !repo.join(".git").exists() {
        return Err(format!("cbspkg sig-repo made no checkout at {dir}").into());
    }
    if mrconfig {
        ui.step("Register the clone with myrepos");
        if let Err(e) = ui.run_required(&register, repo) {
            eprintln!("warning: {e}; register it by hand");
        }
    }
    Ok(true)
}

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// One release's import, planned before anything is made.
struct Planned {
    branch: String,
    base: String,
    found: FoundCommit,
    url: String,
    spec_path: String,
    applied: Applied,
    /// `(file, contents)` of each patch to add.
    files: Vec<(String, Vec<u8>)>,
    message: String,
}

pub fn run(args: &ImportArgs) -> ExitCode {
    match run_inner(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner(args: &ImportArgs) -> Result<(), Box<dyn std::error::Error>> {
    let package = &args.package;
    let ui = Ui {
        explain: args.explain,
        dry_run: args.dry_run,
        quiet: false,
    };
    let cves = normalized_cves(&args.cve)?;
    let id = branch_id(args.rhel_key.as_deref(), &cves, args.advisory.as_deref())?;
    let majors = majors(&args.release)?;
    let checkouts = crate::config::load()
        .ok()
        .and_then(|c| c.checkouts)
        .unwrap_or_default();
    let repo = match &args.repo {
        Some(r) => r.clone(),
        None => checkout_dir(
            checkouts.proposed_updates.as_deref(),
            "src/centos/proposed_updates/rpms",
            package,
        )?,
    };
    if !repo.join(".git").exists() && !make_checkout(&ui, package, &repo, args.yes)? {
        // Without the checkout there is nothing to plan from.
        return Ok(());
    }
    // Where Stream's branches are read from: Stream's own repo, else
    // the SIG's, which mirrors them, else `origin`, said so.
    let found_remotes = Remotes::parse(&git(&repo, &["remote", "-v"])?);
    let base_remote = match found_remotes
        .find(Role::Stream)
        .or_else(|| found_remotes.find(Role::Sig(Sig::ProposedUpdates)))
    {
        Some(r) => r.name.clone(),
        None => {
            eprintln!(
                "note: no remote points at {package}'s CentOS Stream or Proposed Updates repo; \
                 reading Stream's branches from origin"
            );
            "origin".to_string()
        }
    };
    let alma = match &args.alma_dir {
        Some(d) => d.display().to_string(),
        None => {
            let local = checkout_dir(
                checkouts.almalinux.as_deref(),
                "src/almalinux/rpms",
                package,
            )?;
            if local.join(".git").exists() {
                local.display().to_string()
            } else {
                format!("https://git.almalinux.org/rpms/{package}.git")
            }
        }
    };
    let who = format!(
        "{} <{}>",
        git(&repo, &["config", "user.name"])?.trim(),
        git(&repo, &["config", "user.email"])?.trim()
    );
    let date = chrono::Local::now().format("%a %b %d %Y").to_string();

    let mut planned = Vec::new();
    for major in majors {
        let (stream, alma_branch) = branches(major);
        let alma_ref = format!("refs/remotes/almalinux/{alma_branch}");
        eprintln!("Reading AlmaLinux's {alma_branch} and CentOS Stream's {stream}...");
        // A local checkout's own branch may be behind: its
        // remote-tracking one, when it has it, is what git.almalinux.org
        // had at its last fetch.
        let fetched = git(
            &repo,
            &[
                "fetch",
                "--quiet",
                &alma,
                &format!("+refs/remotes/origin/{alma_branch}:{alma_ref}"),
            ],
        )
        .or_else(|_| {
            git(
                &repo,
                &[
                    "fetch",
                    "--quiet",
                    &alma,
                    &format!("+{alma_branch}:{alma_ref}"),
                ],
            )
        });
        if let Err(e) = fetched {
            eprintln!("{stream}: AlmaLinux's {alma_branch} could not be read ({e}); skipped");
            continue;
        }
        git(&repo, &["fetch", "--quiet", &base_remote, &stream])?;
        let commits = if args.commit.is_empty() {
            let log = git(
                &repo,
                &[
                    "log",
                    "-n",
                    "100",
                    "--format=%H%x00%an%x00%ae%x00%s%x00%b%x1e",
                    &alma_ref,
                ],
            )?;
            fix_commits(&log, &cves)
        } else {
            // Given by hand, a commit is taken whether it names the CVE
            // or not, on the branch it is on.
            let mut picked = Vec::new();
            for sha in &args.commit {
                if git(&repo, &["merge-base", "--is-ancestor", sha, &alma_ref]).is_err() {
                    continue;
                }
                let log = git(
                    &repo,
                    &[
                        "log",
                        "-1",
                        "--format=%H%x00%an%x00%ae%x00%s%x00%b%x1e",
                        sha,
                    ],
                )?;
                picked.extend(parse_log(&log));
            }
            picked
        };
        let found = match commits.as_slice() {
            [] if !args.commit.is_empty() => {
                eprintln!(
                    "{stream}: none of the commits given is on AlmaLinux's {alma_branch}; skipped"
                );
                continue;
            }
            [] => {
                eprintln!(
                    "{stream}: no commit on AlmaLinux's {alma_branch} since its last import names \
                     {}; skipped",
                    cves.join(", ")
                );
                continue;
            }
            [one] => one.clone(),
            many if !args.commit.is_empty() => {
                return Err(format!(
                    "{} of the commits given are on AlmaLinux's {alma_branch} ({}): give one \
                     per branch",
                    many.len(),
                    many.iter()
                        .map(|c| c.sha[..10.min(c.sha.len())].to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
                .into());
            }
            many => {
                return Err(format!(
                    "{} commits on AlmaLinux's {alma_branch} name the CVE ({}): pick one with \
                     --commit",
                    many.len(),
                    many.iter()
                        .map(|c| c.sha[..10.min(c.sha.len())].to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
                .into());
            }
        };
        let changes = git(
            &repo,
            &[
                "diff-tree",
                "--no-commit-id",
                // A rename would be one row of two paths.
                "--no-renames",
                "--name-status",
                "-r",
                &found.sha,
            ],
        )?;
        let spec_in_alma = changes
            .lines()
            .filter_map(|l| l.split_once('\t').map(|(_, p)| p))
            .find(|p| p.ends_with(".spec"))
            .ok_or_else(|| format!("AlmaLinux's {} changes no spec", found.sha))?
            .to_string();
        let before = git(&repo, &["show", &format!("{}^:{spec_in_alma}", found.sha)])?;
        let after = git(&repo, &["show", &format!("{}:{spec_in_alma}", found.sha)])?;
        let fix = alma_fix(&before, &after);
        if fix.patches.is_empty() {
            return Err(format!("AlmaLinux's {} adds no patch to its spec", found.sha).into());
        }
        let mut files = Vec::new();
        for (_, file) in &fix.patches {
            let path = changes
                .lines()
                .filter_map(|l| l.split_once('\t'))
                .filter(|(status, _)| status.starts_with('A'))
                .map(|(_, p)| p)
                .find(|p| Path::new(p).file_name().is_some_and(|n| n == file.as_str()))
                .ok_or_else(|| {
                    format!(
                        "AlmaLinux's {} names {file} but adds no such file",
                        found.sha
                    )
                })?;
            let out = Command::new("git")
                .args(["show", &format!("{}:{path}", found.sha)])
                .current_dir(&repo)
                .output()?;
            if !out.status.success() {
                return Err(format!(
                    "git show {}:{path}: {}",
                    found.sha,
                    String::from_utf8_lossy(&out.stderr).trim()
                )
                .into());
            }
            files.push((file.clone(), out.stdout));
        }
        let base = format!("{base_remote}/{stream}");
        let spec_path = format!("{package}.spec");
        let stream_spec = git(&repo, &["show", &format!("{base}:{spec_path}")])?;
        let applied = apply_fix(&stream_spec, &fix, &date, &who, args.rhel_key.as_deref())
            .map_err(|e| format!("{stream}: {e}"))?;
        let url = alma_commit_url(package, &found.sha);
        let message = commit_message(&found, &url, args.rhel_key.as_deref());
        planned.push(Planned {
            branch: proposed_branch(&stream, &id),
            base,
            found,
            url,
            spec_path,
            applied,
            files,
            message,
        });
    }
    if planned.is_empty() {
        return Err("nothing to import".into());
    }

    for p in &planned {
        println!("\n{} from {}", p.branch, p.base);
        println!(
            "  AlmaLinux's fix: {} ({} <{}>)",
            p.url, p.found.author, p.found.email
        );
        for tag in &p.applied.tags {
            println!("  adds {tag}");
        }
        println!(
            "  {} becomes {}, with the changelog entry:",
            p.spec_path, p.applied.evr
        );
        for line in changelog_entry(&p.applied.spec) {
            println!("    {line}");
        }
        println!("  commit, by {}:", p.found.author);
        for line in p.message.lines() {
            println!("    {line}");
        }
    }
    // Every branch checked before any is made, so a leftover one does
    // not stop the run halfway.
    let taken: Vec<&str> = planned
        .iter()
        .filter(|p| git(&repo, &["rev-parse", "--verify", "--quiet", &p.branch]).is_ok())
        .map(|p| p.branch.as_str())
        .collect();
    if !taken.is_empty() {
        return Err(format!(
            "{} exist(s) already; delete them or pick another name",
            taken.join(", ")
        )
        .into());
    }
    if !args.dry_run && !args.yes {
        if !std::io::stdin().is_terminal() {
            return Err("no terminal to confirm at: pass --yes, or --dry-run to look".into());
        }
        if !ui.confirm(&format!("Make {} branch(es) and commit?", planned.len())) {
            return Ok(());
        }
    }
    if !args.dry_run && !git(&repo, &["status", "--porcelain", "--untracked-files=no"])?.is_empty()
    {
        return Err(format!(
            "{} has uncommitted changes: commit or stash them first",
            repo.display()
        )
        .into());
    }
    // Where to come back to: the branch, or the commit of a detached
    // HEAD.
    let start = match git(&repo, &["symbolic-ref", "-q", "--short", "HEAD"]) {
        Ok(branch) => vec![branch.trim().to_string()],
        Err(_) => vec![
            "--detach".to_string(),
            git(&repo, &["rev-parse", "HEAD"])?.trim().to_string(),
        ],
    };
    let result = make(&ui, &repo, &base_remote, &planned);
    if !args.dry_run {
        if result.is_err() {
            // Whatever the failed step left behind goes, and so does the
            // branch it was on, unless it got its commit.
            let _ = git(&repo, &["reset", "--quiet", "--hard"]);
        }
        let mut back = vec!["switch", "--quiet"];
        back.extend(start.iter().map(String::as_str));
        if let Err(e) = git(&repo, &back) {
            eprintln!(
                "warning: could not switch back to {} ({e})",
                start.join(" ")
            );
        }
        if result.is_err() {
            for p in &planned {
                let made = git(&repo, &["rev-parse", "--verify", "--quiet", &p.branch]);
                let base = git(&repo, &["rev-parse", &p.base]);
                if let (Ok(made), Ok(base)) = (made, base)
                    && made == base
                {
                    let _ = git(&repo, &["branch", "-D", &p.branch]);
                }
            }
        }
    }
    result?;
    if args.dry_run {
        println!("\n(dry run: no branch or commit made)");
        return Ok(());
    }
    eprintln!(
        "\nThe commits carry the change as AlmaLinux made it, under its author; nothing in \
         them was written by an AI assistant."
    );
    println!("\nNext:");
    for p in &planned {
        println!(
            "  git -C {} push {base_remote} {}",
            repo.display(),
            p.branch
        );
    }
    println!("  then build each branch with cbspkg build");
    Ok(())
}

/// Make each planned branch and its commit; under `--dry-run`, only
/// show the commands.
fn make(
    ui: &Ui,
    repo: &Path,
    remote: &str,
    planned: &[Planned],
) -> Result<(), Box<dyn std::error::Error>> {
    for p in planned {
        ui.step(&format!("Branch {} off {}", p.branch, p.base));
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        ui.run_required(
            &s(&[
                "git",
                "switch",
                "--quiet",
                "--no-track",
                "-c",
                &p.branch,
                &p.base,
            ]),
            repo,
        )?;
        // It goes to the SIG's repo under its own name, so that is its
        // upstream, as cbspkg sig-repo sets it, not Stream's branch it
        // started from; set before the push, so a plain `git push` works.
        ui.run_required(
            &s(&[
                "git",
                "config",
                &format!("branch.{}.remote", p.branch),
                remote,
            ]),
            repo,
        )?;
        ui.run_required(
            &s(&[
                "git",
                "config",
                &format!("branch.{}.merge", p.branch),
                &format!("refs/heads/{}", p.branch),
            ]),
            repo,
        )?;
        ui.step("Add AlmaLinux's patch and edit the spec");
        if !ui.dry_run {
            for (file, contents) in &p.files {
                std::fs::write(repo.join(file), contents)?;
            }
            std::fs::write(repo.join(&p.spec_path), &p.applied.spec)?;
        }
        let mut add = vec!["git".to_string(), "add".to_string(), p.spec_path.clone()];
        add.extend(p.files.iter().map(|(f, _)| f.clone()));
        ui.run_required(&add, repo)?;
        if !ui.dry_run {
            ui.explain_diff_cached(repo, &[]);
        }
        ui.step(&format!("Commit it as {}'s", p.found.author));
        ui.run_required_with_input(
            &p.message,
            &s(&[
                "git",
                "commit",
                "--quiet",
                "-s",
                "--author",
                &format!("{} <{}>", p.found.author, p.found.email),
                "-F",
                "-",
            ]),
            repo,
        )?;
    }
    Ok(())
}

/// The `--cve` values trimmed and upper-cased, each checked to be a
/// CVE ID (`CVE-2026-125396`).
pub fn normalized_cves(cves: &[String]) -> Result<Vec<String>, String> {
    cves.iter()
        .map(|c| {
            let c = c.trim().to_ascii_uppercase();
            let parts: Vec<&str> = c.split('-').collect();
            let valid = matches!(parts[..], ["CVE", year, number]
                if year.len() == 4
                    && year.chars().all(|d| d.is_ascii_digit())
                    && !number.is_empty()
                    && number.chars().all(|d| d.is_ascii_digit()));
            valid
                .then_some(c.clone())
                .ok_or_else(|| format!("{c} is not a CVE ID (CVE-YYYY-NNNN)"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const A9_BEFORE: &str = include_str!("../testdata/ghostscript-a9-before.spec");
    const A9_AFTER: &str = include_str!("../testdata/ghostscript-a9-after.spec");
    const C9S: &str = include_str!("../testdata/ghostscript-c9s.spec");

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn a_branch_is_named_by_rhel_issue_then_cve_then_advisory() {
        let one = s(&["CVE-2026-125396"]);
        let two = s(&["CVE-2026-1", "CVE-2026-2"]);
        assert_eq!(branch_id(Some("RHEL-1"), &one, None).unwrap(), "RHEL-1");
        assert_eq!(
            branch_id(None, &one, Some("ALSA-2026:9")).unwrap(),
            "CVE-2026-125396"
        );
        assert_eq!(
            branch_id(None, &two, Some("ALSA-2026:9")).unwrap(),
            "ALSA-2026-9"
        );
        assert_eq!(branch_id(None, &two, None).unwrap(), "CVE-2026-1");
        assert!(branch_id(None, &[], None).is_err());
    }

    #[test]
    fn almalinuxs_patch_and_changelog_are_read_from_its_spec() {
        let fix = alma_fix(A9_BEFORE, A9_AFTER);
        assert_eq!(fix.patches.len(), 1);
        let (comments, file) = &fix.patches[0];
        assert_eq!(
            file,
            "1000-PS-interpreter-validate-operands-to-.putgstringcopy.patch"
        );
        assert_eq!(comments.len(), 3, "{comments:?}");
        assert!(comments[0].starts_with("CVE-2026-125396 (Upload2Shell)"));
        assert!(comments.iter().all(|c| !c.contains("AlmaLinux Patch")));
        assert_eq!(
            fix.changelog,
            [
                "- Fix CVE-2026-125396 (Upload2Shell): validate operands to .putgstringcopy",
                "  to prevent -dSAFER bypass via Type 1 font loader",
            ]
        );
        // Nothing added: nothing found.
        assert_eq!(alma_fix(A9_BEFORE, A9_BEFORE), Fix::default());
    }

    #[test]
    fn the_fix_goes_into_streams_spec_as_its_packager_would() {
        let fix = alma_fix(A9_BEFORE, A9_AFTER);
        let applied = apply_fix(C9S, &fix, "Mon Nov 23 2026", "Me <me@example.org>", None).unwrap();
        assert_eq!(applied.evr, "9.54.0-21");
        assert_eq!(
            applied.tags,
            ["Patch028: 1000-PS-interpreter-validate-operands-to-.putgstringcopy.patch"]
        );
        assert!(applied.spec.contains("\nRelease:          21%{?dist}\n"));
        assert!(applied.spec.contains(
            "%changelog\n* Mon Nov 23 2026 Me <me@example.org> - 9.54.0-21\n\
             - Fix CVE-2026-125396 (Upload2Shell): validate operands to .putgstringcopy\n"
        ));
        assert!(!applied.spec.contains("alma"));
        assert_eq!(
            changelog_entry(&applied.spec),
            [
                "* Mon Nov 23 2026 Me <me@example.org> - 9.54.0-21",
                "- Fix CVE-2026-125396 (Upload2Shell): validate operands to .putgstringcopy",
                "  to prevent -dSAFER bypass via Type 1 font loader",
            ]
        );
        let with_key = apply_fix(C9S, &fix, "d", "w", Some("RHEL-1")).unwrap();
        assert!(with_key.spec.contains("- Resolves: RHEL-1\n"));
        // A spec applying patches one by one is left to a person.
        let one_by_one =
            "Version: 1\nRelease: 1\nPatch0: a.patch\n%prep\n%setup -q\n%patch -P0\n%changelog\n";
        assert!(apply_fix(one_by_one, &fix, "d", "w", None).is_err());
    }

    #[test]
    fn fix_commits_are_those_since_the_last_import_naming_the_cve() {
        let rec = |sha: &str, subject: &str, body: &str| {
            format!("{sha}\0A\0a@example.org\0{subject}\0{body}\x1e\n")
        };
        let log = [
            rec(
                "c3",
                "Fix CVE-2026-125396 (Upload2Shell)",
                "Backport upstream",
            ),
            rec("c2", "Fix something else", ""),
            rec("c1", "import UBI ghostscript-9.54.0-19.el9_8.3", ""),
            rec("c0", "Fix CVE-2026-125396 before", ""),
        ]
        .concat();
        let found = fix_commits(&log, &s(&["CVE-2026-125396"]));
        assert_eq!(found.len(), 1);
        // A CVE whose ID another one starts with is not named.
        assert!(fix_commits(&log, &s(&["CVE-2026-1"])).is_empty());
        assert_eq!(found[0].sha, "c3");
        assert_eq!(found[0].body, "Backport upstream");
        let msg = commit_message(&found[0], &alma_commit_url("ghostscript", "c3"), None);
        assert_eq!(
            msg,
            "Fix CVE-2026-125396 (Upload2Shell)\n\nBackport upstream\n\n\
             Taken from AlmaLinux: https://git.almalinux.org/rpms/ghostscript/commit/c3\n"
        );
        assert!(commit_message(&found[0], "u", Some("RHEL-1")).ends_with("\n\nResolves: RHEL-1\n"));
    }

    /// The whole run on local repositories: AlmaLinux's a9 with a fix
    /// on top of an import, and the Proposed Updates checkout with
    /// Stream's c9s; dry, then for real.
    #[test]
    fn a_fix_is_taken_from_almalinux_into_a_proposed_update_branch() {
        let dir = tempfile::tempdir().unwrap();
        let run = |cwd: &Path, args: &[&str]| {
            let out = Command::new("git")
                .args(["-c", "user.name=T", "-c", "user.email=t@example.org"])
                .args([
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "init.defaultBranch=main",
                ])
                .args(args)
                .current_dir(cwd)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        // AlmaLinux: SOURCES/ + SPECS/, an import, then the fix.
        let alma = dir.path().join("alma");
        std::fs::create_dir_all(alma.join("SPECS")).unwrap();
        std::fs::create_dir_all(alma.join("SOURCES")).unwrap();
        run(dir.path(), &["init", "--quiet", "alma"]);
        run(&alma, &["checkout", "--quiet", "-b", "a9"]);
        std::fs::write(alma.join("SPECS/ghostscript.spec"), A9_BEFORE).unwrap();
        run(&alma, &["add", "."]);
        run(
            &alma,
            &[
                "commit",
                "--quiet",
                "-m",
                "import UBI ghostscript-9.54.0-19.el9_8.3",
            ],
        );
        let patch = "1000-PS-interpreter-validate-operands-to-.putgstringcopy.patch";
        std::fs::write(alma.join("SOURCES").join(patch), "the patch\n").unwrap();
        std::fs::write(alma.join("SPECS/ghostscript.spec"), A9_AFTER).unwrap();
        run(&alma, &["add", "."]);
        run(
            &alma,
            &[
                "-c",
                "user.name=Andrew Lukoshko",
                "-c",
                "user.email=alukoshko@almalinux.org",
                "commit",
                "--quiet",
                "-m",
                "Fix CVE-2026-125396 (Upload2Shell): validate operands to .putgstringcopy",
                "-m",
                "Backport upstream commit 1484854e.",
            ],
        );
        // Proposed Updates: origin with Stream's c9s.
        let origin = dir.path().join("pu.git");
        run(dir.path(), &["init", "--quiet", "--bare", "pu.git"]);
        let seed = dir.path().join("seed");
        run(dir.path(), &["init", "--quiet", "seed"]);
        run(&seed, &["checkout", "--quiet", "-b", "c9s"]);
        std::fs::write(seed.join("ghostscript.spec"), C9S).unwrap();
        run(&seed, &["add", "."]);
        run(&seed, &["commit", "--quiet", "-m", "c9s"]);
        run(&seed, &["push", "--quiet", origin.to_str().unwrap(), "c9s"]);
        let repo = dir.path().join("checkout");
        run(
            dir.path(),
            &[
                "clone",
                "--quiet",
                "-b",
                "c9s",
                origin.to_str().unwrap(),
                "checkout",
            ],
        );
        run(&repo, &["config", "user.name", "Me"]);
        run(&repo, &["config", "user.email", "me@example.org"]);
        run(&repo, &["config", "commit.gpgsign", "false"]);

        let mut args = ImportArgs {
            package: "ghostscript".into(),
            commit: Vec::new(),
            cve: s(&["CVE-2026-125396"]),
            from: Source::Almalinux,
            release: s(&["c9s"]),
            advisory: None,
            rhel_key: None,
            alma_dir: Some(alma.clone()),
            repo: Some(repo.clone()),
            dry_run: true,
            yes: false,
            explain: false,
        };
        run_inner(&args).unwrap();
        let branch = "c9s-proposed-CVE-2026-125396";
        assert!(run(&repo, &["branch", "--list", branch]).is_empty());

        args.dry_run = false;
        args.yes = true;
        run_inner(&args).unwrap();
        // Back on the branch it started from.
        assert_eq!(run(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]), "c9s");
        // Its upstream is itself on origin, not Stream's c9s.
        assert_eq!(
            run(&repo, &["config", &format!("branch.{branch}.remote")]),
            "origin"
        );
        assert_eq!(
            run(&repo, &["config", &format!("branch.{branch}.merge")]),
            format!("refs/heads/{branch}")
        );
        assert_eq!(
            run(&repo, &["log", "-1", "--format=%an <%ae>", branch]),
            "Andrew Lukoshko <alukoshko@almalinux.org>"
        );
        let message = run(&repo, &["log", "-1", "--format=%B", branch]);
        assert!(
            message.starts_with("Fix CVE-2026-125396 (Upload2Shell)"),
            "{message}"
        );
        assert!(
            message.contains(
                "Taken from AlmaLinux: https://git.almalinux.org/rpms/ghostscript/commit/"
            )
        );
        assert!(message.contains("Signed-off-by: Me <me@example.org>"));
        assert_eq!(
            run(&repo, &["show", &format!("{branch}:{patch}")]),
            "the patch"
        );
        let spec = run(&repo, &["show", &format!("{branch}:ghostscript.spec")]);
        assert!(spec.contains(&format!("Patch028: {patch}")));
        assert!(spec.contains("Release:          21%{?dist}"));
        assert!(spec.contains("* "));
        // The branch exists now: a second run stops before making it.
        assert!(run_inner(&args).is_err());
        // An AlmaLinux branch it cannot read, or no commit naming the
        // CVE: nothing to import.
        args.release = s(&["c10s"]);
        assert!(run_inner(&args).is_err());
        args.release = s(&["c9s"]);
        args.cve = s(&["CVE-2026-1"]);
        assert!(run_inner(&args).is_err());
    }

    #[test]
    fn cves_are_read_whole_and_upper_cased() {
        assert_eq!(
            normalized_cves(&s(&[" cve-2026-125396", "CVE-2026-1"])).unwrap(),
            ["CVE-2026-125396", "CVE-2026-1"]
        );
        assert!(normalized_cves(&s(&["CVE-26-1"])).is_err());
        assert!(normalized_cves(&s(&["RHEL-1"])).is_err());
    }

    #[test]
    fn an_epoch_is_part_of_the_changelog_version() {
        let spec = "Epoch: 1\nVersion: 2.3\nRelease: 4%{?dist}\nPatch0: a.patch\n%prep\n%autosetup -p1\n%changelog\n";
        let fix = Fix {
            patches: vec![(Vec::new(), "b.patch".into())],
            changelog: vec!["- Fix".into()],
        };
        assert_eq!(
            apply_fix(spec, &fix, "d", "w", None).unwrap().evr,
            "1:2.3-5"
        );
    }

    #[test]
    fn checkouts_are_found_under_the_configured_or_default_directory() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(
            checkout_dir(None, "src/almalinux/rpms", "foo").unwrap(),
            home.join("src/almalinux/rpms/foo")
        );
        assert_eq!(
            checkout_dir(Some("~/pu"), "x", "foo").unwrap(),
            home.join("pu/foo")
        );
        assert_eq!(
            checkout_dir(Some("/srv/pu"), "x", "foo").unwrap(),
            PathBuf::from("/srv/pu/foo")
        );
    }

    /// A missing checkout under --dry-run: the commands that would make
    /// it are shown, and nothing else is done.
    #[test]
    fn a_missing_checkout_is_only_shown_how_to_make_under_dry_run() {
        let dir = tempfile::tempdir().unwrap();
        let ui = Ui {
            explain: false,
            dry_run: true,
            quiet: true,
        };
        let repo = dir.path().join("pu/ghostscript");
        assert!(!make_checkout(&ui, "ghostscript", &repo, false).unwrap());
        assert!(!repo.exists());
        let args = ImportArgs {
            package: "ghostscript".into(),
            commit: Vec::new(),
            cve: s(&["CVE-2026-125396"]),
            from: Source::Almalinux,
            release: Vec::new(),
            advisory: None,
            rhel_key: None,
            alma_dir: None,
            repo: Some(repo.clone()),
            dry_run: true,
            yes: false,
            explain: false,
        };
        run_inner(&args).unwrap();
        assert!(!repo.exists());
    }

    #[test]
    fn releases_are_read_as_their_majors() {
        assert_eq!(majors(&[]).unwrap(), [9, 10]);
        assert_eq!(majors(&s(&["c10s"])).unwrap(), [10]);
        assert!(majors(&s(&["el9"])).is_err());
    }
}
