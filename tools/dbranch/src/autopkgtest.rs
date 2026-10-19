// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Write an autopkgtest that runs the programs a package installs.
//!
//! A package can build, lint clean, and install a program that does
//! not start -- because nothing in a build runs it. The cheapest
//! guard is the one a reviewer asks for by name: a test that invokes
//! what you shipped, "even if that's just to display the help text".
//!
//! The awkward part is that there is no single way to ask a program
//! for help. `--help` is the common answer, `help` is the subcommand
//! style, and `-h` is the terse one; a generated test that guesses
//! wrong is worse than none, because it fails for a reason that has
//! nothing to do with the package. So the verb is not guessed: each
//! program is run until one of them works, and that is what gets
//! written.
//!
//! debcargo already generates test stanzas for a crate with a library
//! package. Those are kept and ours goes above them, which is how
//! `threecpio` carries a hand-written CLI test beside its generated
//! ones. A crate built binary-only (`lib = false`) gets no generated
//! stanzas at all, so what is written here is the whole file.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The ways a program might agree to describe itself, in the order
/// they are worth trying. `--help` first because it is the GNU
/// convention and by far the most common; `help` next for the
/// subcommand style; `-h` last, since on some programs it means
/// something else entirely.
pub const HELP_VERBS: [&str; 3] = ["--help", "help", "-h"];

/// A program and the verb it answered to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probed {
    /// The installed path, as it will appear in the test.
    pub program: String,
    /// The verb that worked, or `None` if none did.
    pub verb: Option<String>,
}

/// What to write and where.
pub struct Options {
    /// The package working directory holding `debian/`.
    pub repo: PathBuf,
    /// Built package(s) whose programs are probed.
    pub debs: Vec<PathBuf>,
}

/// The `debian/tests/smoke` script for the probed programs.
///
/// `set -eu` so a program that fails fails the test; the point is to
/// execute what was shipped, not to collect output. Programs that
/// answered to nothing are written as a comment rather than silently
/// dropped, so the gap is visible in the file a reviewer reads.
pub fn smoke_script(probed: &[Probed]) -> String {
    let mut out = String::from("#!/bin/sh\nset -eu\n\n");
    for entry in probed {
        match &entry.verb {
            Some(verb) => out.push_str(&format!("{} {verb}\n", entry.program)),
            None => out.push_str(&format!(
                "# {} answered to none of {} -- add a command that exercises it\n",
                entry.program,
                HELP_VERBS.join(", ")
            )),
        }
    }
    out
}

/// The `debian/tests/control` naming the smoke test.
///
/// `Depends: @` pulls in every binary package this source builds,
/// which is what makes the test run against the real installation
/// rather than a build tree. `allow-stderr` because a great many
/// programs print their help there and that is not a failure.
///
/// `generated` is debcargo's own `control.debcargo.hint`, when there
/// is one: its stanzas are kept below ours so the crate's cargo tests
/// keep running too.
pub fn tests_control(generated: Option<&str>) -> String {
    let mut out = String::from("Tests: smoke\nDepends: @\nRestrictions: allow-stderr\n");
    if let Some(generated) = generated {
        let generated = generated.trim();
        if !generated.is_empty() {
            out.push('\n');
            out.push_str(generated);
            out.push('\n');
        }
    }
    out
}

/// Whether a `debian/tests/control` names a test script.
///
/// A `Tests:` stanza is a script somebody wrote; a `Test-Command:`
/// stanza is a command a generator emitted. The distinction is what
/// separates "a person tested this" from "a tool exercised the
/// library", and both this module and `check` turn on it.
pub fn declares_test(control: &str) -> bool {
    control
        .lines()
        .any(|l| l.trim_start().starts_with("Tests:"))
}

/// What writing the test should do about what is already there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Nothing is there: write both files.
    Fresh,
    /// Stanzas exist but none of them runs anything (debcargo's
    /// generated cargo tests). Keep them, put the smoke test above.
    AddSmoke(String),
    /// A test is already configured, so there is nothing to add.
    Stop(String),
}

