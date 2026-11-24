// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Before a build: whether the spec's version-release is the one it
//! means to build, and whether the build will sort above stock's (#152).
//!
//! blktrace's c10s Proposed Update shows both going wrong at once. Its
//! merge request added a `%changelog` entry for `1.3.0-14` but left
//! `Release: 13`, so `1.3.0-13~proposed.el10` was built, sorted below
//! stock's `1.3.0-13.el10`, and was never installed. Neither is visible
//! once the build is done, so both are checked before it starts.
//!
//! The spec is read with `rpmspec` from a worktree of the commit being
//! built, with each collection's macros from CBS, after rpmautospec has
//! filled in `%autorelease` and `%autochangelog` from the worktree's
//! history, as CBS does. Stock's build comes from `fedrq`, epoch
//! included. Whatever cannot be read is a warning and leaves that
//! comparison out. The one error is a missing rpmautospec for a spec
//! that uses it: a tool the spec needs, named with its install hint,
//! as `build` does for its other tools.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use sandogasa_cbs::Collection;
use sandogasa_cli::explain::Ui;

use crate::local::{
    build_config_argv, build_configs, host_macros, legacy_patch_macros, readable_spec,
    rpmspec_argv, rpmspec_query, tag_macros,
};

/// The query format giving `epoch:version-release` (epoch `0` when the
/// spec sets none), then the newest `%changelog` entry's name line
/// (`(none)` without one). The `\n` is rpm's to expand, so the narrated
/// command stays on one line.
const EVR_QF: &str = r"%|epoch?{%{epoch}}:{0}|:%{version}-%{release}\n%{changelogname}\n";

/// The query format giving `version-release`.
const VR_QF: &str = r"%{version}-%{release}\n";

/// The version-release a `%changelog` entry's name line is for, its
/// epoch dropped: the line's last word, after the ` - ` that usually
/// precedes it (`A Packager <a@example.com> - 1:1.3.0-14`).
pub fn changelog_vr(name_line: &str) -> Option<String> {
    let evr = name_line.split_whitespace().last()?;
    let vr = evr.split_once(':').map_or(evr, |(_, vr)| vr);
    // A header without one ends in the address, `<jane-doe@example.com>`.
    if !vr.contains('-') || !vr.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    Some(vr.to_string())
}

/// What is wrong with the spec's version-release, when its newest
/// `%changelog` entry, `name_line`, names another: the release is
/// `bare` without the dist tag and `with_dist` with it, and an entry
/// may be written either way.
pub fn changelog_mismatch(
    package: &str,
    bare: &str,
    with_dist: &str,
    name_line: &str,
) -> Option<String> {
    let entry = changelog_vr(name_line)?;
    (entry != bare && entry != with_dist).then(|| {
        format!(
            "{package}.spec builds {bare} (Version and Release), but its newest \
             %changelog entry is for {entry}: bump Release, or fix the entry, and push again"
        )
    })
}

/// Whether stock's `stock_evr` sorts at or above the SIG's `sig_evr`
/// (both `epoch:version-release`), so that dnf, with both repositories
/// enabled, keeps stock's.
pub fn not_above_stock(sig_evr: &str, stock_evr: &str) -> bool {
    sandogasa_rpmvercmp::compare_evr(stock_evr, sig_evr) != Ordering::Less
}

/// The `fedrq` command reading stock `branch`'s source build of
/// `package`.
pub fn fedrq_argv(branch: &str, package: &str) -> Vec<String> {
    [
        "fedrq",
        "pkgs",
        "-b",
        branch,
        "--src",
        "-F",
        "line:name,epoch,version,release",
        package,
    ]
    .map(String::from)
    .to_vec()
}

/// Stock's highest `epoch:version-release` of `package`, from
/// `fedrq_argv`'s output (`name : epoch : version : release` lines);
/// `None` when stock has no such package.
pub fn stock_evr(output: &str, package: &str) -> Option<String> {
    output
        .lines()
        .filter_map(|line| {
            let cols: Vec<&str> = line.split(" : ").map(str::trim).collect();
            let [name, epoch, version, release] = cols[..] else {
                return None;
            };
            (name == package).then(|| format!("{epoch}:{version}-{release}"))
        })
        .max_by(|a, b| sandogasa_rpmvercmp::compare_evr(a, b))
}

