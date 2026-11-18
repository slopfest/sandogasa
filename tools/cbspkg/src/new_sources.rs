// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `new-sources`: write a package's `sources` file and `.gitignore` for
//! new source files, then upload the files to the SIG lookaside (#171),
//! as `fedpkg new-sources` does for Fedora.
//!
//! The `sources` file lists each file with its sha512 sum, which is what
//! CBS fetches the file from the lookaside by; the files themselves are
//! not committed, so `.gitignore` names each one that no pattern in it
//! covers yet. The two are staged, not committed. `--offline` stops
//! there, for someone who cannot upload: the person who merges the
//! change uploads the files.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use sandogasa_cbs::lookaside::Source;
use sandogasa_cli::explain::Ui;

use crate::add_pkg::resolve;
use crate::upload_sources::{git, lookaside_lacks, parse_sha512sum, upload};

#[derive(clap::Args)]
pub struct NewSourcesArgs {
    /// The source files, replacing the ones `sources` lists; a file
    /// outside the checkout is copied into it
    #[arg(required = true, value_name = "FILE")]
    pub files: Vec<PathBuf>,

    /// Show each step and its command without changing anything
    #[arg(long)]
    pub dry_run: bool,

    /// Narrate each step and show its command, pausing before each
    #[arg(long)]
    pub explain: bool,

    /// Write `sources` and `.gitignore` only, uploading nothing
    #[arg(long)]
    pub offline: bool,

    /// The package's dist-git checkout (default: the current
    /// directory)
    #[arg(short = 'C', long, value_name = "DIR")]
    pub repo: Option<PathBuf>,
}

/// One file given to `new-sources`: where it is, the name it has in
/// the checkout, and whether it has to be copied there.
#[derive(Debug, PartialEq)]
pub struct NewFile {
    pub path: PathBuf,
    pub name: String,
    pub copy: bool,
}

/// The `sources` file listing `files`, each with its sum.
pub fn render_sources(files: &[Source]) -> String {
    files
        .iter()
        .map(|s| format!("{} ({}) = {}\n", s.hash.to_ascii_uppercase(), s.name, s.sum))
        .collect()
}

/// `gitignore` with a `/name` line added for each of `names`, the file
/// ending in a newline before them.
pub fn with_ignored(gitignore: &str, names: &[String]) -> String {
    let mut out = gitignore.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    for name in names {
        out.push_str(&format!("/{name}\n"));
    }
    out
}