/// Decide what to do about an existing `debian/tests`.
///
/// Stopping is the whole answer to "what if it already has one". A
/// `Tests:` stanza means a person wrote a test script and named it,
/// and regenerating would overwrite work that is almost always more
/// specific than anything probing can produce. There is deliberately
/// no `--force`: a flag that overwrites a hand-written test is one
/// that will eventually be used by accident, and removing the stanza
/// yourself is a clearer way to say you meant it.
///
/// debcargo's generated `Test-Command:` stanzas are a different
/// thing. They run the crate's cargo tests, never the installed
/// program, so a package carrying only those still has the gap --
/// and the smoke test goes above them with both kept.
pub fn plan(control: Option<&str>, smoke_exists: bool) -> Plan {
    let Some(control) = control else {
        return match smoke_exists {
            // No control naming it, so nothing runs it; but it is
            // still somebody's file and not ours to overwrite.
            true => Plan::Stop(
                "debian/tests/smoke exists but debian/tests/control does \
                 not name it — fix the control file rather than letting \
                 this overwrite the script"
                    .into(),
            ),
            false => Plan::Fresh,
        };
    };
    if declares_test(control) {
        return Plan::Stop(
            "debian/tests/control already declares a test — remove its \
             Tests: stanza first if you want this to write a new one"
                .into(),
        );
    }
    Plan::AddSmoke(control.to_string())
}

/// Probe each program for a verb it answers to, and write the test.
///
/// Returns what was probed, so the caller can say which verb each
/// program took rather than leaving it to be read out of the file.
pub fn run(opts: &Options, ui: &crate::ui::Ui) -> Result<Vec<Probed>, String> {
    let debian = opts.repo.join("debian");
    if !debian.is_dir() {
        // The absolute path, not the literal `.`: being somewhere
        // other than you thought is exactly how this is reached.
        return Err(format!(
            "{} has no debian/ directory — use -C to point at the package",
            std::path::absolute(&opts.repo)
                .unwrap_or_else(|_| opts.repo.clone())
                .display()
        ));
    }

    ui.step("Read what the packages install");
    let mut packaged = BTreeSet::new();
    for deb in &opts.debs {
        packaged.extend(crate::check::deb_contents(deb, ui, &opts.repo)?);
    }
    let programs = crate::check::installed_programs(&packaged);
    if programs.is_empty() {
        return Err("no programs in /usr/bin or /usr/sbin to test".to_string());
    }

    ui.step("Ask each program which way it answers to being asked for help");
    let root = crate::check::extract_all(&opts.debs, ui)?;
    let probed: Vec<Probed> = programs
        .iter()
        .map(|program| Probed {
            program: program.clone(),
            verb: probe(root.path(), program, ui),
        })
        .collect();

    ui.step("Write debian/tests");
    match write_tests(&debian, &probed, ui)? {
        true => Ok(probed),
        // Nothing was written, so there is nothing to report about
        // which verb each program took.
        false => Ok(Vec::new()),
    }
}

/// The first verb `program` answers to, if any.
fn probe(root: &Path, program: &str, ui: &crate::ui::Ui) -> Option<String> {
    let path = root.join(program.trim_start_matches('/'));
    if !path.is_file() {
        return None;
    }
    HELP_VERBS
        .iter()
        .find(|verb| {
            let argv: Vec<String> = ["timeout", "10", &path.display().to_string(), verb]
                .iter()
                .map(|s| s.to_string())
                .collect();
            matches!(ui.run_capture(&argv, root), Ok((0, _)))
        })
        .map(|verb| verb.to_string())
}