/// Check the spec `commit` builds before building it: against its own
/// `%changelog`, and against stock CentOS Stream's build for each
/// Stream collection in `collections`. Problems are listed; for a real
/// build they are confirmed at a terminal, defaulting to not building,
/// while a dry run or a scratch build only warns, and without a terminal
/// (`interactive` false) they are an error. `Ok(false)` means do not
/// build.
pub fn check(
    ui: &Ui,
    repo: &Path,
    package: &str,
    commit: &str,
    collections: &[Collection],
    scratch: bool,
    interactive: bool,
) -> Result<bool, Box<dyn std::error::Error>> {
    ui.step(&format!(
        "Check {package}.spec's Release against its %changelog and stock CentOS Stream"
    ));
    // Everything read, and the worktree gone, before anything is asked.
    let problems = problems(ui, repo, package, commit, collections)?;
    if problems.is_empty() {
        return Ok(true);
    }
    for p in &problems {
        eprintln!("warning: {p}");
    }
    if ui.dry_run || scratch {
        return Ok(true);
    }
    if !interactive {
        return Err("building in CBS is confirmed at a terminal; run it in one".into());
    }
    if ui.confirm_default_no("Build anyway?") {
        Ok(true)
    } else {
        println!("Nothing built.");
        Ok(false)
    }
}

/// A detached worktree of a commit, removed when dropped.
struct Worktree {
    repo: PathBuf,
    path: PathBuf,
}

impl Worktree {
    fn add(ui: &Ui, repo: &Path, commit: &str, path: &Path) -> Result<Worktree, String> {
        let at = path.display().to_string();
        let args = ["worktree", "add", "--quiet", "--detach", &at, commit];
        ui.show_command(
            &std::iter::once("git")
                .chain(args)
                .map(String::from)
                .collect::<Vec<_>>(),
        );
        crate::build::git(repo, &args)?;
        Ok(Worktree {
            repo: repo.to_path_buf(),
            path: path.to_path_buf(),
        })
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        let at = self.path.display().to_string();
        if let Err(e) = crate::build::git(&self.repo, &["worktree", "remove", "--force", &at]) {
            eprintln!(
                "warning: could not remove the worktree {at} ({e}); `git worktree prune` will"
            );
        }
    }
}

