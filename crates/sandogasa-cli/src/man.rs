// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Man page generation from a clap [`Command`].
//!
//! Each tool ships one page, `man/<tool>.1`, rendered from the same
//! clap definition that produces `--help` — so the two cannot
//! disagree about what the tool accepts. Subcommands become
//! subsections of a COMMANDS section rather than separate pages, so
//! `man <tool>` documents the whole CLI in one place.
//!
//! A tool's clap type is private to its binary, so generation is
//! driven from a unit test in `main.rs`:
//!
//! ```ignore
//! #[cfg(test)]
//! mod tests {
//!     #[test]
//!     fn man_page_matches_cli() {
//!         sandogasa_cli::man::check::<super::Cli>(concat!(
//!             env!("CARGO_MANIFEST_DIR"),
//!             "/man/koji-diff.1"
//!         ));
//!     }
//! }
//! ```
//!
//! `scripts/gen-man.sh` (`SANDOGASA_UPDATE_MAN=1 cargo test
//! --workspace`) rewrites every page in place. Without that
//! variable the test asserts that the committed page documents every
//! visible flag and subcommand and carries the current version — not
//! that it is byte-identical to what this version of clap_mangen
//! would produce: a distro build runs `%check` against the
//! clap_mangen *it* packages, and must not fail over roff formatting
//! that changed between patch releases. Exact regeneration belongs
//! to the release flow, where `git diff` shows any wording that
//! moved.
//!
//! Because the version is checked, bumping the workspace version
//! makes every page's test fail until the pages are regenerated —
//! which is deliberate: it puts "regenerate after the version bump"
//! in the test suite instead of only in the release checklist.

use std::io::Write;
use std::path::Path;

use clap::{Command, CommandFactory};
use clap_mangen::Man;

/// The `.TH` manual field: the header's centre text.
const MANUAL: &str = "Sandogasa Manual";

/// The `.TH` source field: the footer's left text, naming the
/// project and its version the way `man bash` shows "GNU Bash 5.3".
fn source(cmd: &Command) -> String {
    match cmd.get_version() {
        Some(version) => format!("sandogasa {version}"),
        None => "sandogasa".to_string(),
    }
}

/// Render `cmd` and its visible subcommands as one roff man page,
/// dated `date` (`YYYY-MM-DD`, shown in the footer's centre).
pub fn render_dated(cmd: Command, date: &str) -> Vec<u8> {
    let mut cmd = cmd;
    cmd.build();
    // The date must be set: clap_mangen writes an unset date as bare
    // whitespace, which groff then parses as an *absent* field, so
    // every later `.TH` field shifts down one — the source lands in
    // the date's place and the manual in the source's.
    let man = Man::new(cmd.clone())
        .date(date)
        .source(source(&cmd))
        .manual(MANUAL);
    let mut out = Vec::new();
    // The title block carries the page's one roff preamble; every
    // later block repeats it and has it stripped.
    man.render_title(&mut out).expect(EXPECT);
    push_body(&mut out, &man, Man::render_name_section, false);
    push_body(&mut out, &man, Man::render_synopsis_section, false);
    push_body(&mut out, &man, Man::render_description_section, false);
    push_body(&mut out, &man, Man::render_options_section, false);
    let name = cmd.get_name().to_string();
    if subcommands(&cmd).next().is_some() {
        out.extend_from_slice(b".SH COMMANDS\n");
        for sub in subcommands(&cmd) {
            render_subcommand(&mut out, &name, sub);
        }
    }
    out
}

/// Render `cmd` as a man page dated today.
pub fn render(cmd: Command) -> Vec<u8> {
    render_dated(cmd, &today())
}

/// Today's date in UTC, `YYYY-MM-DD`.
fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// Regenerate the page at `path` when `SANDOGASA_UPDATE_MAN` is
/// set; otherwise assert that the committed page documents every
/// visible flag and subcommand of `C` and carries the current
/// version.
///
/// Panics with the regeneration command on any mismatch — it is
/// called from tests, where a panic is the failure.
pub fn check<C: CommandFactory>(path: &str) {
    let source = source(&C::command());
    if std::env::var_os("SANDOGASA_UPDATE_MAN").is_some() {
        update::<C>(Path::new(path), &source);
        return;
    }
    let committed = std::fs::read(path).unwrap_or_else(|e| panic!("{path}: {e}\n{REGENERATE}"));
    // Roff escapes hyphens as `\-`, and wraps names in `\fB...\fR`;
    // dropping every backslash leaves the flag, command and version
    // names findable without reimplementing the escaping rules.
    let flat = String::from_utf8_lossy(&committed).replace('\\', "");
    let (title, _) = split_title(&flat);
    assert!(
        !title.is_empty(),
        "{path} does not look like a man page\n{REGENERATE}"
    );
    assert!(
        title.contains(&source),
        "{path} does not name `{source}` — the version moved on\n{REGENERATE}"
    );
    for token in tokens(&C::command()) {
        assert!(
            flat.contains(&token),
            "{path} does not document `{token}`\n{REGENERATE}"
        );
    }
}

