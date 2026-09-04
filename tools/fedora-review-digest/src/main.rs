// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use sandogasa_config::ConfigFile;

use fedora_review_digest::checklist::{self, Item, Mark, Resolution, ReviewedIssue};
use fedora_review_digest::review::{Generator, Review};
use fedora_review_digest::{bugzilla, config, cratesio, runner};
use sandogasa_bugzilla::BzClient;
use sandogasa_review::resolve_interactive;

const BUGZILLA_URL: &str = "https://bugzilla.redhat.com";

#[derive(Parser)]
#[command(
    about,
    long_about = None,
    max_term_width = 80,
    version = sandogasa_cli::version!(),
    before_help = sandogasa_cli::banner!(),
    args_conflicts_with_subcommands = true,
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Digest a review (the default action when no subcommand is given).
    #[command(flatten)]
    digest: DigestArgs,
}

#[derive(Subcommand)]
enum Command {
    /// Set up or verify the Bugzilla API key used by `--post`.
    Config,
    /// Run fedora-review on a bug, then digest the result.
    Run(Box<RunArgs>),
}

#[derive(Args)]
struct RunArgs {
    /// Review-request Bugzilla bug id (fedora-review -b).
    #[arg(value_name = "BUGID")]
    bug: String,

    /// COPR(s) with staged deps (owner/project; CSV ok)
    #[arg(long, value_delimiter = ',', value_name = "OWNER/PROJECT")]
    copr: Vec<String>,

    /// Extra repo baseurl(s) to build against (CSV ok)
    #[arg(long, value_delimiter = ',', value_name = "URL")]
    repo: Vec<String>,

    /// Mock config; doubles as the COPR chroot name
    #[arg(short, long, value_name = "NAME")]
    mock_config: Option<String>,

    /// Extra mock option(s), appended to fedora-review -o
    #[arg(long, value_name = "OPT", allow_hyphen_values = true)]
    mock_option: Vec<String>,

    /// Unique mock buildroot suffix (mock --uniqueext)
    #[arg(long, value_name = "TEXT")]
    uniqueext: Option<String>,

    /// Print the fedora-review command without running it
    #[arg(long)]
    dry_run: bool,

    /// Stop after the fedora-review build (skip digest)
    #[arg(long)]
    no_digest: bool,

    #[command(flatten)]
    opts: DigestOpts,
}

#[derive(Args)]
struct DigestArgs {
    /// fedora-review result dir, or a bug id under --reviews-dir
    #[arg(value_name = "DIR-OR-BUGID")]
    input: Option<String>,

    #[command(flatten)]
    opts: DigestOpts,
}

/// Digest options shared by the default action and `run` (which
/// flows into a digest after the fedora-review build).
#[derive(Args)]
struct DigestOpts {
    /// Reviewer note placed above the review ("LGTM!")
    #[arg(short, long, value_name = "TEXT")]
    comment: Option<String>,

    /// Accept inferred checklist marks without prompting
    #[arg(short = 'y', long)]
    yes: bool,

    /// Base directory for bug-id lookup
    #[arg(long, value_name = "DIR", default_value = ".")]
    reviews_dir: PathBuf,

    /// Skip the crates.io latest-version check
    #[arg(long)]
    no_net: bool,

    /// Post to Bugzilla: comment, review flag, claim
    #[arg(long)]
    post: bool,
}