/// Write `debian/tests/{control,smoke}`, keeping debcargo's stanzas.
///
/// Returns `false` when there was already a test and nothing was
/// written, which the caller reports rather than claiming success.
fn write_tests(debian: &Path, probed: &[Probed], ui: &crate::ui::Ui) -> Result<bool, String> {
    let tests = debian.join("tests");
    let existing = std::fs::read_to_string(tests.join("control")).ok();
    let keep = match plan(existing.as_deref(), tests.join("smoke").is_file()) {
        Plan::Stop(why) => {
            println!("nothing written: {why}");
            return Ok(false);
        }
        Plan::AddSmoke(control) => Some(control),
        // Only now is debcargo's hint worth reading: there is no
        // control file of its own to preserve.
        Plan::Fresh => std::fs::read_to_string(tests.join("control.debcargo.hint")).ok(),
    };
    let control = tests_control(keep.as_deref());
    let script = smoke_script(probed);
    if ui.dry_run {
        println!(
            "would write {}:\n{control}",
            tests.join("control").display()
        );
        println!("would write {}:\n{script}", tests.join("smoke").display());
        return Ok(true);
    }
    std::fs::create_dir_all(&tests).map_err(|e| format!("{}: {e}", tests.display()))?;
    let control_path = tests.join("control");
    std::fs::write(&control_path, control)
        .map_err(|e| format!("{}: {e}", control_path.display()))?;
    let smoke_path = tests.join("smoke");
    std::fs::write(&smoke_path, script).map_err(|e| format!("{}: {e}", smoke_path.display()))?;
    make_executable(&smoke_path)?;
    println!(
        "wrote {} and {}",
        control_path.display(),
        smoke_path.display()
    );
    Ok(true)
}