pub fn run(args: &NewSourcesArgs) -> ExitCode {
    match run_inner(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The sha512 sum of each of `paths`, from one `sha512sum` run, in
/// order.
fn sums(paths: &[&Path]) -> Result<Vec<String>, String> {
    let out = Command::new("sha512sum")
        .args(paths)
        .output()
        .map_err(|e| format!("cannot run sha512sum: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "sha512sum: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let sums: Vec<String> = text.lines().filter_map(parse_sha512sum).collect();
    if sums.len() != paths.len() {
        return Err(format!(
            "sha512sum printed {} sums for {} files",
            sums.len(),
            paths.len()
        ));
    }
    Ok(sums)
}

/// Where each of `files` is and its name in `repo`, refusing a name
/// given twice, a path that is not a file, and a name `sha512sum`
/// would escape (a backslash or a newline in it). A file already at the
/// top of `repo` is not copied; one anywhere else, a subdirectory of
/// the checkout included, is.
fn locate(repo: &Path, files: &[PathBuf]) -> Result<Vec<NewFile>, String> {
    let repo_dir = repo
        .canonicalize()
        .map_err(|e| format!("{}: {e}", repo.display()))?;
    let mut located: Vec<NewFile> = Vec::new();
    for path in files {
        if !path.is_file() {
            return Err(format!("{} is not a file", path.display()));
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| !n.contains(['\\', '\n']))
            .ok_or_else(|| format!("{} has no usable file name", path.display()))?
            .to_string();
        if located.iter().any(|f| f.name == name) {
            return Err(format!("{name} is given twice"));
        }
        let full = path
            .canonicalize()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let copy = full.parent() != Some(repo_dir.as_path());
        located.push(NewFile {
            path: path.clone(),
            name,
            copy,
        });
    }
    Ok(located)
}

/// Refuse names that are the checkout's own files rather than sources:
/// `sources` and `.gitignore`, a spec, or a file git tracks (a patch,
/// say), as `cbspkg new-sources *` would give.
fn refuse_own_files(repo: &Path, names: &[String]) -> Result<(), String> {
    if let Some(n) = names
        .iter()
        .find(|n| *n == "sources" || *n == ".gitignore" || n.ends_with(".spec"))
    {
        return Err(format!(
            "{n} is part of the package's repo, not a source file"
        ));
    }
    let mut args = vec!["ls-files", "--"];
    args.extend(names.iter().map(String::as_str));
    let tracked = git(repo, &args)?;
    if let Some(n) = tracked.lines().next() {
        return Err(format!(
            "{n} is tracked in the package's repo, so it is not a source file"
        ));
    }
    Ok(())
}

/// Which of `names` no pattern in `repo`'s ignore files covers, from one
/// `git check-ignore`.
fn not_ignored(repo: &Path, names: &[String]) -> Result<Vec<String>, String> {
    // --no-index: whether a pattern matches, even for a file tracked by
    // mistake. With -z the names go in on stdin and the ignored ones come
    // back as they are, NUL-separated. Exit 0 is some ignored, 1 none.
    use std::io::Write;
    let mut child = Command::new("git")
        .args(["check-ignore", "-z", "--stdin", "--no-index"])
        .current_dir(repo)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run git: {e}"))?;
    let input: String = names.iter().map(|n| format!("{n}\0")).collect();
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(input.as_bytes())
        .map_err(|e| format!("git check-ignore: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("git check-ignore: {e}"))?;
    if !matches!(out.status.code(), Some(0 | 1)) {
        return Err(format!(
            "git check-ignore: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let listed = String::from_utf8_lossy(&out.stdout);
    let ignored: Vec<&str> = listed.split('\0').collect();
    Ok(names
        .iter()
        .filter(|n| !ignored.contains(&n.as_str()))
        .cloned()
        .collect())
}

/// `text`, each line indented, as a dry run shows what it would write.
fn indent(text: &str) -> String {
    text.lines().map(|l| format!("    {l}\n")).collect()
}

fn run_inner(args: &NewSourcesArgs) -> Result<(), Box<dyn std::error::Error>> {
    let ui = Ui {
        explain: args.explain,
        dry_run: args.dry_run,
        quiet: false,
    };
    let repo = args.repo.clone().unwrap_or_else(|| PathBuf::from("."));
    sandogasa_cli::require_tools(&[
        ("git", "sudo dnf install git-core", Some("--version")),
        ("sha512sum", "sudo dnf install coreutils", Some("--version")),
    ])?;
    let checkout = resolve(&repo, None, None, None)?;
    let package = &checkout.package;
    let files = locate(&repo, &args.files)?;
    let names: Vec<String> = files.iter().map(|f| f.name.clone()).collect();
    refuse_own_files(&repo, &names)?;

    // A file of the same name already at the top of the checkout, not
    // the one given, would be overwritten by the copy: it is summed in
    // the same run, and only one with the same contents is taken as the
    // same file.
    let there: Vec<PathBuf> = files
        .iter()
        .filter(|f| f.copy && repo.join(&f.name).exists())
        .map(|f| repo.join(&f.name))
        .collect();
    let paths: Vec<&Path> = files
        .iter()
        .map(|f| f.path.as_path())
        .chain(there.iter().map(PathBuf::as_path))
        .collect();
    ui.step("Compute the files' sha512 sums");
    let mut shown = vec!["sha512sum".to_string()];
    shown.extend(files.iter().map(|f| f.path.display().to_string()));
    ui.show_command(&shown);
    let mut all_sums = sums(&paths)?;
    let there_sums = all_sums.split_off(files.len());
    let new_sums = all_sums;
    let mut there_sums = there_sums.into_iter();
    let mut copies = Vec::new();
    for (file, sum) in files.iter().zip(&new_sums) {
        if !file.copy {
            continue;
        }
        if repo.join(&file.name).exists() {
            if there_sums.next().as_ref() != Some(sum) {
                return Err(format!(
                    "the checkout has a different {}: remove it, or give that one",
                    file.name
                )
                .into());
            }
        } else {
            copies.push(file);
        }
    }

    for file in &copies {
        println!(
            "{} is not at the top of the checkout: copying it there as {}",
            file.path.display(),
            file.name
        );
        let from = std::path::absolute(&file.path)?;
        ui.step(&format!("Copy {} into the checkout", file.name));
        ui.run_required(
            &[
                "cp".to_string(),
                from.display().to_string(),
                file.name.clone(),
            ],
            &repo,
        )?;
    }

    let sources: Vec<Source> = files
        .iter()
        .zip(new_sums)
        .map(|(f, sum)| Source {
            name: f.name.clone(),
            hash: "sha512".to_string(),
            sum,
        })
        .collect();
    let text = render_sources(&sources);
    ui.step("Write the sums to sources, replacing the files it lists");
    let mut tagged = vec!["sha512sum".to_string(), "--tag".to_string()];
    tagged.extend(names.iter().cloned());
    ui.show_command(&tagged);
    if ui.dry_run {
        print!("{}", indent(&text));
    } else {
        std::fs::write(repo.join("sources"), &text)?;
    }

    let to_ignore = not_ignored(&repo, &names)?;
    let mut staged = vec!["sources".to_string()];
    if !to_ignore.is_empty() {
        ui.step("Ignore the files .gitignore does not cover yet");
        let path = repo.join(".gitignore");
        let before = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("{}: {e}", path.display()).into()),
        };
        if ui.dry_run {
            let added: Vec<String> = to_ignore.iter().map(|n| format!("/{n}")).collect();
            print!("{}", indent(&added.join("\n")));
        } else {
            std::fs::write(&path, with_ignored(&before, &to_ignore))?;
        }
        staged.push(".gitignore".to_string());
    }

    ui.step("Stage them");
    let mut add = vec!["git".to_string(), "add".to_string(), "--".to_string()];
    add.extend(staged.iter().cloned());
    ui.run_required(&add, &repo)?;
    // Against the index: a file the step created has no other diff.
    let staged_refs: Vec<&str> = staged.iter().map(String::as_str).collect();
    ui.explain_diff_cached(&repo, &staged_refs);
    if !ui.dry_run {
        println!(
            "Staged {} for {}; commit it with the spec change.",
            staged.join(" and "),
            names.join(", ")
        );
    }

    if args.offline {
        println!("--offline: nothing uploaded; whoever merges the change uploads the files.");
        return Ok(());
    }
    let missing = lookaside_lacks(&ui, package, &sources)?;
    if missing.is_empty() {
        println!("{package}'s sources are in the SIG lookaside already.");
        return Ok(());
    }
    upload(&ui, &repo, package, &missing)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::{EnvGuard, Fixture};

    const BRANCH: &str = "c10s-proposed-RHEL-1";
    const FILE: &str = "rsync-3.2.7.tar.gz";
    /// `printf 'data\n' | sha512sum`
    const SUM: &str = "73651d654c5ba73dd4b687f9dbbbdfc00884bf3dc1674e4cdcf762ff31778b29\
                       11e61e02b2a97c4055523eed2c4e6051b9902b0f91b4ca5b95e4c53cdf940b3d";

    fn args(f: &Fixture, files: &[PathBuf], dry_run: bool, offline: bool) -> NewSourcesArgs {
        NewSourcesArgs {
            files: files.to_vec(),
            dry_run,
            explain: false,
            offline,
            repo: Some(f.repo.clone()),
        }
    }

    fn read(f: &Fixture, name: &str) -> String {
        std::fs::read_to_string(f.repo.join(name)).unwrap_or_default()
    }

    /// The staged names, from `git diff --cached --name-only`.
    fn staged(f: &Fixture) -> String {
        f.git(&["diff", "--cached", "--name-only"])
    }

    #[test]
    fn a_sources_file_lists_each_file_with_its_sum() {
        let sources = [
            Source {
                name: "a.tar.gz".into(),
                hash: "sha512".into(),
                sum: "1".into(),
            },
            Source {
                name: "b.tar.gz".into(),
                hash: "sha512".into(),
                sum: "2".into(),
            },
        ];
        let text = render_sources(&sources);
        assert_eq!(text, "SHA512 (a.tar.gz) = 1\nSHA512 (b.tar.gz) = 2\n");
        // The form upload-sources reads.
        assert_eq!(sandogasa_cbs::lookaside::parse_sources(&text), sources);
    }

    #[test]
    fn ignored_files_are_added_after_a_newline() {
        let names = ["a.tar.gz".to_string()];
        assert_eq!(with_ignored("", &names), "/a.tar.gz\n");
        assert_eq!(
            with_ignored("/old.tar.gz", &names),
            "/old.tar.gz\n/a.tar.gz\n"
        );
        assert_eq!(
            with_ignored("/old.tar.gz\n", &names),
            "/old.tar.gz\n/a.tar.gz\n"
        );
    }

    #[test]
    #[serial_test::serial]
    fn offline_writes_sources_and_gitignore_and_stages_them() {
        let f = Fixture::proposed(BRANCH, &[("sources", "SHA512 (rsync-3.2.6.tar.gz) = 0\n")]);
        std::fs::write(f.repo.join(FILE), "data\n").unwrap();
        run_inner(&args(&f, &[f.repo.join(FILE)], false, true)).unwrap();
        // The old file is replaced, as fedpkg new-sources does.
        assert_eq!(read(&f, "sources"), format!("SHA512 ({FILE}) = {SUM}\n"));
        assert_eq!(read(&f, ".gitignore"), format!("/{FILE}\n"));
        assert_eq!(staged(&f), ".gitignore\nsources");
    }

    #[test]
    #[serial_test::serial]
    fn a_file_a_glob_already_ignores_is_not_added() {
        let f = Fixture::proposed(BRANCH, &[(".gitignore", "/*.tar.gz\n")]);
        std::fs::write(f.repo.join(FILE), "data\n").unwrap();
        run_inner(&args(&f, &[f.repo.join(FILE)], false, true)).unwrap();
        assert_eq!(read(&f, ".gitignore"), "/*.tar.gz\n");
        assert_eq!(staged(&f), "sources");
    }

    #[test]
    #[serial_test::serial]
    fn a_file_outside_the_checkout_is_copied_in() {
        let f = Fixture::proposed(BRANCH, &[]);
        let outside = f.tmp.path().join(FILE);
        std::fs::write(&outside, "data\n").unwrap();
        run_inner(&args(&f, std::slice::from_ref(&outside), false, true)).unwrap();
        assert_eq!(read(&f, FILE), "data\n");
        assert_eq!(read(&f, "sources"), format!("SHA512 ({FILE}) = {SUM}\n"));
        // Copying a file the checkout has the same contents for is not
        // needed; a different one is refused.
        run_inner(&args(&f, std::slice::from_ref(&outside), false, true)).unwrap();
        std::fs::write(&outside, "other\n").unwrap();
        let err = run_inner(&args(&f, &[outside], false, true))
            .unwrap_err()
            .to_string();
        assert!(err.contains("has a different"), "{err}");
    }

    #[test]
    #[serial_test::serial]
    fn a_dry_run_changes_nothing_but_asks_the_lookaside() {
        let f = Fixture::proposed(BRANCH, &[]);
        let outside = f.tmp.path().join(FILE);
        std::fs::write(&outside, "data\n").unwrap();
        let mut server = mockito::Server::new();
        let mock = server
            .mock(
                "HEAD",
                format!("/sources/rsync/{FILE}/sha512/{SUM}/{FILE}").as_str(),
            )
            .with_status(404)
            .expect(1)
            .create();
        let _env = EnvGuard::new(&[("CBSPKG_LOOKASIDE_BASE", &server.url())]);
        run_inner(&args(&f, &[outside], true, false)).unwrap();
        mock.assert();
        assert!(!f.repo.join(FILE).exists());
        assert!(!f.repo.join("sources").exists());
        assert!(!f.repo.join(".gitignore").exists());
        assert_eq!(staged(&f), "");
    }

    #[test]
    #[serial_test::serial]
    fn sources_the_lookaside_has_are_not_uploaded() {
        let f = Fixture::proposed(BRANCH, &[]);
        std::fs::write(f.repo.join(FILE), "data\n").unwrap();
        let mut server = mockito::Server::new();
        let mock = server
            .mock(
                "HEAD",
                format!("/sources/rsync/{FILE}/sha512/{SUM}/{FILE}").as_str(),
            )
            .with_status(200)
            .expect(1)
            .create();
        let _env = EnvGuard::new(&[("CBSPKG_LOOKASIDE_BASE", &server.url())]);
        run_inner(&args(&f, &[f.repo.join(FILE)], false, false)).unwrap();
        mock.assert();
        assert_eq!(
            run(&args(&f, &[f.repo.join(FILE)], false, false)),
            ExitCode::SUCCESS
        );
    }

    #[test]
    #[serial_test::serial]
    fn a_name_given_twice_or_a_missing_file_is_refused() {
        let f = Fixture::proposed(BRANCH, &[]);
        std::fs::write(f.repo.join(FILE), "data\n").unwrap();
        let other = f.tmp.path().join(FILE);
        std::fs::write(&other, "data\n").unwrap();
        let err = run_inner(&args(&f, &[f.repo.join(FILE), other], false, true))
            .unwrap_err()
            .to_string();
        assert!(err.contains("given twice"), "{err}");
        let gone = f.repo.join("gone.tar.gz");
        let err = run_inner(&args(&f, std::slice::from_ref(&gone), false, true))
            .unwrap_err()
            .to_string();
        assert!(err.contains("is not a file"), "{err}");
        assert_eq!(run(&args(&f, &[gone], false, true)), ExitCode::FAILURE);
    }

    #[test]
    #[serial_test::serial]
    fn only_the_files_no_pattern_covers_are_ignored() {
        let f = Fixture::proposed(BRANCH, &[(".gitignore", "/*.tar.gz")]);
        std::fs::write(f.repo.join(FILE), "data\n").unwrap();
        std::fs::write(f.repo.join("extra.zip"), "zip\n").unwrap();
        run_inner(&args(
            &f,
            &[f.repo.join(FILE), f.repo.join("extra.zip")],
            false,
            true,
        ))
        .unwrap();
        // The missing final newline is added before the new line.
        assert_eq!(read(&f, ".gitignore"), "/*.tar.gz\n/extra.zip\n");
    }

    #[test]
    #[serial_test::serial]
    fn a_gitignore_that_cannot_be_read_is_not_overwritten() {
        let f = Fixture::proposed(BRANCH, &[]);
        std::fs::write(f.repo.join(".gitignore"), b"# Latin-1 \xe9\n/old.tar.gz\n").unwrap();
        std::fs::write(f.repo.join(FILE), "data\n").unwrap();
        let err = run_inner(&args(&f, &[f.repo.join(FILE)], false, true))
            .unwrap_err()
            .to_string();
        assert!(err.contains(".gitignore"), "{err}");
        assert_eq!(
            std::fs::read(f.repo.join(".gitignore")).unwrap(),
            b"# Latin-1 \xe9\n/old.tar.gz\n"
        );
    }

    #[test]
    #[serial_test::serial]
    fn a_file_in_a_subdirectory_is_copied_to_the_top() {
        let f = Fixture::proposed(BRANCH, &[]);
        std::fs::create_dir(f.repo.join("SOURCES")).unwrap();
        let below = f.repo.join("SOURCES").join(FILE);
        std::fs::write(&below, "data\n").unwrap();
        run_inner(&args(&f, &[below], false, true)).unwrap();
        assert_eq!(read(&f, FILE), "data\n");
    }

    #[test]
    #[serial_test::serial]
    fn the_packages_own_files_are_not_sources() {
        let f = Fixture::proposed(BRANCH, &[("fix.patch", "x\n"), ("sources", "")]);
        for name in ["sources", "rsync.spec", "fix.patch"] {
            let err = run_inner(&args(&f, &[f.repo.join(name)], false, true))
                .unwrap_err()
                .to_string();
            assert!(err.contains("not a source file"), "{name}: {err}");
        }
        let odd = f.repo.join("a\\b.tar.gz");
        std::fs::write(&odd, "x\n").unwrap();
        let err = run_inner(&args(&f, &[odd], false, true))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no usable file name"), "{err}");
    }
}