/// The problems with the spec `commit` builds, for each collection.
/// What cannot be read is a warning, and that comparison is left out;
/// only a missing rpmautospec, for a spec that needs it, is an error.
fn problems(
    ui: &Ui,
    repo: &Path,
    package: &str,
    commit: &str,
    collections: &[Collection],
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let skip = |what: String| {
        eprintln!("warning: {what}; Release not checked");
        Ok(Vec::new())
    };
    let dir = tempfile::tempdir()?;
    let tree = match Worktree::add(ui, repo, commit, &dir.path().join("tree")) {
        Ok(t) => t,
        Err(e) => return skip(format!("cannot check out {commit} ({e})")),
    };
    let spec = tree.path.join(format!("{package}.spec"));
    if !spec.is_file() {
        return skip(format!("{package}.spec is not in the commit to build"));
    }
    let text = std::fs::read_to_string(&spec)?;
    // Checked here, not left to readable_spec, so a missing tool is an
    // error naming it rather than one more warning.
    if crate::local::uses_rpmautospec(&text) {
        sandogasa_cli::require_tools(&[(
            "rpmautospec",
            "sudo dnf install rpmautospec",
            Some("--help"),
        )])?;
    }
    let spec = match readable_spec(ui, &spec, &dir.path().join("processed")) {
        Ok(s) => s.display().to_string(),
        Err(e) => return skip(e),
    };
    let (configs, stock) = read_cbs_and_stock(ui, package, collections);
    let configs = match configs {
        Ok(c) => c,
        Err(e) => return skip(format!("cannot read the macros CBS builds with ({e})")),
    };
    let sourcedir = ("_sourcedir".to_string(), tree.path.display().to_string());
    let mut problems = Vec::new();
    let mut changelog_checked = false;
    for (collection, config) in collections.iter().zip(&configs) {
        let mut macros = host_macros(&tag_macros(config), collection.major);
        macros.push(sourcedir.clone());
        macros.extend(legacy_patch_macros(&text));
        let target = collection.target();
        let out = match rpmspec_query(ui, &tree.path, &rpmspec_argv(&spec, &macros, EVR_QF)) {
            Ok(out) => out,
            Err(e) => {
                eprintln!("warning: {e}; {target} not checked");
                continue;
            }
        };
        let mut lines = out.lines();
        let evr = lines.next().unwrap_or_default().trim().to_string();
        let name_line = lines.next().unwrap_or_default();
        // The changelog is the same for every collection: compared
        // once, with the collection's macros but no dist tag, and with.
        if !changelog_checked {
            changelog_checked = true;
            let mut bare_macros: Vec<(String, String)> = macros
                .iter()
                .filter(|(k, _)| k != "dist")
                .cloned()
                .collect();
            bare_macros.push(("dist".to_string(), "%{nil}".to_string()));
            match rpmspec_query(ui, &tree.path, &rpmspec_argv(&spec, &bare_macros, VR_QF)) {
                Ok(bare) => {
                    let with_dist = evr.split_once(':').map_or(evr.as_str(), |(_, vr)| vr);
                    problems.extend(changelog_mismatch(
                        package,
                        bare.lines().next().unwrap_or_default().trim(),
                        with_dist,
                        name_line,
                    ));
                }
                Err(e) => eprintln!("warning: {e}; not compared with its %changelog"),
            }
        }
        if !collection.stream {
            continue;
        }
        let branch = stream_branch(collection);
        match stock.get(&branch) {
            Some(Ok(Some(stock_evr))) if not_above_stock(&evr, stock_evr) => {
                problems.push(format!(
                    "stock {branch} has {package} {stock_evr}, which sorts at or above the \
                     build for {target} ({evr}), so dnf keeps installing stock's: bump \
                     Release past stock's and push again"
                ));
            }
            Some(Ok(Some(_))) => {}
            Some(Ok(None)) => println!("Stock {branch} has no {package}: not compared with stock."),
            Some(Err(e)) => eprintln!(
                "warning: cannot read stock {branch}'s {package} ({e}); not compared with stock"
            ),
            None => eprintln!("warning: stock {branch} was not read; not compared with stock"),
        }
    }
    Ok(problems)
}

/// The stock CentOS Stream branch a Stream collection builds on.
fn stream_branch(collection: &Collection) -> String {
    format!("c{}s", collection.major)
}

type Stock = HashMap<String, Result<Option<String>, String>>;

/// The macros CBS builds each collection with, and stock's build of
/// `package` for each Stream release among them, read side by side.
fn read_cbs_and_stock(
    ui: &Ui,
    package: &str,
    collections: &[Collection],
) -> (Result<Vec<serde_json::Value>, String>, Stock) {
    let tags: Vec<String> = collections.iter().map(|c| c.build_tag()).collect();
    let mut branches: Vec<String> = collections
        .iter()
        .filter(|c| c.stream)
        .map(stream_branch)
        .collect();
    branches.sort();
    branches.dedup();
    for tag in &tags {
        ui.show_command(&build_config_argv(tag));
    }
    for branch in &branches {
        ui.show_command(&fedrq_argv(branch, package));
    }
    if !ui.explain && !ui.dry_run {
        eprintln!("Reading CBS's build settings and stock CentOS Stream's {package}…");
    }
    std::thread::scope(|s| {
        let configs = s.spawn(|| build_configs(&tags).map_err(|e| e.to_string()));
        let queries: Vec<_> = branches
            .iter()
            .map(|b| (b, s.spawn(|| fedrq(b, package))))
            .collect();
        let stock = queries
            .into_iter()
            .map(|(b, h)| {
                let r = h.join().unwrap_or_else(|_| Err("fedrq panicked".into()));
                (b.clone(), r)
            })
            .collect();
        let configs = configs
            .join()
            .unwrap_or_else(|_| Err("koji panicked".into()));
        (configs, stock)
    })
}