fn main() -> ExitCode {
    let cli = sandogasa_cli::parse_with_defaults::<Cli>(env!("CARGO_PKG_NAME"));
    let result = match cli.command {
        Some(Command::Config) => cmd_config(),
        Some(Command::Run(args)) => cmd_run(&args),
        None => run(&cli.digest).map(|text| print!("{text}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fedora-review-digest: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Set up / verify the Bugzilla API key (and email, for validation)
/// stored at `~/.config/fedora-review-digest/config.toml`.
fn cmd_config() -> Result<(), String> {
    let cf = ConfigFile::for_tool("fedora-review-digest");
    let mut cfg: config::Config = cf.load().unwrap_or_default();
    if cfg.bugzilla.api_key.trim().is_empty() {
        eprintln!(
            "Create an API key at \
             https://bugzilla.redhat.com/userprefs.cgi?tab=apikey"
        );
        cfg.bugzilla.api_key = sandogasa_config::prompt_field("Bugzilla", "API key", true, None)
            .map_err(|e| e.to_string())?;
    }
    if cfg.bugzilla.email.trim().is_empty() {
        cfg.bugzilla.email = sandogasa_config::prompt_field(
            "Bugzilla",
            "email",
            false,
            Some(&sandogasa_config::validate_email),
        )
        .map_err(|e| e.to_string())?;
    }
    cf.save(&cfg).map_err(|e| e.to_string())?;
    eprintln!("Saved to {}", cf.path().display());

    let client = BzClient::new(BUGZILLA_URL)
        .with_api_key(cfg.bugzilla.api_key.clone())
        .map_err(|e| e.to_string())?;
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    eprint!("Validating API key… ");
    match rt.block_on(client.valid_login(&cfg.bugzilla.email)) {
        Ok(true) => eprintln!("valid."),
        Ok(false) => eprintln!("login not recognized (check the email/key)."),
        Err(e) => eprintln!("could not validate: {e}"),
    }
    Ok(())
}

/// The `run` subcommand: invoke fedora-review (optionally with
/// extra dependency repos for builds whose deps are staged in a
/// COPR ahead of Rawhide), then flow straight into the digest —
/// a mock build takes minutes, so its result shouldn't dead-end.
fn cmd_run(args: &RunArgs) -> Result<(), String> {
    let review_args = runner::build_args(&runner::RunSpec {
        bug: args.bug.clone(),
        coprs: args.copr.clone(),
        repos: args.repo.clone(),
        mock_config: args.mock_config.clone(),
        uniqueext: args.uniqueext.clone(),
        mock_options: args.mock_option.clone(),
    })?;
    if args.dry_run {
        println!("{}", runner::display_command(&review_args));
        return Ok(());
    }
    runner::run_fedora_review(&review_args, &args.opts.reviews_dir)?;
    if args.no_digest {
        let dir = find_review_dir(&args.opts.reviews_dir, &args.bug)?;
        eprintln!("review results in {}", dir.display());
        return Ok(());
    }
    let digest = DigestArgs {
        input: Some(args.bug.clone()),
        opts: DigestOpts {
            comment: args.opts.comment.clone(),
            yes: args.opts.yes,
            reviews_dir: args.opts.reviews_dir.clone(),
            no_net: args.opts.no_net,
            post: args.opts.post,
        },
    };
    run(&digest).map(|text| print!("{text}"))
}

fn run(cli: &DigestArgs) -> Result<String, String> {
    let input = cli
        .input
        .as_deref()
        .ok_or("provide a fedora-review result directory or a bug id")?;
    let dir = resolve_input(input, &cli.opts.reviews_dir)?;
    let review = Review::from_dir(&dir)?;
    match review.spec.generator {
        Generator::Rust2Rpm => {}
        Generator::Pyp2Spec => {
            return Err("pyp2spec digests aren't implemented yet (rust2rpm only for now)".into());
        }
        Generator::Unknown => {
            return Err(format!(
                "{} wasn't generated by rust2rpm or pyp2spec — nothing to digest",
                dir.display()
            ));
        }
    }

    // Evidence for the "latest version" item: the latest stable on
    // crates.io. A lookup failure is a warning, not fatal — the item
    // just reports "not checked".
    let crate_latest = if cli.opts.no_net {
        None
    } else {
        match cratesio::fetch_max_stable_version(&review.upstream_name) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("warning: {e}; skipping the latest-version check");
                None
            }
        }
    };

    let mut items = checklist::infer(&review, crate_latest.as_deref());
    let mut issues = checklist::reviewed(&review.issues);

    // Finalize the marks: interactively by default, or accept the
    // inferred ones with --yes. A non-tty without --yes can't prompt, so
    // fail with a remedy rather than silently rubber-stamping.
    let interactive = !cli.opts.yes && std::io::stdin().is_terminal();
    if !cli.opts.yes {
        if !interactive {
            return Err(
                "not a terminal; pass -y to accept the inferred marks (and --comment for a note)"
                    .into(),
            );
        }
        // Show the inferred findings in full first, then confirm each
        // item — reviewing in context beats deciding line-by-line blind.
        eprintln!(
            "── inferred review for {} {} ──\n",
            review.spec.name, review.spec.version
        );
        eprint!(
            "{}",
            checklist::render_review(review.spec.generator, &items, &issues)
        );
        show_static_deps(&review);
        eprintln!("\n── confirm each item ──");
        finalize(&mut items)?;
        finalize_issues(&mut issues)?;
    }

    let comment = match &cli.opts.comment {
        Some(c) => Some(c.clone()),
        None if interactive => prompt_comment()?,
        None => None,
    };

    let review_block = checklist::render_review(review.spec.generator, &items, &issues);
    let post_block = checklist::render_post_import(review.spec.generator, &review.upstream_name);
    let text = checklist::assemble(comment.as_deref(), &review_block, &post_block);

    if cli.opts.post {
        let approved = checklist::approved(&items, &issues);
        post_to_bugzilla(input, &dir, &text, approved, cli.opts.yes)?;
    }
    Ok(text)
}

/// Post the assembled review to its Bugzilla review bug: a comment, the
/// `fedora-review` flag (and, on approval, status POST), and claiming the
/// bug for the reviewer. Fetches the bug first (to validate it, read the
/// current flag, and see who it's assigned to), shows what will change,
/// and confirms before the write.
fn post_to_bugzilla(
    input: &str,
    dir: &Path,
    digest: &str,
    approved: bool,
    yes: bool,
) -> Result<(), String> {
    let bug_id = bug_id(input, dir)
        .ok_or("couldn't determine the bug id from the input or dir name; pass the bug id")?;
    let creds = config::credentials().map_err(|e| e.to_string())?;
    let client = BzClient::new(BUGZILLA_URL)
        .with_api_key(creds.api_key)
        .map_err(|e| e.to_string())?;
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;

    let bug = rt
        .block_on(client.bug(bug_id))
        .map_err(|e| format!("fetching bug {bug_id}: {e}"))?;
    let current_flag = bugzilla::current_review_flag(&bug);
    // Claim the bug by assigning it to the reviewer — unless it's already
    // theirs, in which case there's nothing to change.
    let claim =
        (!bug.assigned_to.eq_ignore_ascii_case(&creds.email)).then_some(creds.email.as_str());

    eprintln!("\nBug {bug_id}: {}", bug.summary);
    eprintln!(
        "Will {}.",
        bugzilla::action_summary(approved, current_flag, claim.is_some())
    );
    if !yes {
        if !std::io::stdin().is_terminal() {
            return Err("not a terminal; pass -y to post to Bugzilla".into());
        }
        if !confirm_yes("Post to Bugzilla?")? {
            return Err("aborted".into());
        }
    }

    let body = bugzilla::update_body(digest, approved, current_flag, claim);
    rt.block_on(client.update(bug_id, &body))
        .map_err(|e| format!("posting to bug {bug_id}: {e}"))?;
    eprintln!("Posted to bug {bug_id}.");
    Ok(())
}

/// The review-request bug id: a numeric input, else the leading digits
/// of the directory name (`2489102-rust-foo` → 2489102).
fn bug_id(input: &str, dir: &Path) -> Option<u64> {
    if let Ok(n) = input.parse::<u64>() {
        return Some(n);
    }
    bug_id_from_dirname(dir.file_name()?.to_str()?)
}

fn bug_id_from_dirname(name: &str) -> Option<u64> {
    name.chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}

/// Yes/no confirmation defaulting to yes (the `--post` intent).
fn confirm_yes(question: &str) -> Result<bool, String> {
    sandogasa_cli::confirm(question, true).map_err(|e| e.to_string())
}

/// Resolve the positional input to a review directory: an existing
/// directory is used as-is; an all-digits argument is treated as a bug
/// id and matched against `<id>-*` under `reviews_dir`.
fn resolve_input(input: &str, reviews_dir: &Path) -> Result<PathBuf, String> {
    let p = Path::new(input);
    if p.is_dir() {
        return Ok(p.to_path_buf());
    }
    if looks_like_bug_id(input) {
        return find_review_dir(reviews_dir, input);
    }
    Err(format!(
        "{input:?} is neither an existing directory nor a bug id"
    ))
}

/// Whether a string is a bare Bugzilla bug id (all digits).
fn looks_like_bug_id(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Find the single `<bug_id>-*` directory under `base` (fedora-review's
/// naming). Errors if there are none or more than one.
fn find_review_dir(base: &Path, bug_id: &str) -> Result<PathBuf, String> {
    let prefix = format!("{bug_id}-");
    let mut matches: Vec<PathBuf> = std::fs::read_dir(base)
        .map_err(|e| format!("reading {}: {e}", base.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(&prefix))
        })
        .collect();
    match matches.len() {
        0 => Err(format!(
            "no review dir for bug {bug_id} under {} (looked for {prefix}*)",
            base.display()
        )),
        1 => Ok(matches.pop().unwrap()),
        _ => Err(format!(
            "multiple {prefix}* dirs under {}; pass the path directly",
            base.display()
        )),
    }
}

/// For a binary crate, print the statically-linked dependency license
/// breakdown — the build log's LICENSE SUMMARY reconciled against the
/// spec's folded `License:` — for the reviewer to inspect. Goes to
/// stderr (it's evidence, not part of the pasted comment).
fn show_static_deps(review: &Review) {
    let Some(sd) = &review.static_deps else {
        return;
    };
    eprintln!("\n── statically linked dependency licenses ──");
    eprintln!("the spec's folded License: section (verify every dep license is folded in):\n");
    eprintln!("{}", sd.spec_section);
    let missing = sd.missing();
    if sd.summary.is_empty() {
        eprintln!("\n(no LICENSE SUMMARY in the build log to cross-check against)");
    } else if missing.is_empty() {
        eprintln!(
            "\nall {} licenses from the build-log LICENSE SUMMARY are present above.",
            sd.summary.len()
        );
    } else {
        eprintln!(
            "\n⚠ in the build-log summary but NOT the spec License:: {}",
            missing.join(", ")
        );
    }
}

/// Walk the checklist, prompting +1 / 0 / -1 per item (Enter accepts the
/// inferred default); for a caveat/fail, also prompt for the note.
fn finalize(items: &mut [Item]) -> Result<(), String> {
    eprintln!("+1 pass / 0 caveat / -1 fail — Enter accepts the inferred mark\n");
    let n = items.len();
    for (i, it) in items.iter_mut().enumerate() {
        let inferred = it.mark;
        let vote = prompt_vote(i + 1, n, it)?;
        it.mark = Mark::from_vote(vote).expect("vote validated");
        match it.mark {
            // Keep an evidence note on an unchanged pass; drop a
            // now-stale caveat/fail note if the reviewer upgraded to pass.
            Mark::Pass => {
                if inferred != Mark::Pass {
                    it.note = None;
                }
            }
            // A caveat/fail needs a justification — prompt for it (Enter
            // keeps the inferred placeholder).
            Mark::Caveat | Mark::Fail => it.note = prompt_note(it)?,
        }
    }
    eprintln!();
    Ok(())
}

/// Walk the fedora-review issues, letting the reviewer keep each one
/// (still blocks approval), explain it away (kept on record but
/// non-blocking), or remove it (a false positive — dropped). With all
/// issues addressed and no failing checklist item, the verdict flips to
/// APPROVED.
fn finalize_issues(issues: &mut Vec<ReviewedIssue>) -> Result<(), String> {
    if issues.is_empty() {
        return Ok(());
    }
    eprintln!("── address issues ──");
    eprintln!("(k)eep blocks approval / (e)xplain accepts it / (r)emove drops it\n");
    let decisions = resolve_interactive(std::mem::take(issues), |ri| ri.finding().to_string())?;
    // Removed issues are dropped; keep/explain carry the new resolution.
    *issues = decisions
        .into_iter()
        .filter_map(|(ri, resolution)| match resolution {
            Resolution::Removed => None,
            res => Some(ReviewedIssue {
                resolution: res,
                ..ri
            }),
        })
        .collect();
    eprintln!();
    Ok(())
}

/// Prompt for one item's vote, defaulting to its inferred mark.
fn prompt_vote(idx: usize, total: usize, it: &Item) -> Result<i32, String> {
    let default = it.mark.vote();
    // Show the evidence/justification inline so the call is made *with*
    // the finding in view, not after committing to a vote.
    let note = it
        .note
        .as_deref()
        .map(|n| format!(" ({n})"))
        .unwrap_or_default();
    loop {
        eprint!(
            "[{idx}/{total}] {} {}{note} [{:+}]: ",
            it.mark.emoji(),
            it.label,
            default
        );
        let _ = std::io::stderr().flush();
        let line = read_line()?;
        let s = line.trim();
        if s.is_empty() {
            return Ok(default);
        }
        match s {
            "+1" | "1" => return Ok(1),
            "0" => return Ok(0),
            "-1" => return Ok(-1),
            _ => eprintln!("  enter +1, 0, or -1"),
        }
    }
}

/// Prompt for a caveat/fail justification, defaulting to the inferred
/// note (Enter keeps it).
fn prompt_note(it: &Item) -> Result<Option<String>, String> {
    let default = it.note.clone().unwrap_or_default();
    eprint!("    note [{default}]: ");
    let _ = std::io::stderr().flush();
    let line = read_line()?;
    let s = line.trim();
    Ok(if s.is_empty() {
        it.note.clone()
    } else {
        Some(s.to_string())
    })
}

/// Prompt for the free-form top comment: read lines until a blank line
/// or EOF. `None` if nothing was entered.
fn prompt_comment() -> Result<Option<String>, String> {
    eprintln!("Top comment (optional) — end with an empty line:");
    let mut lines: Vec<String> = Vec::new();
    loop {
        let line = read_line()?;
        if line.is_empty() {
            break; // EOF
        }
        let trimmed = line.trim_end_matches('\n');
        if trimmed.is_empty() {
            break; // blank line ends input
        }
        lines.push(trimmed.to_string());
    }
    Ok(if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    })
}

/// Read a line from stdin; empty string signals EOF.
fn read_line() -> Result<String, String> {
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| format!("reading input: {e}"))?;
    Ok(line)
}

#[cfg(test)]
mod tests {
    /// The committed man page is generated from this CLI; see
    /// `sandogasa_cli::man` and `scripts/gen-man.sh`.
    #[test]
    fn man_page_matches_cli() {
        sandogasa_cli::man::check::<super::Cli>(
            concat!(env!("CARGO_MANIFEST_DIR"), "/man/fedora-review-digest.1"),
            env!("CARGO_PKG_VERSION"),
        );
    }

    use super::*;

    #[test]
    fn bug_id_detection() {
        assert!(looks_like_bug_id("2489102"));
        assert!(!looks_like_bug_id("2489102-rust-foo"));
        assert!(!looks_like_bug_id("rust-foo"));
        assert!(!looks_like_bug_id(""));
    }

    #[test]
    fn bug_id_from_dirname_takes_leading_digits() {
        assert_eq!(
            bug_id_from_dirname("2489102-rust-trustfall_core"),
            Some(2489102)
        );
        assert_eq!(bug_id_from_dirname("rust-foo"), None);
        assert_eq!(bug_id_from_dirname(""), None);
    }
}