/// Write the page for `C` to `path`, keeping the committed file when
/// only its date would change.
///
/// Every page is regenerated at once, so stamping today's date
/// unconditionally would rewrite all of them whenever one tool's
/// flags change. Keeping a page whose content still matches leaves
/// `git diff` showing the tools that actually changed.
fn update<C: CommandFactory>(path: &Path, source: &str) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    }
    let page = render(C::command());
    if let Ok(committed) = std::fs::read_to_string(path) {
        let fresh = String::from_utf8_lossy(&page);
        let (old_title, old_body) = split_title(&committed);
        let (_, new_body) = split_title(&fresh);
        if old_body == new_body && old_title.contains(source) {
            return;
        }
    }
    std::fs::write(path, &page).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

/// Split a page into its `.TH` line and everything after it. An
/// empty title means no `.TH` line was found.
fn split_title(page: &str) -> (&str, &str) {
    let Some(start) = page.find(".TH ") else {
        return ("", page);
    };
    let end = page[start..]
        .find('\n')
        .map_or(page.len(), |n| start + n + 1);
    (&page[start..end], &page[end..])
}

/// `expect` message for writes into a `Vec`, which cannot fail.
const EXPECT: &str = "writing roff to a Vec cannot fail";

/// Shown on every failure so the fix never has to be looked up.
const REGENERATE: &str = "regenerate the man pages with scripts/gen-man.sh";

/// The subcommands a man page should document: everything except
/// hidden commands and clap's built-in `help`.
fn subcommands(cmd: &Command) -> impl Iterator<Item = &Command> {
    cmd.get_subcommands()
        .filter(|c| !c.is_hide_set() && c.get_name() != "help")
}

/// Drop one leading line when `pred` accepts it.
fn strip_line(block: &str, pred: impl Fn(&str) -> bool) -> &str {
    match block.split_once('\n') {
        Some((head, rest)) if pred(head) => rest,
        _ => block,
    }
}

/// Strip the repeated roff preamble, and optionally the `.SH`
/// header, from one of clap_mangen's blocks.
///
/// clap_mangen prefixes every block with the same two lines defining
/// the `Aq` quote string; one copy per page is enough. Each block
/// also opens its own `.SH` section, which inside a `.SS` subsection
/// would close the enclosing one.
fn body(block: &str, strip_header: bool) -> &str {
    let block = strip_line(block, |l| l.starts_with(".ie "));
    let block = strip_line(block, |l| l.starts_with(".el "));
    if strip_header {
        strip_line(block, |l| l.starts_with(".SH"))
    } else {
        block
    }
}

/// Render one of clap_mangen's section blocks into the page.
fn push_body(
    out: &mut Vec<u8>,
    man: &Man,
    section: fn(&Man, &mut dyn Write) -> std::io::Result<()>,
    strip_header: bool,
) {
    let mut block = Vec::new();
    section(man, &mut block).expect(EXPECT);
    let block = String::from_utf8_lossy(&block);
    out.extend_from_slice(body(&block, strip_header).as_bytes());
}

/// Render one subcommand as a `.SS` subsection, recursing into its
/// own subcommands. `path` is the command path so far, so nested
/// commands read as `tool sub subsub`.
fn render_subcommand(out: &mut Vec<u8>, path: &str, cmd: &Command) {
    let name = format!("{path} {}", cmd.get_name());
    // clap_mangen takes the synopsis from the display name; setting
    // it makes the subsection read `tool sub [OPTIONS]` rather than
    // just `sub`.
    let sub = cmd.clone().display_name(name.clone());
    let man = Man::new(sub.clone());
    writeln!(out, ".SS {name}").expect(EXPECT);
    push_body(out, &man, Man::render_synopsis_section, true);
    // The synopsis is a bare line of text; start a paragraph so the
    // description does not run on from it.
    out.extend_from_slice(b".PP\n");
    push_body(out, &man, Man::render_description_section, true);
    push_body(out, &man, Man::render_options_section, true);
    for nested in subcommands(&sub) {
        render_subcommand(out, &name, nested);
    }
}

/// Every long flag and subcommand name the CLI exposes, recursively.
/// These are what the coverage check looks for in the committed
/// page.
fn tokens(cmd: &Command) -> Vec<String> {
    let mut out = Vec::new();
    collect_tokens(cmd, &mut out);
    out
}