/// Stock `branch`'s `epoch:version-release` of `package`.
fn fedrq(branch: &str, package: &str) -> Result<Option<String>, String> {
    let argv = fedrq_argv(branch, package);
    let out = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("cannot run fedrq: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(stock_evr(&String::from_utf8_lossy(&out.stdout), package))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::uses_rpmautospec;

    #[test]
    fn the_changelog_entry_names_the_version_release() {
        let line = "Michel Lind <salimma@centosproject.org> - 1.3.0-14";
        assert_eq!(changelog_vr(line).as_deref(), Some("1.3.0-14"));
        assert_eq!(
            changelog_vr("A <a@b> - 1:2.1-4.1").as_deref(),
            Some("2.1-4.1")
        );
        assert_eq!(changelog_vr("A <a@b> 1.0-2").as_deref(), Some("1.0-2"));
        assert_eq!(changelog_vr("(none)"), None);
        assert_eq!(changelog_vr("A <a@b>"), None);
        // A header giving no version ends in the address.
        assert_eq!(changelog_vr("Jane Doe <jane-doe@example.com>"), None);
        // blktrace's MR !5.
        let problem =
            changelog_mismatch("blktrace", "1.3.0-13", "1.3.0-13~proposed.el10", line).unwrap();
        assert!(
            problem.contains("builds 1.3.0-13") && problem.contains("is for 1.3.0-14"),
            "{problem}"
        );
        assert_eq!(
            changelog_mismatch("blktrace", "1.3.0-14", "1.3.0-14.el10", line),
            None
        );
        // An entry naming the release with its dist tag, wherever the
        // tag sits in it.
        let with_dist = "A <a@b> - 1.3.0-13.el10.1";
        assert_eq!(
            changelog_mismatch("p", "1.3.0-13.1", "1.3.0-13.el10.1", with_dist),
            None
        );
        assert!(
            changelog_mismatch("p", "1.3.0-1", "1.3.0-1.el10", "A <a@b> - 1.3.0-1.1").is_some()
        );
    }

    #[test]
    fn rpmautospec_is_found_in_every_spelling_outside_comments() {
        for spec in [
            "Release: %autorelease\n",
            "Release: %{autorelease}\n",
            "Release: %{autorelease -b 2}\n",
            "Release: %{?autorelease}\n",
            "%changelog\n%autochangelog\n",
        ] {
            assert!(uses_rpmautospec(spec), "{spec}");
        }
        for spec in [
            "Release: 3%{?dist}\n",
            "# TODO: switch to %autorelease\nRelease: 3\n",
            "%global x %autorelease_foo\n",
        ] {
            assert!(!uses_rpmautospec(spec), "{spec}");
        }
    }

    #[test]
    fn a_proposed_update_needs_a_release_past_stocks() {
        // The ~ sorts the SIG's build below stock's of the same release.
        assert!(not_above_stock(
            "0:1.3.0-13~proposed.el10",
            "0:1.3.0-13.el10"
        ));
        assert!(!not_above_stock(
            "0:1.3.0-14~proposed.el10",
            "0:1.3.0-13.el10"
        ));
        // Stock moved on past the SIG's build.
        assert!(not_above_stock(
            "0:1.3.0-14~proposed.el10",
            "0:1.3.0-15.el10"
        ));
        // Hyperscale's .hs sorts above stock's .el of the same release.
        assert!(!not_above_stock("0:1.3.0-13.hs.el10", "0:1.3.0-13.el10"));
        // An epoch outranks the rest.
        assert!(!not_above_stock("1:1.0-1~proposed.el10", "0:1.3.0-5.el10"));
    }

    #[test]
    fn stocks_highest_build_is_read_from_fedrq() {
        let out =
            "rsync : 0 : 3.4.1 : 1.el10\nrsync : 0 : 3.4.1 : 3.el10\nrsync : 0 : 3.4.1 : 2.el10\n";
        assert_eq!(stock_evr(out, "rsync").as_deref(), Some("0:3.4.1-3.el10"));
        assert_eq!(stock_evr(out, "blktrace"), None);
        assert_eq!(stock_evr("", "rsync"), None);
    }

    use crate::test_support::Fixture;

    /// rpmspec answering for rsync: it builds 3.4.1-`release`, its
    /// changelog is for -`changelog`.
    fn fake_rpmspec(f: &Fixture, release: &str, changelog: &str) {
        let bare = format!("3.4.1-{release}");
        let built = format!(
            "0:3.4.1-{release}~proposed.el10\nA Packager <a@example.com> - 3.4.1-{changelog}"
        );
        f.fake(
            "rpmspec",
            &[
                ("--version", ""),
                ("dist %{nil}", &bare),
                ("dist ~proposed.el10", &built),
            ],
        );
    }

    /// rsync's spec on the SIG branch, committed and pushed, with CBS,
    /// rpmspec and fedrq answering: the spec builds 3.4.1-`release`, its
    /// changelog is for -`changelog`, stock c10s has -`stock`.
    fn rsync(release: &str, changelog: &str, stock: &str) -> (Fixture, String) {
        let f = Fixture::proposed("c10s-proposed-RHEL-1", &[("rsync.spec", "Name: rsync\n")]);
        f.fake(
            "koji",
            &[
                ("--help", ""),
                (
                    "getBuildConfig",
                    r#"{"extra": {"rpm.macro.distprefix": "~proposed"}}"#,
                ),
            ],
        );
        fake_rpmspec(&f, release, changelog);
        let stock = format!("rsync : 0 : 3.4.1 : {stock}.el10");
        f.fake("fedrq", &[("--help", ""), ("pkgs", &stock)]);
        let commit = f.git(&["rev-parse", "HEAD"]);
        (f, commit)
    }

    fn ui(dry_run: bool) -> Ui {
        Ui {
            explain: false,
            dry_run,
            quiet: false,
        }
    }

    fn collections(f: &Fixture) -> Vec<Collection> {
        crate::add_pkg::resolve(&f.repo, None, None, None)
            .unwrap()
            .collections
    }

    fn checked(f: &Fixture, commit: &str, dry_run: bool, scratch: bool) -> Result<bool, String> {
        check(
            &ui(dry_run),
            &f.repo,
            "rsync",
            commit,
            &collections(f),
            scratch,
            false,
        )
        .map_err(|e| e.to_string())
    }

    #[test]
    #[serial_test::serial]
    fn a_release_past_stock_and_matching_its_changelog_builds() {
        let (f, commit) = rsync("3", "3", "2");
        assert_eq!(checked(&f, &commit, false, false), Ok(true));
        // The worktree is gone afterwards.
        assert!(!f.git(&["worktree", "list"]).contains("tree"));
    }

    #[test]
    #[serial_test::serial]
    fn a_release_lagging_its_changelog_or_stock_is_confirmed() {
        // blktrace's case: the changelog says -3, Release says 2, and
        // stock is at 2. Without a terminal, that is an error.
        let (f, commit) = rsync("2", "3", "2");
        let err = checked(&f, &commit, false, false).unwrap_err();
        assert!(err.contains("confirmed at a terminal"), "{err}");
        // A dry run or a scratch build only warns.
        assert_eq!(checked(&f, &commit, true, false), Ok(true));
        assert_eq!(checked(&f, &commit, false, true), Ok(true));
        // The changelog alone is enough to stop it.
        let (f, commit) = rsync("2", "3", "1");
        assert!(checked(&f, &commit, false, false).is_err());
        // And stock alone.
        let (f, commit) = rsync("3", "3", "3");
        assert!(checked(&f, &commit, false, false).is_err());
    }

    #[test]
    #[serial_test::serial]
    fn what_cannot_be_read_is_not_compared() {
        // A Release lagging both, but nothing to compare it with.
        let (f, commit) = rsync("2", "3", "2");
        f.fake("fedrq", &[("--help", ""), ("pkgs", "!no network")]);
        f.fake(
            "rpmspec",
            &[("--version", ""), ("--srpm", "!error: bad spec")],
        );
        assert_eq!(checked(&f, &commit, false, false), Ok(true));
        // CBS unreadable.
        fake_rpmspec(&f, "2", "3");
        f.fake(
            "koji",
            &[("--help", ""), ("getBuildConfig", "!No such tagInfo")],
        );
        assert_eq!(checked(&f, &commit, false, false), Ok(true));
        // Stock without the package: only the changelog, which agrees.
        let (f, commit) = rsync("3", "3", "2");
        f.fake("fedrq", &[("--help", ""), ("pkgs", "")]);
        fake_rpmspec(&f, "3", "3");
        assert_eq!(checked(&f, &commit, false, false), Ok(true));
        // A commit git cannot check out.
        assert_eq!(checked(&f, "0123456789abcdef", false, false), Ok(true));
        // A commit without the spec.
        f.git(&["rm", "--quiet", "rsync.spec"]);
        f.git(&["commit", "--quiet", "-m", "gone"]);
        let head = f.git(&["rev-parse", "HEAD"]);
        assert_eq!(checked(&f, &head, false, false), Ok(true));
    }

    #[test]
    #[serial_test::serial]
    fn an_autorelease_spec_is_checked_as_rpmautospec_fills_it_in() {
        let (f, _) = rsync("3", "3", "2");
        std::fs::write(
            f.repo.join("rsync.spec"),
            "Release: %{autorelease}\n%changelog\n%autochangelog\n",
        )
        .unwrap();
        f.git(&["commit", "--quiet", "-am", "auto"]);
        let head = f.git(&["rev-parse", "HEAD"]);
        // Without rpmautospec: an error naming it.
        let err = checked(&f, &head, false, false).unwrap_err();
        assert!(
            err.contains("missing required tool(s): rpmautospec"),
            "{err}"
        );
        // process-distgit writes the filled-in copy, which is what
        // rpmspec is asked about.
        crate::test_support::install_script(
            &f.bin,
            "rpmautospec",
            "[ \"$1\" = --help ] && exit 0\n\
             [ \"$1\" = process-distgit ] && sed 's/%{autorelease}/3/' \"$2\" > \"$3\" && exit 0\n\
             exit 1\n",
        );
        f.fake(
            "rpmspec",
            &[
                ("--version", ""),
                ("tree/rsync.spec", "!read the spec as committed"),
                ("dist %{nil}", "3.4.1-3"),
                (
                    "dist ~proposed.el10",
                    "0:3.4.1-3~proposed.el10\nA <a@b> - 3.4.1-3",
                ),
            ],
        );
        assert_eq!(checked(&f, &head, false, false), Ok(true));
        // A process-distgit that fails is a warning, not an error.
        crate::test_support::install_script(
            &f.bin,
            "rpmautospec",
            "[ \"$1\" = --help ] && exit 0\necho 'bad history' >&2\nexit 1\n",
        );
        assert_eq!(checked(&f, &head, false, false), Ok(true));
    }

    #[test]
    #[serial_test::serial]
    fn a_spec_with_obsolete_patch_lines_is_read_as_cbs_reads_it() {
        // rsync's spec with Stream's `%patch1`: rpmspec from rpm 4.20 on
        // refuses it unless `%patch1` is defined as the new spelling.
        let (f, _) = rsync("3", "3", "2");
        f.git(&["checkout", "--quiet", "c10s-proposed-RHEL-1"]);
        std::fs::write(
            f.repo.join("rsync.spec"),
            "Name: rsync\n%prep\n%patch1 -p1\n",
        )
        .unwrap();
        f.git(&["commit", "--quiet", "-am", "patch"]);
        let commit = f.git(&["rev-parse", "HEAD"]);
        f.fake(
            "rpmspec",
            &[
                ("--version", ""),
                // The define is the last before the bare query's dist,
                // and the last before the spec in the built one.
                ("patch1 %%patch -P 1 --define dist %{nil}", "3.4.1-3"),
                (
                    "patch1 %%patch -P 1 /",
                    "0:3.4.1-3~proposed.el10\nA Packager <a@example.com> - 3.4.1-3",
                ),
                ("--srpm", "!error: %patchN is obsolete, use %patch N"),
            ],
        );
        assert_eq!(checked(&f, &commit, false, false), Ok(true));
        // Without the define, the Release check would have been skipped
        // with a warning and passed all the same; the lagging Release
        // shows the spec was read.
        std::fs::write(
            f.repo.join("rsync.spec"),
            "Name: rsync\n%prep\n%patch1 -p1\n# again\n",
        )
        .unwrap();
        f.git(&["commit", "--quiet", "-am", "again"]);
        let commit = f.git(&["rev-parse", "HEAD"]);
        f.fake(
            "rpmspec",
            &[
                ("--version", ""),
                ("patch1 %%patch -P 1 --define dist %{nil}", "3.4.1-2"),
                (
                    "patch1 %%patch -P 1 /",
                    "0:3.4.1-2~proposed.el10\nA Packager <a@example.com> - 3.4.1-3",
                ),
                ("--srpm", "!error: %patchN is obsolete, use %patch N"),
            ],
        );
        assert!(checked(&f, &commit, false, false).is_err());
    }
}