/// Make the smoke script executable, which autopkgtest requires.
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probed(program: &str, verb: Option<&str>) -> Probed {
        Probed {
            program: program.to_string(),
            verb: verb.map(str::to_string),
        }
    }

    #[test]
    fn the_script_runs_each_program_the_way_it_answered() {
        let script = smoke_script(&[
            probed("/usr/bin/dbranch", Some("--help")),
            probed("/usr/bin/damo", Some("help")),
        ]);
        assert_eq!(
            script,
            "#!/bin/sh\nset -eu\n\n/usr/bin/dbranch --help\n/usr/bin/damo help\n"
        );
    }

    #[test]
    fn a_program_that_answered_to_nothing_is_left_visible_not_dropped() {
        // Writing nothing would produce a test that passes by testing
        // nothing, which is the state being fixed.
        let script = smoke_script(&[probed("/usr/sbin/daemon", None)]);
        assert!(script.contains("# /usr/sbin/daemon answered to none of"));
        assert!(script.contains("--help, help, -h"));
        assert!(
            !script.contains("\n/usr/sbin/daemon "),
            "it must not be invoked with a verb it rejected"
        );
    }

    #[test]
    fn the_control_depends_on_the_packages_this_source_builds() {
        let control = tests_control(None);
        assert_eq!(
            control,
            "Tests: smoke\nDepends: @\nRestrictions: allow-stderr\n"
        );
    }

    #[test]
    fn debcargos_own_stanzas_are_kept_below_ours() {
        // threecpio's shape: a hand-written CLI test first, then the
        // generated cargo-test stanzas, so both keep running.
        let hint = "Test-Command: /usr/share/cargo/bin/cargo-auto-test dbranch 0.26.0\n\
                    Features: test-name=rust-dbranch:@\n";
        let control = tests_control(Some(hint));
        assert!(control.starts_with("Tests: smoke\n"));
        assert!(control.contains("cargo-auto-test dbranch 0.26.0"));
        // A blank line between stanzas, as deb822 requires.
        assert!(control.contains("allow-stderr\n\nTest-Command:"));
    }

    #[test]
    fn an_empty_hint_adds_no_trailing_stanza() {
        assert_eq!(tests_control(Some("   \n")), tests_control(None));
    }

    #[test]
    fn a_package_that_already_declares_a_test_is_left_alone() {
        // damo's and threecpio's shape: a named script a person
        // wrote. Regenerating would overwrite work more specific
        // than anything probing can produce.
        let control = "Tests: cli\nDepends: 3cpio\nRestrictions: allow-stderr\n";
        let Plan::Stop(why) = plan(Some(control), true) else {
            panic!("an existing test must stop this");
        };
        assert!(why.contains("already declares a test"), "{why}");
    }

    #[test]
    fn debcargos_generated_stanzas_do_not_count_as_a_test_of_the_program() {
        // They run the crate's cargo tests, never the installed
        // binary, so the gap this command fills is still open.
        let generated = "Test-Command: /usr/share/cargo/bin/cargo-auto-test dbranch 0.26.0\n\
                         Features: test-name=rust-dbranch:@\n";
        let Plan::AddSmoke(kept) = plan(Some(generated), false) else {
            panic!("generated stanzas are kept, not treated as a test");
        };
        assert_eq!(kept, generated, "they are preserved verbatim");
    }

    #[test]
    fn an_empty_debian_tests_is_written_fresh() {
        assert_eq!(plan(None, false), Plan::Fresh);
    }

    #[test]
    fn a_stray_smoke_script_is_never_overwritten() {
        // No control names it, so nothing runs it -- but it is still
        // somebody's file, and silently replacing it is exactly the
        // kind of damage this command must not do.
        let Plan::Stop(why) = plan(None, true) else {
            panic!("a script with no control entry must stop this");
        };
        assert!(why.contains("rather than letting"), "{why}");
    }

    fn silent() -> crate::ui::Ui {
        crate::ui::Ui {
            explain: false,
            dry_run: false,
            quiet: true,
        }
    }

    #[test]
    fn writing_produces_a_runnable_script_and_a_control_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let debian = dir.path().join("debian");
        std::fs::create_dir_all(&debian).unwrap();
        let wrote = write_tests(
            &debian,
            &[probed("/usr/bin/foo", Some("--help"))],
            &silent(),
        )
        .expect("a fresh tree is written");
        assert!(wrote);
        let control = std::fs::read_to_string(debian.join("tests/control")).unwrap();
        assert_eq!(
            control,
            "Tests: smoke\nDepends: @\nRestrictions: allow-stderr\n"
        );
        let script = std::fs::read_to_string(debian.join("tests/smoke")).unwrap();
        assert!(script.contains("/usr/bin/foo --help"));
        // autopkgtest runs the script, so it has to be executable.
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(debian.join("tests/smoke"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755, "the script must be executable");
    }

    #[test]
    fn writing_keeps_debcargos_stanzas_from_the_hint() {
        let dir = tempfile::tempdir().unwrap();
        let tests = dir.path().join("debian/tests");
        std::fs::create_dir_all(&tests).unwrap();
        std::fs::write(
            tests.join("control.debcargo.hint"),
            "Test-Command: cargo-auto-test dbranch 0.26.0\n",
        )
        .unwrap();
        write_tests(
            &dir.path().join("debian"),
            &[probed("/usr/bin/dbranch", Some("--help"))],
            &silent(),
        )
        .unwrap();
        let control = std::fs::read_to_string(tests.join("control")).unwrap();
        assert!(control.starts_with("Tests: smoke\n"));
        assert!(control.contains("cargo-auto-test dbranch 0.26.0"));
    }

    #[test]
    fn writing_refuses_to_touch_a_test_somebody_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let tests = dir.path().join("debian/tests");
        std::fs::create_dir_all(&tests).unwrap();
        let theirs = "Tests: cli\nDepends: @\n";
        std::fs::write(tests.join("control"), theirs).unwrap();
        std::fs::write(tests.join("cli"), "#!/bin/sh\nreal test\n").unwrap();
        let wrote = write_tests(
            &dir.path().join("debian"),
            &[probed("/usr/bin/foo", Some("--help"))],
            &silent(),
        )
        .unwrap();
        assert!(!wrote, "an existing test is not overwritten");
        assert_eq!(
            std::fs::read_to_string(tests.join("control")).unwrap(),
            theirs,
            "their control file is left exactly as it was"
        );
        assert!(!tests.join("smoke").exists(), "and no script appears");
    }

    #[test]
    fn the_verbs_are_tried_commonest_first() {
        // -h last on purpose: on some programs it is not help at all.
        assert_eq!(HELP_VERBS, ["--help", "help", "-h"]);
    }
}
