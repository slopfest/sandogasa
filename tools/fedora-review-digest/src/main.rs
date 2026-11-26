// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use sandogasa_config::{ConfigFile, identity};

use fedora_review_digest::bugzilla::{self, Verdict};
use fedora_review_digest::checklist::{self, Item, Mark, Resolution, ReviewedIssue};
use fedora_review_digest::review::{Generator, Review};
use fedora_review_digest::{config, cratesio, hackage, pyp2spec, pypi, runner};
use sandogasa_bugzilla::BzClient;

use sandogasa_review::resolve_interactive;

use fedora_review_digest::secrets::BUGZILLA_URL;

/// The name the tool's config is kept under.
const TOOL: &str = "fedora-review-digest";

// The keyring lives here, not in the library: the library is built
// without cfg(test) for this binary's tests, so a keyring it chose
// could be the user's.
sandogasa_config::keyring_fn!();

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

    /// Deprecated: `digest` without the subcommand's name.
    #[command(flatten)]
    digest: DigestArgs,
}

#[derive(Subcommand)]
enum Command {
    /// Set up or verify the Bugzilla API key used by `--post`.
    Config(sandogasa_cli::ConfigArgs),
    /// Digest an existing fedora-review result.
    Digest(DigestArgs),
    /// Run fedora-review on a bug, then digest the result.
    Run(Box<RunArgs>),
}

#[derive(Args)]
struct RunArgs {
    /// Review-request Bugzilla bug id (fedora-review -b).
    #[arg(value_name = "BUGID")]
    bug: String,

    /// COPR(s) with staged deps (owner/project; CSV ok)
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "OWNER/PROJECT",
        help_heading = "Build"
    )]
    copr: Vec<String>,

    /// Mock config; doubles as the COPR chroot name
    #[arg(short, long, value_name = "NAME", help_heading = "Build")]
    mock_config: Option<String>,

    /// Extra mock option(s), appended to fedora-review -o
    #[arg(
        long,
        value_name = "OPT",
        allow_hyphen_values = true,
        help_heading = "Build"
    )]
    mock_option: Vec<String>,

    /// Stop after the fedora-review build (skip digest)
    #[arg(long, help_heading = "Build")]
    no_digest: bool,

    /// Extra repo baseurl(s) to build against (CSV ok)
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "URL",
        help_heading = "Build"
    )]
    repo: Vec<String>,

    /// Unique mock buildroot suffix (mock --uniqueext)
    #[arg(long, value_name = "TEXT", help_heading = "Build")]
    uniqueext: Option<String>,

    #[command(flatten, next_help_heading = "Digest")]
    opts: DigestOpts,

    /// Print the fedora-review command without running it
    #[arg(long, help_heading = "Writing")]
    dry_run: bool,
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

    /// Show a Python spec's changes from pyp2spec with CMD (e.g. difft)
    #[arg(long, value_name = "CMD")]
    diff_tool: Option<String>,

    /// Skip the registry version check and the pyp2spec comparison
    #[arg(long)]
    no_net: bool,

    /// Post to Bugzilla: comment, review flag, claim
    #[arg(long)]
    post: bool,

    /// Base directory for bug-id lookup
    #[arg(long, value_name = "DIR", default_value = ".")]
    reviews_dir: PathBuf,

    /// Accept inferred checklist marks without prompting
    #[arg(short = 'y', long)]
    yes: bool,
}