fn collect_tokens(cmd: &Command, out: &mut Vec<String>) {
    for arg in cmd.get_arguments().filter(|a| !a.is_hide_set()) {
        if let Some(long) = arg.get_long() {
            out.push(format!("--{long}"));
        }
    }
    for sub in subcommands(cmd) {
        out.push(sub.get_name().to_string());
        collect_tokens(sub, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A CLI shaped like the tools': flags, a subcommand with its
    /// own flags, and a hidden subcommand that must stay out.
    fn cmd() -> Command {
        Command::new("demo")
            .about("Demo tool")
            .arg(clap::Arg::new("json").long("json").num_args(0))
            .subcommand(
                Command::new("show")
                    .about("Show a thing")
                    .arg(clap::Arg::new("all").long("all").num_args(0)),
            )
            .subcommand(Command::new("secret").hide(true))
    }

    #[test]
    fn render_documents_flags_and_subcommands() {
        let page = String::from_utf8(render(cmd())).unwrap();
        assert!(page.contains(".TH demo 1"), "{page}");
        assert!(page.contains(".SH COMMANDS"));
        assert!(page.contains(".SS demo show"));
        // The subsection carries the subcommand's own options, with
        // clap_mangen's `.SH OPTIONS` header stripped.
        assert!(page.contains("\\-\\-all"));
        assert_eq!(page.matches(".SH OPTIONS").count(), 1, "{page}");
        // Hidden subcommands and the built-in `help` stay out.
        assert!(!page.contains("secret"), "{page}");
        assert!(!page.contains(".SS demo help"), "{page}");
    }

    #[test]
    fn render_titles_carry_the_date_and_version() {
        let page = String::from_utf8(render_dated(cmd().version("1.2.3"), "2026-08-05")).unwrap();
        let (title, _) = split_title(&page);
        // `.TH <title> <section> <date> <source> <manual>` — all five
        // fields present and in order, so groff does not shift them.
        assert_eq!(
            title.trim_end(),
            r#".TH demo 1 2026-08-05 "sandogasa 1.2.3" "Sandogasa Manual""#,
            "{title}"
        );
        // `--version` is still a documented flag.
        assert!(page.contains("\\-\\-version"), "{page}");
    }

    #[test]
    fn render_without_a_version_still_names_the_project() {
        let page = String::from_utf8(render_dated(cmd(), "2026-08-05")).unwrap();
        let (title, _) = split_title(&page);
        assert!(title.contains(" sandogasa "), "{title}");
    }

    /// A `CommandFactory` for [`cmd`], since `update` is generic over
    /// one the way the tools' `Cli` structs are.
    struct Demo;

    impl clap::CommandFactory for Demo {
        fn command() -> Command {
            cmd().version("1.2.3")
        }
        fn command_for_update() -> Command {
            Self::command()
        }
    }

    #[test]
    fn update_keeps_a_page_whose_content_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.1");
        let source = source(&Demo::command());

        // A page generated in the past, still current apart from its
        // date, is left alone — one tool's change must not restamp
        // every other tool's page.
        let old = render_dated(Demo::command(), "2020-01-01");
        std::fs::write(&path, &old).unwrap();
        update::<Demo>(&path, &source);
        assert_eq!(std::fs::read(&path).unwrap(), old);

        // A stale version rewrites it, even with the body unchanged.
        let stale = String::from_utf8(old.clone())
            .unwrap()
            .replace("sandogasa 1.2.3", "sandogasa 1.0.0");
        std::fs::write(&path, &stale).unwrap();
        update::<Demo>(&path, &source);
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("sandogasa 1.2.3"), "{written}");
        assert!(!written.contains("2020-01-01"), "{written}");
    }

    #[test]
    fn update_writes_a_missing_page() {
        let dir = tempfile::tempdir().unwrap();
        // A nested path: the tool's `man/` directory may not exist.
        let path = dir.path().join("man").join("demo.1");
        update::<Demo>(&path, &source(&Demo::command()));
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains(".TH demo 1"), "{written}");
    }

    #[test]
    fn split_title_finds_the_th_line() {
        let (title, body) = split_title(".ie x\n.TH a 1 b\nrest\n");
        assert_eq!(title, ".TH a 1 b\n");
        assert_eq!(body, "rest\n");
        assert_eq!(split_title("no title\n"), ("", "no title\n"));
    }

    #[test]
    fn tokens_cover_nested_commands() {
        let found = tokens(&cmd());
        assert!(found.contains(&"--json".to_string()));
        assert!(found.contains(&"show".to_string()));
        assert!(found.contains(&"--all".to_string()));
        assert!(!found.contains(&"secret".to_string()));
    }

    #[test]
    fn body_strips_the_preamble_and_optionally_the_header() {
        let block = ".ie x\n.el y\n.SH OPTIONS\nbody\n";
        assert_eq!(body(block, false), ".SH OPTIONS\nbody\n");
        assert_eq!(body(block, true), "body\n");
        assert_eq!(body("no preamble\n", true), "no preamble\n");
    }
}