fn main() -> ExitCode {
    let cli = sandogasa_cli::parse_with_defaults::<Cli>(env!("CARGO_PKG_NAME"));
    let result = match cli.command {
        Some(Command::Config(args)) => cmd_config(&args),
        Some(Command::Digest(args)) => run(&args).map(|text| print!("{text}")),
        Some(Command::Run(args)) => cmd_run(&args),
        None => {
            // The bare form, kept until the next breaking release (#162).
            if let Some(input) = &cli.digest.input {
                warn_bare_form(input, &cli.digest.opts);
            }
            run(&cli.digest).map(|text| print!("{text}"))
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fedora-review-digest: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Warn that the bare `fedora-review-digest <DIR-OR-BUGID>` is
/// deprecated, naming the `digest` command to use instead: the warning
/// in yellow, the command in bold green (DEVELOPMENT.md, "Colors"), set
/// apart by blank lines. At a terminal it waits for Enter, so the
/// warning is read before the review scrolls it away; without one it
/// carries on.
fn warn_bare_form(input: &str, opts: &DigestOpts) {
    use sandogasa_cli::style;
    let color = style::stderr_color();
    eprintln!(
        "\n{}\n  {}\n(or `run <BUGID>` to run fedora-review first)\n",
        style::paint(
            "warning: fedora-review-digest without a subcommand is deprecated, \
             and goes at the next breaking release; use:",
            style::YELLOW,
            color,
        ),
        style::paint(
            &command_hint("digest", input, opts),
            &format!("{}{}", style::BOLD, style::GREEN),
            color,
        ),
    );
    if !opts.yes && std::io::stdin().is_terminal() {
        eprint!("Press Enter to go on, Ctrl-C to stop: ");
        let _ = std::io::stderr().flush();
        let _ = read_answer();
        eprintln!();
    }
}

/// Bugzilla's verdict on `key` for the account `email`: `valid_login`
/// sends the key and asks whether it belongs to that account, so it
/// proves both, which is also why a refusal names the address as well
/// as the key. Shared by `config` and a key typed in at `--post`'s
/// prompt.
fn key_verdict(
    rt: &tokio::runtime::Runtime,
    key: &str,
    email: &str,
) -> sandogasa_cli::credential::KeyVerdict {
    use sandogasa_bugzilla::LoginCheck;
    use sandogasa_cli::credential::KeyVerdict;
    let client = match BzClient::new(BUGZILLA_URL).with_api_key(key.to_string()) {
        Ok(c) => c,
        Err(e) => return KeyVerdict::Unknown(format!("could not check: {e}")),
    };
    match rt.block_on(client.check_login(email)) {
        Ok(LoginCheck::Valid) => KeyVerdict::Accepted("valid.".to_string()),
        Ok(LoginCheck::NotRecognised) => KeyVerdict::Refused(format!(
            "not recognized for {email} — the key, or the address, is not \
             the one Bugzilla has."
        )),
        Ok(LoginCheck::Refused(why)) => KeyVerdict::Refused(format!("refused: {why}")),
        Ok(_) => KeyVerdict::Unknown("could not tell.".to_string()),
        Err(e) => KeyVerdict::Unknown(format!("could not check: {e}")),
    }
}

/// Why a request Bugzilla failed may have been refused, asked on demand
/// once it was (#164): Bugzilla's own reason when the key is the
/// problem ("The API key you specified has expired…"), with where to get
/// a new one; `None` when the key is fine, or that cannot be told.
fn key_refusal(rt: &tokio::runtime::Runtime, key: &str, email: &str) -> Option<String> {
    use sandogasa_cli::credential::KeyVerdict;
    match key_verdict(rt, key, email) {
        KeyVerdict::Refused(why) => Some(refusal_text(&why)),
        KeyVerdict::Accepted(_) | KeyVerdict::Unknown(_) => None,
    }
}

/// What to say about a key Bugzilla refused (`why`, from
/// [`key_verdict`]), and how to replace it.
fn refusal_text(why: &str) -> String {
    format!(
        "Bugzilla API key {why}\nCreate a new key at \
         https://bugzilla.redhat.com/userprefs.cgi?tab=apikey and save it with \
         `fedora-review-digest config`."
    )
}

/// Set up / verify the Bugzilla API key (and email, for validation):
/// the key in the desktop keyring when there is one, the rest at
/// `~/.config/fedora-review-digest/config.toml`.
fn cmd_config(args: &sandogasa_cli::ConfigArgs) -> Result<(), String> {
    use sandogasa_config::{Setting, ask};
    let cf = ConfigFile::for_tool(TOOL);
    // The user file alone, so saving does not copy /etc's values into it.
    let mut cfg: config::Config = cf.load_user().unwrap_or_default();
    // With --token, only the key is asked about, again.
    let host = fedora_review_digest::secrets::bugzilla()
        .service()
        .to_string();
    args.token.check([host.as_str()])?;
    let review = args.review_secret(&host);
    // A key kept in the keyring is the saved one (#79).
    let before = config::fill(keyring(), &mut cfg, review, &args.reenable());
    let hint = sandogasa_config::credentials::token_help(
        "sandogasa: Bugzilla API key",
        "fedora-review-digest",
        BUGZILLA_URL,
    );
    cfg.bugzilla.api_key = ask(
        &Setting::new("Bugzilla", "API key")
            .sensitive()
            .hint(hint.trim_end()),
        cfg.bugzilla.api_key.trim(),
        review,
    )
    .map_err(|e| e.to_string())?;
    // The email is the one the sandogasa tools share (#259): offered
    // from there, or from another tool, when this one has none.
    let email_host = identity::host_of(BUGZILLA_URL);
    if !args.tokens_only() {
        cfg.bugzilla.email = identity::prompt(
            &identity::BUGZILLA_EMAIL,
            &email_host,
            TOOL,
            cfg.bugzilla.email.trim(),
            &Setting::new("Bugzilla", "email").validated(&sandogasa_config::validate_email),
            args.all,
        )
        .map_err(|e| e.to_string())?;
    }
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let email = identity::resolve(&identity::BUGZILLA_EMAIL, &email_host, &cfg.bugzilla.email)
        .unwrap_or_default();
    let answer = cfg.bugzilla.email.clone();
    // Checked against the address too (`key_verdict`).
    cfg.bugzilla.api_key = sandogasa_cli::credential::validated_key_blocking(
        "Bugzilla",
        "API key",
        cfg.bugzilla.api_key.clone(),
        |key| key_verdict(&rt, key, &email),
    )
    .key;
    config::save(&cf, keyring(), &mut cfg, &before).map_err(|e| e.to_string())?;
    // Moved to the shared file once the rest is saved.
    if !args.tokens_only() {
        identity::settle(
            &identity::BUGZILLA_EMAIL,
            &email_host,
            TOOL,
            &answer,
            &args.reenable(),
        )
        .map_err(|e| e.to_string())?;
    }
    if cf.path().exists() {
        eprintln!("Saved to {}", cf.path().display());
    } else {
        eprintln!(
            "Nothing else to keep in {}; removed it.",
            cf.path().display()
        );
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
    // fedora-review refuses to start when the bug's result directory is
    // already there ("… is in the way, please remove"), so settle that
    // first, and let a dry run say so.
    let existing = existing_result(&args.opts.reviews_dir, &args.bug);
    if args.dry_run {
        println!("{}", runner::display_command(&review_args));
        match &existing {
            Ok(None) => {}
            Ok(Some(dir)) => eprintln!(
                "note: {} is already there; the real run asks whether to reuse it",
                dir.display()
            ),
            Err(e) => eprintln!("note: the real run stops here: {e}"),
        }
        return Ok(());
    }
    let existing = existing?;
    let built_differently = !args.copr.is_empty()
        || !args.repo.is_empty()
        || args.mock_config.is_some()
        || !args.mock_option.is_empty()
        || args.uniqueext.is_some();
    let reuse = match &existing {
        None => false,
        Some(dir) => {
            let interactive = !args.opts.yes && std::io::stdin().is_terminal();
            settle_existing(
                dir,
                &args.bug,
                &args.opts,
                interactive,
                built_differently,
                &mut || read_answer(),
                &mut |q| sandogasa_cli::confirm(q, false).unwrap_or(false),
            )?
        }
    };
    if !reuse {
        runner::run_fedora_review(&review_args, &args.opts.reviews_dir)?;
    }
    if args.no_digest {
        let dir = find_review_dir(&args.opts.reviews_dir, &args.bug)?;
        eprintln!("review results in {}", dir.display());
        return Ok(());
    }
    let digest = DigestArgs {
        input: Some(args.bug.clone()),
        opts: DigestOpts {
            comment: args.opts.comment.clone(),
            diff_tool: args.opts.diff_tool.clone(),
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
    let dir = match resolve_input(input, &cli.opts.reviews_dir) {
        Ok(dir) => dir,
        // A bug with no result yet: fedora-review has not run for it.
        Err(e)
            if looks_like_bug_id(input)
                && review_dirs(&cli.opts.reviews_dir, input).is_ok_and(|d| d.is_empty()) =>
        {
            return Err(format!(
                "{e}\nTo run fedora-review for it first, then digest the result:\n  {}",
                command_hint("run", input, &cli.opts)
            ));
        }
        Err(e) => return Err(e),
    };
    let review = Review::from_dir(&dir)?;
    match review.spec.generator {
        Generator::Rust2Rpm | Generator::CabalRpm => {}
        Generator::Pyp2Spec => return run_python(cli, input, &dir, &review),
        Generator::Unknown => {
            return Err(format!(
                "{} is neither a Python spec nor generated by rust2rpm or \
                 cabal-rpm — nothing to digest",
                dir.display()
            ));
        }
    }

    // Evidence for the "latest version" item: the latest on the
    // registry the generator packages from. A lookup failure is a
    // warning, not fatal — the item just reports "not checked".
    let crate_latest = if cli.opts.no_net {
        None
    } else {
        let looked_up = match review.spec.generator {
            Generator::CabalRpm => hackage::fetch_latest_version(&review.upstream_name),
            _ => cratesio::fetch_max_stable_version(&review.upstream_name),
        };
        match looked_up {
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
    // inferred ones with --yes.
    let interactive = interactive(&cli.opts, "accept the inferred marks")?;
    if interactive {
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

    let review_block = checklist::render_review(review.spec.generator, &items, &issues);
    let post_block = checklist::render_post_import(
        review.spec.generator,
        &review.upstream_name,
        &review.spec.name,
    );
    let verdict = if checklist::approved(&items, &issues) {
        Verdict::Approved
    } else {
        Verdict::NotApproved
    };
    finish(
        cli,
        input,
        &dir,
        interactive,
        &review_block,
        &post_block,
        verdict,
    )
}

/// Whether to ask at the terminal: not under `-y`; without `-y`, a run
/// without a terminal cannot ask, so it fails with the remedy rather
/// than `accept` silently.
fn interactive(opts: &DigestOpts, accept: &str) -> Result<bool, String> {
    if opts.yes {
        return Ok(false);
    }
    if !std::io::stdin().is_terminal() {
        return Err(format!(
            "not a terminal; pass -y to {accept} (and --comment for a note)"
        ));
    }
    Ok(true)
}

/// The finished comment: the reviewer's note on top (`--comment`, else
/// asked for when `interactive`), the review block and the post-import
/// block, posted with `verdict` under `--post`.
fn finish(
    cli: &DigestArgs,
    input: &str,
    dir: &Path,
    interactive: bool,
    review_block: &str,
    post_block: &str,
    verdict: Verdict,
) -> Result<String, String> {
    let prompted = cli.opts.comment.is_none() && interactive;
    let comment = match &cli.opts.comment {
        Some(c) => Some(c.clone()),
        None if prompted => prompt_comment()?,
        None => None,
    };
    let text = checklist::assemble(comment.as_deref(), review_block, post_block);
    if cli.opts.post {
        let confirm = if cli.opts.yes {
            Confirm::Yes
        } else {
            Confirm::Ask { default: true }
        };
        if !post_to_bugzilla(input, dir, &text, verdict, confirm)? {
            return Err("aborted".into());
        }
    } else if interactive && bug_id(input, dir).is_some() {
        // Show the comment, then offer to post it (#162): printed here,
        // so it comes before the question, and not again by the caller.
        use sandogasa_cli::style;
        eprintln!(
            "{}{}\n",
            if prompted { "" } else { "\n" },
            style::paint(
                "── The comment ──",
                &format!("{}{}", style::BOLD, style::GREEN),
                style::stderr_color(),
            )
        );
        print!("{text}");
        std::io::stdout().flush().map_err(|e| e.to_string())?;
        // Asked before anything reads a key or the network, so declining,
        // or being offline, still leaves the comment to paste.
        let offered = sandogasa_cli::confirm("Post it to the bug?", false).unwrap_or(false);
        if !offered
            || !post_to_bugzilla(input, dir, &text, verdict, Confirm::Ask { default: true })?
        {
            eprintln!(
                "{}",
                style::paint(
                    "Not posted: paste it into the bug, or rerun with --post.",
                    &format!("{}{}", style::BOLD, style::GREEN),
                    style::stderr_color(),
                )
            );
        }
        return Ok(String::new());
    } else if interactive {
        // The comment goes to stdout next; set it apart from the prompts,
        // and say it was not posted.
        // Something for the user to do: bold green, as --explain shows a
        // command to run (DEVELOPMENT.md, "Colors").
        use sandogasa_cli::style;
        // The comment prompt ends with an empty line already.
        eprintln!(
            "{}{}\n",
            if prompted { "" } else { "\n" },
            style::paint(
                "── Post this comment on the bug (or rerun with --post) ──",
                &format!("{}{}", style::BOLD, style::GREEN),
                style::stderr_color(),
            )
        );
    }
    Ok(text)
}

/// The digest of a Python spec: how it differs from what pyp2spec
/// generates for the same release (#160), then fedora-review's issues
/// and the Python checklist with its verdict (#161).
fn run_python(
    cli: &DigestArgs,
    input: &str,
    dir: &Path,
    review: &Review,
) -> Result<String, String> {
    let interactive = interactive(&cli.opts, "accept the findings")?;
    let comparison = if cli.opts.no_net {
        Err("--no-net was given".to_string())
    } else {
        eprintln!("Generating the spec with pyp2spec, from PyPI...");
        pyp2spec::compare(
            &review.spec_text,
            &review.spec.name,
            &review.spec.version,
            &pyp2spec::run_pyp2spec,
        )
    };
    if let (Err(e), false) = (&comparison, cli.opts.no_net) {
        eprintln!("warning: {e}");
    }
    let compared = |reader| match &comparison {
        Ok(c) => pyp2spec::render(c, reader),
        Err(e) => format!("The spec was not compared with pyp2spec's output: {e}.\n"),
    };

    // The checklist's evidence from PyPI: the release's name and version
    // in PyPI's form, its latest version, and the license its metadata
    // gives (pyp2spec's License:).
    let (pypi_name, version) = match &comparison {
        Ok(c) => (c.name.clone(), c.version.clone()),
        Err(_) => pyp2spec::release(&review.spec_text, &review.spec.name, &review.spec.version),
    };
    let latest = if cli.opts.no_net {
        None
    } else {
        match pypi::fetch_latest_version(&pypi_name) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("warning: {e}; skipping the latest-version check");
                None
            }
        }
    };
    let upstream_license = comparison
        .as_ref()
        .ok()
        .and_then(|c| c.upstream_license.as_deref());
    let mut items = checklist::python_checklist(
        review,
        &checklist::PythonEvidence {
            version: &version,
            latest: latest.as_deref(),
            upstream_license,
            not_compared: comparison.as_ref().err().map(String::as_str),
        },
    );
    let mut issues = checklist::reviewed(&review.issues);
    if interactive {
        eprintln!(
            "── review of {} {} ──\n",
            review.spec.name, review.spec.version
        );
        let tool = cli.opts.diff_tool.clone().or_else(config::diff_tool);
        match (&comparison, tool) {
            (Ok(c), Some(tool)) if !c.diff.is_empty() => match pyp2spec::show_with(&tool, c) {
                Ok(()) => eprint!("\n{}", pyp2spec::sources(c, pyp2spec::Reader::Reviewer)),
                Err(e) => {
                    eprintln!("warning: {e}; the unified diff follows");
                    eprint!("{}", compared(pyp2spec::Reader::Reviewer));
                }
            },
            _ => eprint!("{}", compared(pyp2spec::Reader::Reviewer)),
        }
        eprintln!("\n── inferred checklist ──\n");
        eprint!(
            "{}",
            checklist::render_python_review("", &items, &issues).trim_start()
        );
        eprintln!("\n── confirm each item ──");
        finalize(&mut items)?;
        finalize_issues(&mut issues)?;
    }
    let review_block =
        checklist::render_python_review(&compared(pyp2spec::Reader::Comment), &items, &issues);
    let post_block =
        checklist::render_post_import(review.spec.generator, &pypi_name, &review.spec.name);
    let verdict = if checklist::approved(&items, &issues) {
        Verdict::Approved
    } else {
        Verdict::NotApproved
    };
    finish(
        cli,
        input,
        dir,
        interactive,
        &review_block,
        &post_block,
        verdict,
    )
}

/// Whether [`post_to_bugzilla`] asks before it writes.
#[derive(Debug, Clone, Copy)]
enum Confirm {
    /// `-y`: post without asking.
    Yes,
    /// Ask at the terminal, with this answer on Enter: yes under
    /// `--post`, no when the post is only offered.
    Ask { default: bool },
}

/// Post the assembled review to its Bugzilla review bug: a comment, the
/// `fedora-review` flag (and, on approval, status POST), and claiming the
/// bug for the reviewer. Fetches the bug first (to validate it, read the
/// current flag, and see who it's assigned to), shows what will change,
/// and confirms before the write (`confirm`). Whether it posted: false
/// when the reviewer declined.
fn post_to_bugzilla(
    input: &str,
    dir: &Path,
    digest: &str,
    verdict: Verdict,
    confirm: Confirm,
) -> Result<bool, String> {
    let bug_id = bug_id(input, dir)
        .ok_or("couldn't determine the bug id from the input or dir name; pass the bug id")?;
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let creds = config::credentials(keyring(), &|key, email| key_verdict(&rt, key, email))
        .map_err(|e| e.to_string())?;
    let client = BzClient::new(BUGZILLA_URL)
        .with_api_key(creds.api_key.clone())
        .map_err(|e| e.to_string())?;
    // Bugzilla's refusal of the key, asked only once a request failed.
    let why = |what: String| match key_refusal(&rt, &creds.api_key, &creds.email) {
        Some(refusal) => format!("{what}\n{refusal}"),
        None => what,
    };

    let bug = match rt.block_on(client.bug(bug_id)) {
        Ok(bug) => bug,
        Err(e) if e.status().is_some_and(|s| s.is_client_error()) => {
            return Err(why(format!("fetching bug {bug_id}: {e}")));
        }
        Err(e) => return Err(format!("fetching bug {bug_id}: {e}")),
    };
    let current_flag = bugzilla::current_review_flag(&bug);
    // Claim the bug by assigning it to the reviewer — unless it's already
    // theirs, in which case there's nothing to change.
    let claim =
        (!bug.assigned_to.eq_ignore_ascii_case(&creds.email)).then_some(creds.email.as_str());

    eprintln!("\nBug {bug_id}: {}", bug.summary);
    eprintln!(
        "Will {}.",
        bugzilla::action_summary(verdict, current_flag, claim.is_some())
    );
    if let Confirm::Ask { default } = confirm {
        if !std::io::stdin().is_terminal() {
            return Err("not a terminal; pass -y to post to Bugzilla".into());
        }
        if !sandogasa_cli::confirm("Post to Bugzilla?", default).map_err(|e| e.to_string())? {
            return Ok(false);
        }
    }

    let body = bugzilla::update_body(digest, verdict, current_flag, claim);
    let out = rt.block_on(client.update_verified(bug_id, &body, 3));
    if let Some(note) = out.note() {
        eprintln!("note: bug {bug_id}: {note}");
    }
    if !out.complete() {
        return Err(why(format!(
            "posting to bug {bug_id}: {}",
            out.last_error.unwrap_or_default()
        )));
    }
    eprintln!("Posted to bug {bug_id}.");
    Ok(true)
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

/// The fedora-review result directories for `bug_id` under `base`
/// (`<bug_id>-*`), sorted. A `base` that does not exist is an error
/// rather than "no result", so a mistyped `--reviews-dir` is caught.
fn review_dirs(base: &Path, bug_id: &str) -> Result<Vec<PathBuf>, String> {
    let prefix = format!("{bug_id}-");
    let entries = std::fs::read_dir(base).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("--reviews-dir {} does not exist", base.display())
        } else {
            format!("reading {}: {e}", base.display())
        }
    })?;
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(&prefix))
        })
        .collect();
    dirs.sort();
    Ok(dirs)
}

/// The one result directory for `bug_id` under `base`, if there is one;
/// several are an error, as nothing says which to use.
fn existing_result(base: &Path, bug_id: &str) -> Result<Option<PathBuf>, String> {
    let mut dirs = review_dirs(base, bug_id)?;
    match dirs.len() {
        0 => Ok(None),
        1 => Ok(dirs.pop()),
        _ => Err(format!(
            "multiple {bug_id}-* dirs under {}; pass the path directly, or \
             remove all but one",
            base.display()
        )),
    }
}

/// Find the review directory for a bug id under `base` (`<id>-*`).
fn find_review_dir(base: &Path, bug_id: &str) -> Result<PathBuf, String> {
    existing_result(base, bug_id)?.ok_or_else(|| {
        format!(
            "no review dir for bug {bug_id} under {} (looked for {bug_id}-*)",
            base.display()
        )
    })
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

/// `arg` as one word for a POSIX or fish shell: bare when it is plain,
/// else single-quoted.
fn shell_word(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@+,".contains(c));
    if plain {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

/// The `subcommand` command line for `bug` with the digest options
/// given, to suggest: `run` when there is no result yet, `digest` when
/// `run` will not run over one.
fn command_hint(subcommand: &str, bug: &str, opts: &DigestOpts) -> String {
    let mut argv = vec!["fedora-review-digest".to_string(), subcommand.to_string()];
    if opts.reviews_dir != Path::new(".") {
        argv.push("--reviews-dir".into());
        argv.push(opts.reviews_dir.display().to_string());
    }
    if let Some(c) = &opts.comment {
        argv.push("--comment".into());
        argv.push(c.clone());
    }
    if let Some(t) = &opts.diff_tool {
        argv.push("--diff-tool".into());
        argv.push(t.clone());
    }
    for (on, flag) in [
        (opts.no_net, "--no-net"),
        (opts.post, "--post"),
        (opts.yes, "-y"),
    ] {
        if on {
            argv.push(flag.into());
        }
    }
    argv.push(bug.to_string());
    argv.iter()
        .map(|a| shell_word(a))
        .collect::<Vec<_>>()
        .join(" ")
}

/// What to do with a result directory `run` finds already there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Existing {
    Reuse,
    Delete,
    Abort,
}

/// An answer to [`settle_existing`]'s question: Enter reuses.
fn parse_existing(answer: &str) -> Option<Existing> {
    match answer.trim().to_ascii_lowercase().as_str() {
        "" | "r" | "reuse" => Some(Existing::Reuse),
        "d" | "delete" => Some(Existing::Delete),
        "a" | "abort" => Some(Existing::Abort),
        _ => None,
    }
}

/// Settle a result directory `dir` that `run` found for `bug`: reuse it
/// (true: digest it without running fedora-review), or delete it after
/// `confirm` (false: run fedora-review again). `read` gives each answer,
/// `None` at the end of input, which aborts. Without `interactive`,
/// stops with the `digest` command to use instead. `built_differently`
/// says the run was given build options, which a reused result was not
/// built with.
fn settle_existing(
    dir: &Path,
    bug: &str,
    opts: &DigestOpts,
    interactive: bool,
    built_differently: bool,
    read: &mut dyn FnMut() -> Option<String>,
    confirm: &mut dyn FnMut(&str) -> bool,
) -> Result<bool, String> {
    if !interactive {
        return Err(format!(
            "{} is already there, and fedora-review will not run over it; \
             digest it with `{}`, or remove it and run again",
            dir.display(),
            command_hint("digest", bug, opts)
        ));
    }
    eprintln!(
        "{} is already there (fedora-review will not run over it).\n  \
         r) reuse it: digest the existing result (default){}\n  \
         d) delete it and run fedora-review again\n  \
         a) abort",
        dir.display(),
        if built_differently {
            ", built without the build options given now"
        } else {
            ""
        }
    );
    loop {
        eprint!("Reuse, delete or abort? [R/d/a]: ");
        let _ = std::io::stderr().flush();
        let Some(answer) = read() else {
            return Err("aborted".into());
        };
        match parse_existing(&answer) {
            Some(Existing::Reuse) => return Ok(true),
            Some(Existing::Abort) => return Err("aborted".into()),
            Some(Existing::Delete) => {
                if !confirm(&format!("Delete {}?", dir.display())) {
                    return Err("aborted".into());
                }
                std::fs::remove_dir_all(dir)
                    .map_err(|e| format!("removing {}: {e}", dir.display()))?;
                eprintln!("Removed {}.", dir.display());
                return Ok(false);
            }
            None => eprintln!("Answer r, d or a."),
        }
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

/// A line from stdin, `None` at the end of input (or on a read error),
/// told apart from an empty answer, which is a newline.
fn read_answer() -> Option<String> {
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line),
    }
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
    /// `--token` names bugzilla.redhat.com, where the key is kept;
    /// another host is refused before anything is asked.
    #[test]
    fn config_token_names_bugzilla() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(flatten)]
            config: sandogasa_cli::ConfigArgs,
        }
        let args =
            <Cli as clap::Parser>::parse_from(["fedora-review-digest", "--token", "gitlab.com"]);
        assert_eq!(
            super::cmd_config(&args.config).unwrap_err(),
            "--token: no secret is kept for gitlab.com; this configuration has: \
             bugzilla.redhat.com"
        );
    }

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

    fn opts(reviews_dir: &str) -> DigestOpts {
        DigestOpts {
            comment: None,
            diff_tool: None,
            no_net: false,
            post: false,
            reviews_dir: PathBuf::from(reviews_dir),
            yes: false,
        }
    }

    #[test]
    fn the_run_hint_carries_the_options_given() {
        assert_eq!(
            command_hint("run", "2575659", &opts(".")),
            "fedora-review-digest run 2575659"
        );
        let mut o = opts("/home/me/reviews");
        o.comment = Some("LGTM, it's fine".into());
        o.diff_tool = Some("difft".into());
        o.post = true;
        o.yes = true;
        assert_eq!(
            command_hint("run", "2575659", &o),
            "fedora-review-digest run --reviews-dir /home/me/reviews \
             --comment 'LGTM, it'\\''s fine' --diff-tool difft --post -y 2575659"
        );
    }

    #[test]
    fn an_existing_result_is_reused_on_enter() {
        assert_eq!(parse_existing(""), Some(Existing::Reuse));
        assert_eq!(parse_existing(" R\n"), Some(Existing::Reuse));
        assert_eq!(parse_existing("delete"), Some(Existing::Delete));
        assert_eq!(parse_existing("a"), Some(Existing::Abort));
        assert_eq!(parse_existing("x"), None);
    }

    #[test]
    fn result_dirs_are_found_by_bug_id() {
        let base = tempfile::tempdir().unwrap();
        // A --reviews-dir that does not exist is an error, not "no result".
        assert_eq!(
            review_dirs(&base.path().join("missing"), "1"),
            Err(format!(
                "--reviews-dir {} does not exist",
                base.path().join("missing").display()
            ))
        );
        std::fs::create_dir(base.path().join("2575659-python-cstq")).unwrap();
        std::fs::create_dir(base.path().join("25756590-other")).unwrap();
        std::fs::write(base.path().join("2575659-file"), "").unwrap();
        assert_eq!(
            existing_result(base.path(), "2575659"),
            Ok(Some(base.path().join("2575659-python-cstq")))
        );
        assert_eq!(existing_result(base.path(), "1"), Ok(None));
        assert!(
            find_review_dir(base.path(), "1")
                .unwrap_err()
                .starts_with("no review dir")
        );
        std::fs::create_dir(base.path().join("2575659-again")).unwrap();
        assert!(
            existing_result(base.path(), "2575659")
                .unwrap_err()
                .starts_with("multiple")
        );
    }

    /// [`settle_existing`] at a terminal, answering `answers` in turn
    /// (the end of input after them) and `delete` to the deletion.
    fn settle(dir: &Path, answers: &[&str], delete: bool) -> Result<bool, String> {
        let mut answers = answers.iter().map(|a| a.to_string());
        settle_existing(
            dir,
            "1",
            &opts("."),
            true,
            false,
            &mut || answers.next(),
            &mut |_| delete,
        )
    }

    #[test]
    fn an_existing_result_is_reused_deleted_or_left() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("1-x");
        std::fs::create_dir(&dir).unwrap();
        // Enter reuses; an answer it cannot read is asked again.
        assert_eq!(settle(&dir, &["\n"], false), Ok(true));
        assert_eq!(settle(&dir, &["maybe\n", "r\n"], false), Ok(true));
        // The end of input, or a, stops.
        assert_eq!(settle(&dir, &[], false), Err("aborted".into()));
        assert_eq!(settle(&dir, &["a\n"], false), Err("aborted".into()));
        // Delete, declined at the second question: stops, nothing removed.
        assert_eq!(settle(&dir, &["d\n"], false), Err("aborted".into()));
        assert!(dir.is_dir());
        // Delete, confirmed: removed, and fedora-review runs again.
        assert_eq!(settle(&dir, &["d\n"], true), Ok(false));
        assert!(!dir.exists());
    }

    #[test]
    fn an_existing_result_without_a_terminal_says_how_to_go_on() {
        let mut o = opts("/home/me/reviews");
        o.yes = true;
        o.post = true;
        let err = settle_existing(
            Path::new("/home/me/reviews/1-x"),
            "1",
            &o,
            false,
            false,
            &mut || panic!("asked"),
            &mut |_| panic!("asked"),
        )
        .unwrap_err();
        // The options carry over, so the command works where this ran.
        assert!(
            err.contains(
                "digest it with `fedora-review-digest digest --reviews-dir \
                 /home/me/reviews --post -y 1`"
            ),
            "{err}"
        );
    }

    #[test]
    fn a_refused_key_says_why_and_how_to_replace_it() {
        let text = refusal_text(
            "refused: The API key you specified has expired. The API key was \
             expired in bug https://bugzilla.redhat.com/show_bug.cgi?id=2379663",
        );
        assert!(
            text.starts_with("Bugzilla API key refused: The API key you specified has expired.")
        );
        assert!(text.ends_with(
            "Create a new key at https://bugzilla.redhat.com/userprefs.cgi?tab=apikey \
             and save it with `fedora-review-digest config`."
        ));
    }
}
