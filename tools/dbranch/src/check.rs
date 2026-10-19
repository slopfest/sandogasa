// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Check packaging files rather than trusting them.
//!
//! `debian/` is often adopted rather than written: from upstream's
//! own tree, from a previous release, from a generator's overlay. It
//! then goes stale quietly -- a changelog naming a version that is no
//! longer the one being packaged, a patch the new upstream already
//! applied, a file that exists in the source and reaches no package.
//! None of that stops a build, so a build is not what finds it.
//!
//! The checks here are the ones nothing else runs. Lintian already
//! reads a finished package well; what it does not do is compare the
//! packaging against the *source it came from*, or run the program
//! afterwards. Three questions, in the order they bite:
//!
//! 1. Does the packaging still describe this source? (stale metadata)
//! 2. Does everything the source ships reach a package? (lost files)
//! 3. Does the installed program actually run? (smoke)
//!
//! Each finding carries the command a person could run to see it for
//! themselves, so a report teaches the check as well as reporting it.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

/// How much a finding matters.
///
/// `Blocker` is reserved for what a sponsor would refuse over: a
/// program that does not run, a package describing a different
/// source. Everything advisory is a `Warning`, and `Note` is for
/// what is probably fine but worth a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Blocker,
    Warning,
    Note,
}

impl Severity {
    /// The label a report prints.
    pub fn label(self) -> &'static str {
        match self {
            Severity::Blocker => "blocker",
            Severity::Warning => "warning",
            Severity::Note => "note",
        }
    }
}

/// One thing worth saying about a package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// Stable slug for the check that produced this, so a report can
    /// be filtered without matching on prose.
    pub check: String,
    pub severity: Severity,
    pub message: String,
    /// A real command that shows the same thing, where one exists.
    /// dbranch is a learning tool: a finding a person cannot verify
    /// for themselves is a finding they have to take on trust.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

impl Finding {
    fn new(check: &str, severity: Severity, message: String) -> Self {
        Self {
            check: check.to_string(),
            severity,
            message,
            command: None,
        }
    }

    fn with_command(mut self, command: String) -> Self {
        self.command = Some(command);
        self
    }
}

/// The upstream part of a Debian version: no epoch, no revision.
///
/// `1:0.26.0-1` is upstream `0.26.0`. The revision is taken from the
/// *last* hyphen because an upstream version may contain its own
/// (`1.0-rc1-1`), while an epoch is everything before the first `:`.
pub fn upstream_version(version: &str) -> &str {
    let without_epoch = version.split_once(':').map_or(version, |(_, v)| v);
    match without_epoch.rsplit_once('-') {
        Some((upstream, _)) => upstream,
        None => without_epoch,
    }
}

/// A dotted version as comparable numbers, non-numeric parts dropped.
///
/// Deliberately crude: it exists to answer "is this mention older
/// than what we package", not to implement Debian's comparison
/// algorithm. `0.25.4` sorts below `0.26.0`; anything it cannot read
/// as digits compares as zero, so a version it does not understand is
/// never reported as stale.
fn version_key(version: &str) -> Vec<u64> {
    version
        .split('.')
        .map(|part| {
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().unwrap_or(0)
        })
        .collect()
}

/// Whether `mentioned` names a release older than `current`.
fn is_older(mentioned: &str, current: &str) -> bool {
    !mentioned.is_empty() && !current.is_empty() && version_key(mentioned) < version_key(current)
}

/// Does the packaging still describe this source?
///
/// `source_version` is the version the *source* says it is, when that
/// can be established (a crate's `Cargo.toml`, an unpacked tarball's
/// directory name). `None` means it could not be established, which
/// is reported rather than passed over: a check that silently skips
/// is indistinguishable from one that passed.
pub fn version_findings(
    changelog_version: &str,
    distribution: &str,
    source_version: Option<&str>,
) -> Vec<Finding> {
    let mut found = Vec::new();
    let packaged = upstream_version(changelog_version);
    match source_version {
        Some(source) if source != packaged => found.push(
            Finding::new(
                "stale-changelog-version",
                Severity::Blocker,
                format!(
                    "debian/changelog is for {packaged}, but the source is \
                     {source} — the packaging was adopted from another \
                     release and never caught up"
                ),
            )
            .with_command("head -1 debian/changelog".into()),
        ),
        Some(_) => {}
        None => found.push(Finding::new(
            "source-version-unknown",
            Severity::Note,
            "could not establish the source's own version, so the \
             changelog version was not checked against it"
                .into(),
        )),
    }
    // debcargo writes a sentinel distribution that must not reach an
    // upload; a plain UNRELEASED is the normal state while preparing.
    if distribution.contains("FIXME") {
        found.push(
            Finding::new(
                "changelog-distribution-placeholder",
                Severity::Warning,
                format!("debian/changelog still has the generated placeholder {distribution}"),
            )
            .with_command("head -1 debian/changelog".into()),
        );
    }
    found
}

/// Do `debian/patches/series` and the directory agree?
///
/// Both directions are worth knowing. A listed patch that is missing
/// breaks the build, so it surfaces quickly; an *unlisted* patch file
/// does not, and is how a patch that was dropped stays in the tree
/// looking as if it is still carried.
pub fn series_findings(series: &str, present: &BTreeSet<String>) -> Vec<Finding> {
    let listed: Vec<&str> = series
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        // A series line may carry options, e.g. `foo.patch -p0`.
        .map(|l| l.split_whitespace().next().unwrap_or(l))
        .collect();
    let mut found = Vec::new();
    for patch in &listed {
        if !present.contains(*patch) {
            found.push(
                Finding::new(
                    "patch-missing",
                    Severity::Blocker,
                    format!("debian/patches/series lists {patch}, which is not there"),
                )
                .with_command("cat debian/patches/series".into()),
            );
        }
    }
    for patch in present {
        if !listed.contains(&patch.as_str()) {
            found.push(
                Finding::new(
                    "patch-unlisted",
                    Severity::Warning,
                    format!(
                        "debian/patches/{patch} is not in series — a dropped \
                         patch left behind looks like one still carried"
                    ),
                )
                .with_command("cat debian/patches/series".into()),
            );
        }
    }
    found
}

/// Patches that no longer apply to this source.
///
/// `failures` pairs a patch name with why it failed. The usual cause
/// is the happiest one: upstream took the fix, and the patch is now
/// describing work already done.
pub fn apply_findings(failures: &[(String, String)]) -> Vec<Finding> {
    failures
        .iter()
        .map(|(patch, reason)| {
            Finding::new(
                "patch-does-not-apply",
                Severity::Blocker,
                format!(
                    "debian/patches/{patch} no longer applies ({reason}) — \
                     upstream may already carry it"
                ),
            )
            .with_command(format!("patch -p1 --dry-run -f -i debian/patches/{patch}"))
        })
        .collect()
}

/// Comments naming a release older than the one being packaged.
///
/// `files` maps a path to its text. Only comment lines are read, and
/// only a version strictly older than `current` is reported, so a
/// file that mentions the current release (or a bug number, or a
/// dependency's version) stays quiet. This is a `Note`: prose going
/// stale is untidy rather than wrong, but it is reliably the first
/// sign that the rest of the file has too.
pub fn stale_mention_findings(files: &BTreeMap<String, String>, current: &str) -> Vec<Finding> {
    let mut found = Vec::new();
    for (path, text) in files {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for line in text.lines() {
            let Some(comment) = line.trim().strip_prefix('#') else {
                continue;
            };
            for token in comment.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
                let token = token.trim_matches('.');
                // Two dots or more: a release, not a bare number.
                if token.matches('.').count() >= 2 && is_older(token, current) {
                    seen.insert(token.to_string());
                }
            }
        }
        for version in seen {
            found.push(
                Finding::new(
                    "stale-version-mention",
                    Severity::Note,
                    format!("{path} comments on {version}, older than the packaged {current}"),
                )
                .with_command(format!("grep -n {version} {path}")),
            );
        }
    }
    found
}

/// Whether anything runs the programs this package installs.
///
/// A generated suite is not nothing, so "no autopkgtest" would often
/// be false. What is true is narrower and more useful: none of them
/// runs the installed program. autodep8's Python generator emits one
/// `import` and marks it `superficial`, Debian's own word for a test
/// that proves little; its Ruby generator runs the gem's own suite,
/// which is the library's tests and is `skippable` when there are
/// none; debcargo's stanzas run cargo tests against the crate. All
/// three leave /usr/bin untouched.
pub fn autopkgtest_findings(control: Option<&str>, testsuite: Option<&str>) -> Vec<Finding> {
    if control.is_some_and(crate::autopkgtest::declares_test) {
        return Vec::new();
    }
    let message = match testsuite {
        Some(suite) => format!(
            "{suite} exercises this package's library, not the programs \
             it installs in /usr/bin"
        ),
        None => "debian/tests/control names no test, so nothing runs the \
                 programs this package installs"
            .to_string(),
    };
    vec![
        Finding::new("no-program-test", Severity::Warning, message)
            .with_command("dbranch autopkgtest --deb ../*.deb".into()),
    ]
}

/// The `Testsuite:` a `debian/control` declares, if any.
pub fn declared_testsuite(control: &str) -> Option<String> {
    control.lines().find_map(|line| {
        line.strip_prefix("Testsuite:")
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    })
}

/// A file the source ships that a package ought to carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceAsset {
    /// Path within the source tree, for the report.
    pub path: String,
    /// The name to look for among the package's contents.
    pub basename: String,
    /// What kind of thing it is, for the message.
    pub kind: &'static str,
}

/// Assets the source ships that no package installs.
///
/// This is the check that catches the quietest packaging bug there
/// is: a file that exists, builds, and is simply never copied in. The
/// program is then missing a man page, or -- when the file is one it
/// loads at runtime -- fails the moment someone runs it.
///
/// Matching is on basename, allowing the compression a package
/// applies (`dbranch.1` is installed as `dbranch.1.gz`), because the
/// install path is the packager's choice and the name is not.
pub fn uninstalled_assets(assets: &[SourceAsset], packaged: &BTreeSet<String>) -> Vec<Finding> {
    let names: BTreeSet<&str> = packaged
        .iter()
        .filter_map(|p| p.rsplit('/').next())
        .map(|n| n.strip_suffix(".gz").unwrap_or(n))
        .collect();
    assets
        .iter()
        .filter(|a| !names.contains(a.basename.as_str()))
        .map(|a| {
            Finding::new(
                "shipped-but-not-installed",
                Severity::Warning,
                format!(
                    "{} is in the source but reaches no package — the {} \
                     will be missing once installed",
                    a.path, a.kind
                ),
            )
            .with_command("dpkg-deb -c ../*.deb".into())
        })
        .collect()
}

/// How running an installed program went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Smoke {
    /// It ran and exited successfully.
    Ok,
    /// It ran and failed, with whatever it said.
    Failed { code: i32, output: String },
    /// It could not be run at all.
    NotRun(String),
}

/// Programs the package installs that do not run.
///
/// The one check that would have caught the failure this module was
/// written for: a `/usr/bin` entry whose supporting file was never
/// installed, so it dies on `--help` with a module-not-found. Nothing
/// in a build catches that, because the build is not what runs it.
pub fn smoke_findings(results: &[(String, Smoke)]) -> Vec<Finding> {
    results
        .iter()
        .filter_map(|(binary, outcome)| match outcome {
            Smoke::Ok => None,
            Smoke::Failed { code, output } => Some(
                Finding::new(
                    "binary-does-not-run",
                    Severity::Blocker,
                    format!(
                        "{binary} --help exited {code}: {}",
                        first_line(output.trim())
                    ),
                )
                .with_command(format!("{binary} --help")),
            ),
            Smoke::NotRun(why) => Some(Finding::new(
                "binary-not-run",
                Severity::Note,
                format!("{binary} was not run: {why}"),
            )),
        })
        .collect()
}

/// Whether a set of findings should fail the run.
pub fn has_blocker(findings: &[Finding]) -> bool {
    findings.iter().any(|f| f.severity == Severity::Blocker)
}

/// What to check and where.
pub struct CheckOptions {
    /// The package working directory (the one holding `debian/`).
    pub repo: std::path::PathBuf,
    /// Built packages to inspect. Empty means the source-tree checks
    /// only -- which need no build and so can run anywhere.
    pub debs: Vec<std::path::PathBuf>,
    /// Emit the findings as JSON instead of prose.
    pub json: bool,
}

/// Run the checks and report. Returns the findings.
///
/// The source-tree checks always run; the package checks only when
/// `debs` names something, because they are the ones that need a
/// build. Nothing here writes, so it is safe to run on a tree you are
/// in the middle of.
pub fn run(opts: &CheckOptions, ui: &crate::ui::Ui) -> Result<Vec<Finding>, String> {
    let mut findings = Vec::new();
    let debian = opts.repo.join("debian");
    if !debian.is_dir() {
        return Err(format!("{} has no debian/ directory", opts.repo.display()));
    }

    ui.step("Read the changelog and the source's own version");
    let changelog = read(&debian.join("changelog"))?;
    let header = changelog
        .lines()
        .find_map(crate::changelog::parse_header)
        .ok_or("debian/changelog has no parsable stanza header")?;
    let source = source_version(&opts.repo);
    findings.extend(version_findings(
        &header.version,
        &header.distribution,
        source.as_deref(),
    ));

    ui.step("Compare debian/patches against the series file");
    let series_path = debian.join("patches/series");
    if series_path.is_file() {
        let series = read(&series_path)?;
        let present = patch_files(&debian.join("patches"));
        findings.extend(series_findings(&series, &present));
        // An unpacked quilt source has its patches already applied,
        // so a dry-run apply there tests nothing and fails anyway.
        if opts.repo.join(".pc/applied-patches").is_file() {
            findings.push(Finding::new(
                "patches-already-applied",
                Severity::Note,
                "the patches are applied in this tree (.pc exists), so \
                 whether they still apply was not tested"
                    .into(),
            ));
        } else {
            findings.extend(apply_findings(&failing_patches(&series, ui, &opts.repo)));
        }
    }

    ui.step("Check that something runs the installed programs");
    findings.extend(autopkgtest_findings(
        std::fs::read_to_string(debian.join("tests/control"))
            .ok()
            .as_deref(),
        std::fs::read_to_string(debian.join("control"))
            .ok()
            .as_deref()
            .and_then(declared_testsuite)
            .as_deref(),
    ));

    ui.step("Look for comments left behind by an older release");
    findings.extend(stale_mention_findings(
        &debian_texts(&debian),
        upstream_version(&header.version),
    ));

    if opts.debs.is_empty() {
        findings.push(Finding::new(
            "packages-not-checked",
            Severity::Note,
            "no built package given, so what the source ships and what \
             it installs were not compared — pass --deb to check that"
                .into(),
        ));
    } else {
        ui.step("Compare what the source ships against what the packages install");
        let mut packaged = BTreeSet::new();
        for deb in &opts.debs {
            packaged.extend(deb_contents(deb, ui, &opts.repo)?);
        }
        findings.extend(uninstalled_assets(&collect_assets(&opts.repo), &packaged));

        ui.step("Run each installed program, which is what a build never does");
        findings.extend(smoke_findings(&smoke_test(&opts.debs, &packaged, ui)?));
    }

    report(&findings, opts.json);
    Ok(findings)
}

/// Read a file, naming it when that fails.
fn read(path: &std::path::Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The version the source says it is, from whatever states it.
///
/// A crate's `Cargo.toml` first, then the unpacked directory's own
/// `<name>-<version>` name. `None` when neither answers, which the
/// caller reports rather than treating as agreement.
fn source_version(repo: &std::path::Path) -> Option<String> {
    if let Ok(text) = std::fs::read_to_string(repo.join("Cargo.toml"))
        && let Some(v) = crate_version(&text)
    {
        return Some(v);
    }
    repo.file_name()
        .and_then(|n| n.to_str())
        .and_then(dir_version)
}

/// The `version` of a `Cargo.toml`'s `[package]` table.
///
/// Hand-read rather than parsed: only one key of one table is wanted,
/// and a TOML dependency for that would be the larger change. Stops
/// at the next table header so a dependency's version is never read
/// as the crate's, and ignores a workspace-inherited version, which
/// states nothing on its own.
fn crate_version(text: &str) -> Option<String> {
    let mut in_package = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(rest) = line.strip_prefix("version") {
            let rest = rest.trim_start().strip_prefix('=')?.trim();
            if let Some(v) = rest.strip_prefix('"').and_then(|r| r.split('"').next()) {
                return Some(v.to_string());
            }
            // `version.workspace = true` and friends say nothing here.
            return None;
        }
    }
    None
}

/// The version in an unpacked source directory's `<name>-<version>`.
fn dir_version(name: &str) -> Option<String> {
    let (_, version) = name.rsplit_once('-')?;
    version
        .starts_with(|c: char| c.is_ascii_digit())
        .then(|| version.to_string())
}

/// The patch files actually in `debian/patches`, series aside.
fn patch_files(dir: &std::path::Path) -> BTreeSet<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return BTreeSet::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            (name != "series" && e.path().is_file()).then_some(name)
        })
        .collect()
}

/// Which of the series' patches no longer apply, and why.
fn failing_patches(
    series: &str,
    ui: &crate::ui::Ui,
    repo: &std::path::Path,
) -> Vec<(String, String)> {
    let mut failed = Vec::new();
    for line in series.lines() {
        let patch = line.trim();
        if patch.is_empty() || patch.starts_with('#') {
            continue;
        }
        let patch = patch.split_whitespace().next().unwrap_or(patch);
        let argv: Vec<String> = [
            "patch",
            "-p1",
            "--dry-run",
            "-f",
            "-s",
            "-i",
            &format!("debian/patches/{patch}"),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        match ui.run_capture(&argv, repo) {
            Ok((0, _)) => {}
            Ok((_, out)) => failed.push((patch.to_string(), first_line(out.trim()).to_string())),
            // patch(1) missing is not the package's fault; say so
            // rather than reporting every patch as broken.
            Err(e) => failed.push((patch.to_string(), format!("could not run patch: {e}"))),
        }
    }
    failed
}

/// Every readable file directly in `debian/`, for the prose scan.
fn debian_texts(debian: &std::path::Path) -> BTreeMap<String, String> {
    let Ok(entries) = std::fs::read_dir(debian) else {
        return BTreeMap::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| {
            let text = std::fs::read_to_string(e.path()).ok()?;
            Some((format!("debian/{}", e.file_name().to_str()?), text))
        })
        .collect()
}

/// What kind of asset a path is, if it is one worth installing.
fn classify_asset(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.ends_with(".desktop") {
        return Some("desktop entry");
    }
    if name.ends_with(".metainfo.xml") || name.ends_with(".appdata.xml") {
        return Some("AppStream metadata");
    }
    if name.ends_with(".service") || name.ends_with(".timer") || name.ends_with(".socket") {
        return Some("systemd unit");
    }
    // A man page is `<name>.<section>`, section 1-9 with an optional
    // language suffix left off here.
    if let Some((_, section)) = name.rsplit_once('.')
        && section.len() == 1
        && section.starts_with(|c: char| ('1'..='9').contains(&c))
    {
        return Some("man page");
    }
    None
}

/// Assets the source tree ships, skipping what is not shipped source.
fn collect_assets(repo: &std::path::Path) -> Vec<SourceAsset> {
    let mut found = Vec::new();
    walk(repo, repo, &mut found);
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

/// Recurse, skipping the directories that hold no installable source.
fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<SourceAsset>) {
    const SKIP: [&str; 6] = [".git", "debian", "target", ".pc", "node_modules", ".github"];
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if path.is_dir() {
            if !SKIP.contains(&name.as_str()) {
                walk(root, &path, out);
            }
            continue;
        }
        let Some(kind) = classify_asset(&name) else {
            continue;
        };
        let rel = path.strip_prefix(root).unwrap_or(&path);
        out.push(SourceAsset {
            path: rel.display().to_string(),
            basename: name,
            kind,
        });
    }
}

/// The paths a .deb installs, as `dpkg-deb -c` lists them.
pub(crate) fn deb_contents(
    deb: &std::path::Path,
    ui: &crate::ui::Ui,
    cwd: &std::path::Path,
) -> Result<BTreeSet<String>, String> {
    let argv: Vec<String> = ["dpkg-deb", "-c", &deb.display().to_string()]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let (code, out) = ui
        .run_capture(&argv, cwd)
        .map_err(|e| format!("dpkg-deb -c {}: {e}", deb.display()))?;
    if code != 0 {
        return Err(format!("dpkg-deb -c {} failed: {out}", deb.display()));
    }
    Ok(out.lines().filter_map(deb_entry_path).collect())
}

/// The installed path on one `dpkg-deb -c` line, or `None` when the
/// line names no file.
///
/// Two shapes have to be read rather than assumed. A symlink line
/// ends `./usr/bin/x -> target`, so the last field is the *target*
/// and not the path; and a directory line ends in `/`, which is not
/// something that can be installed or run.
fn deb_entry_path(line: &str) -> Option<String> {
    let path = line.split(" -> ").next().unwrap_or(line);
    let path = path.split_whitespace().last()?;
    (!path.ends_with('/')).then(|| path.to_string())
}

/// Extract each package and run the programs it installs.
///
/// Running from an extraction rather than an installation is the
/// point: a program that needs a file at an absolute path only finds
/// it if that file was actually packaged, which is the bug being
/// looked for.
/// The programs a package installs, as absolute paths.
pub(crate) fn installed_programs(packaged: &BTreeSet<String>) -> Vec<String> {
    packaged
        .iter()
        .map(|p| p.trim_start_matches('.'))
        .filter(|p| p.starts_with("/usr/bin/") || p.starts_with("/usr/sbin/"))
        .map(str::to_string)
        .collect()
}

/// Unpack every package into one temporary root.
///
/// One root for all of them, because a program in one package may
/// need a file from another, which is exactly the relationship a
/// smoke test is looking for.
pub(crate) fn extract_all(
    debs: &[std::path::PathBuf],
    ui: &crate::ui::Ui,
) -> Result<tempfile::TempDir, String> {
    let root = tempfile::tempdir().map_err(|e| format!("could not make a temp dir: {e}"))?;
    for deb in debs {
        let argv: Vec<String> = [
            "dpkg-deb",
            "-x",
            &deb.display().to_string(),
            &root.path().display().to_string(),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let (code, out) = ui
            .run_capture(&argv, root.path())
            .map_err(|e| format!("dpkg-deb -x {}: {e}", deb.display()))?;
        if code != 0 {
            return Err(format!("dpkg-deb -x {} failed: {out}", deb.display()));
        }
    }
    Ok(root)
}

fn smoke_test(
    debs: &[std::path::PathBuf],
    packaged: &BTreeSet<String>,
    ui: &crate::ui::Ui,
) -> Result<Vec<(String, Smoke)>, String> {
    let programs = installed_programs(packaged);
    if programs.is_empty() {
        return Ok(Vec::new());
    }
    let root = extract_all(debs, ui)?;
    let mut results = Vec::new();
    for program in programs {
        let path = root.path().join(program.trim_start_matches('/'));
        if !path.is_file() {
            results.push((program, Smoke::NotRun("not in the extracted tree".into())));
            continue;
        }
        // `timeout` so a program that waits for input cannot hang the
        // check, and because it keeps this a real, runnable command.
        let argv: Vec<String> = ["timeout", "10", &path.display().to_string(), "--help"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        match ui.run_capture(&argv, root.path()) {
            Ok((0, _)) => results.push((program, Smoke::Ok)),
            Ok((code, output)) => results.push((program, Smoke::Failed { code, output })),
            Err(e) => results.push((program, Smoke::NotRun(e.to_string()))),
        }
    }
    Ok(results)
}

/// The findings as a person reads them: worst first, each followed by
/// the command that shows the same thing.
pub fn render(findings: &[Finding]) -> String {
    if findings.is_empty() {
        return "nothing to report\n".to_string();
    }
    let mut sorted: Vec<&Finding> = findings.iter().collect();
    sorted.sort_by_key(|f| f.severity);
    let mut out = String::new();
    for finding in sorted {
        out.push_str(&format!(
            "{}: {}\n",
            finding.severity.label(),
            finding.message
        ));
        if let Some(command) = &finding.command {
            out.push_str(&format!("    {command}\n"));
        }
    }
    out
}

/// Print the findings, as JSON or as prose.
fn report(findings: &[Finding], json: bool) {
    if json {
        match serde_json::to_string_pretty(findings) {
            Ok(text) => println!("{text}"),
            Err(e) => eprintln!("could not render JSON: {e}"),
        }
        return;
    }
    print!("{}", render(findings));
}

/// The first line of a tool's output, for a one-line message.
fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("").trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(findings: &[Finding]) -> Vec<&str> {
        findings.iter().map(|f| f.check.as_str()).collect()
    }

    #[test]
    fn an_upstream_version_drops_the_epoch_and_the_revision() {
        assert_eq!(upstream_version("0.26.0-1"), "0.26.0");
        assert_eq!(upstream_version("1:0.26.0-1"), "0.26.0");
        // The revision is the last hyphen, so an upstream version may
        // contain one of its own.
        assert_eq!(upstream_version("1.0-rc1-1"), "1.0-rc1");
        // A native package has no revision at all.
        assert_eq!(upstream_version("1.2.3"), "1.2.3");
    }

    #[test]
    fn packaging_adopted_from_another_release_is_a_blocker() {
        // The overlay this check was written for: a changelog left at
        // 0.25.4 while 0.26.0 was what got packaged.
        let found = version_findings("0.25.4-1", "unstable", Some("0.26.0"));
        assert_eq!(names(&found), ["stale-changelog-version"]);
        assert_eq!(found[0].severity, Severity::Blocker);
        assert!(found[0].command.is_some(), "a finding names its command");
    }

    #[test]
    fn a_changelog_that_matches_its_source_says_nothing() {
        assert!(version_findings("0.26.0-1", "unstable", Some("0.26.0")).is_empty());
        // The epoch and revision are not the source's business.
        assert!(version_findings("1:0.26.0-3", "unstable", Some("0.26.0")).is_empty());
    }

    #[test]
    fn an_unknown_source_version_is_reported_rather_than_passed_over() {
        // A check that skips silently cannot be told from one that
        // passed, which is the whole failure mode here.
        let found = version_findings("0.26.0-1", "unstable", None);
        assert_eq!(names(&found), ["source-version-unknown"]);
        assert_eq!(found[0].severity, Severity::Note);
    }

    #[test]
    fn a_generated_placeholder_distribution_is_flagged() {
        let found = version_findings(
            "0.26.0-1",
            "UNRELEASED-FIXME-AUTOGENERATED-DEBCARGO",
            Some("0.26.0"),
        );
        assert_eq!(names(&found), ["changelog-distribution-placeholder"]);
        // A plain UNRELEASED is the normal state while preparing an
        // upload, so it must not be noise.
        assert!(version_findings("0.26.0-1", "UNRELEASED", Some("0.26.0")).is_empty());
    }

    #[test]
    fn series_and_the_patch_directory_are_compared_both_ways() {
        let present = BTreeSet::from(["kept.patch".to_string(), "dropped.patch".to_string()]);
        let found = series_findings("kept.patch\ngone.patch\n", &present);
        // Listed but absent breaks the build; present but unlisted is
        // the dropped patch that still looks carried.
        assert_eq!(names(&found), ["patch-missing", "patch-unlisted"]);
        assert!(found[0].message.contains("gone.patch"));
        assert!(found[1].message.contains("dropped.patch"));
    }

    #[test]
    fn series_comments_blank_lines_and_options_are_read_the_way_quilt_reads_them() {
        let present = BTreeSet::from(["a.patch".to_string()]);
        let series = "# a comment\n\na.patch -p0\n";
        assert!(
            series_findings(series, &present).is_empty(),
            "a commented series with an option is still satisfied"
        );
    }

    #[test]
    fn a_patch_upstream_has_taken_is_a_blocker_naming_its_command() {
        let found = apply_findings(&[("relax-anstream.patch".into(), "hunk #1 FAILED".into())]);
        assert_eq!(names(&found), ["patch-does-not-apply"]);
        assert_eq!(
            found[0].command.as_deref(),
            Some("patch -p1 --dry-run -f -i debian/patches/relax-anstream.patch")
        );
    }

    #[test]
    fn a_comment_about_an_older_release_is_a_note() {
        // The real one: "Packaging dbranch 0.25.4 before 0.26.0
        // exists" sat in debcargo.toml long after 0.26.0 existed.
        let files = BTreeMap::from([(
            "debian/debcargo.toml".to_string(),
            "# Packaging dbranch 0.25.4 before 0.26.0 exists\noverlay = \".\"\n".to_string(),
        )]);
        let found = stale_mention_findings(&files, "0.26.0");
        assert_eq!(names(&found), ["stale-version-mention"]);
        assert!(found[0].message.contains("0.25.4"));
        // The current release is mentioned in the same line and must
        // not be reported as stale.
        assert!(!found[0].message.contains("older than the packaged 0.25.4"));
    }

    #[test]
    fn only_comments_are_read_and_only_for_older_releases() {
        let files = BTreeMap::from([(
            "debian/rules".to_string(),
            // Not a comment, so not prose going stale.
            "VERSION = 0.1.0\n# needs 9.9.9 one day\n# see bug 1234567\n".to_string(),
        )]);
        assert!(
            stale_mention_findings(&files, "0.26.0").is_empty(),
            "code, newer versions and bug numbers are all left alone"
        );
    }

    #[test]
    fn a_generated_suite_is_named_rather_than_called_nothing() {
        // autopkgtest-pkg-python emits one `import` marked
        // `superficial`; pkg-ruby runs the gem's own suite and skips
        // when there is none. Both are real autopkgtests, so saying
        // "no autopkgtest" would be false -- what is true is that
        // neither runs the installed program.
        let found = autopkgtest_findings(None, Some("autopkgtest-pkg-python"));
        assert_eq!(names(&found), ["no-program-test"]);
        assert!(
            found[0]
                .message
                .contains("autopkgtest-pkg-python exercises"),
            "{}",
            found[0].message
        );
    }

    #[test]
    fn debcargo_stanzas_alone_still_leave_the_programs_untested() {
        let generated = "Test-Command: /usr/share/cargo/bin/cargo-auto-test dbranch 0.26.0\n";
        let found = autopkgtest_findings(Some(generated), None);
        assert_eq!(names(&found), ["no-program-test"]);
        // A named script is a test of the program; this is not.
        assert!(autopkgtest_findings(Some("Tests: smoke\n"), None).is_empty());
    }

    #[test]
    fn the_declared_testsuite_is_read_from_debian_control() {
        let control = "Source: damo\nSection: admin\nTestsuite: autopkgtest-pkg-python\n";
        assert_eq!(
            declared_testsuite(control).as_deref(),
            Some("autopkgtest-pkg-python")
        );
        assert_eq!(declared_testsuite("Source: foo\n"), None);
        assert_eq!(declared_testsuite("Testsuite:\n"), None);
    }

    #[test]
    fn a_man_page_that_reaches_no_package_is_reported() {
        // dbranch's own: man/dbranch.1 ships in the crate and nothing
        // installs it, so `man dbranch` would be missing.
        let assets = [SourceAsset {
            path: "man/dbranch.1".into(),
            basename: "dbranch.1".into(),
            kind: "man page",
        }];
        let packaged = BTreeSet::from(["./usr/bin/dbranch".to_string()]);
        let found = uninstalled_assets(&assets, &packaged);
        assert_eq!(names(&found), ["shipped-but-not-installed"]);
    }

    #[test]
    fn an_asset_the_package_compresses_still_counts_as_installed() {
        let assets = [SourceAsset {
            path: "man/dbranch.1".into(),
            basename: "dbranch.1".into(),
            kind: "man page",
        }];
        let packaged = BTreeSet::from(["./usr/share/man/man1/dbranch.1.gz".to_string()]);
        assert!(
            uninstalled_assets(&assets, &packaged).is_empty(),
            "a man page is installed compressed, which is still installed"
        );
    }

    /// A minimal package working directory: a changelog, and
    /// whatever else the test needs written on top.
    fn fixture(version: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a temp dir");
        std::fs::create_dir_all(dir.path().join("debian")).unwrap();
        std::fs::write(
            dir.path().join("debian/changelog"),
            format!(
                "foo ({version}) unstable; urgency=medium\n\n  * Thing.\n\n \
                 -- A <a@b.c>  Mon, 19 Oct 2026 10:00:00 +0100\n"
            ),
        )
        .unwrap();
        dir
    }

    /// A Ui that neither narrates nor runs anything of its own.
    fn silent() -> crate::ui::Ui {
        crate::ui::Ui {
            explain: false,
            dry_run: false,
            quiet: true,
        }
    }

    #[test]
    fn the_source_states_its_version_through_cargo_then_its_directory() {
        let dir = fixture("1.2.3-1");
        // Nothing states it yet, and the temp directory's name is not
        // a `<name>-<version>`, so the honest answer is "unknown".
        assert_eq!(source_version(dir.path()), None);
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"foo\"\nversion = \"1.2.3\"\n",
        )
        .unwrap();
        assert_eq!(source_version(dir.path()).as_deref(), Some("1.2.3"));
    }

    #[test]
    fn the_patch_directory_is_read_without_its_series_file() {
        let dir = fixture("1.2.3-1");
        let patches = dir.path().join("debian/patches");
        std::fs::create_dir_all(&patches).unwrap();
        std::fs::write(patches.join("series"), "a.patch\n").unwrap();
        std::fs::write(patches.join("a.patch"), "--- a\n+++ b\n").unwrap();
        std::fs::write(patches.join("b.patch"), "--- a\n+++ b\n").unwrap();
        let found = patch_files(&patches);
        assert_eq!(
            found,
            BTreeSet::from(["a.patch".to_string(), "b.patch".to_string()]),
            "series is the list, not a patch"
        );
        // A directory that is not there is empty, not an error: a
        // package with no patches is the normal case.
        assert!(patch_files(&dir.path().join("debian/nope")).is_empty());
    }

    #[test]
    fn only_the_files_directly_in_debian_are_read_for_prose() {
        let dir = fixture("1.2.3-1");
        std::fs::create_dir_all(dir.path().join("debian/patches")).unwrap();
        std::fs::write(dir.path().join("debian/rules"), "#!/usr/bin/make -f\n").unwrap();
        let texts = debian_texts(&dir.path().join("debian"));
        let mut keys: Vec<&str> = texts.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, ["debian/changelog", "debian/rules"]);
    }

    #[test]
    fn assets_are_collected_from_the_source_but_not_from_debian_or_git() {
        let dir = fixture("1.2.3-1");
        std::fs::create_dir_all(dir.path().join("man")).unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("man/foo.1"), ".TH FOO 1\n").unwrap();
        std::fs::write(dir.path().join("README.md"), "hi\n").unwrap();
        // Assets inside the skipped directories must not be reported:
        // debian/ is packaging, and the others are not shipped source.
        std::fs::write(dir.path().join("debian/foo.1"), ".TH\n").unwrap();
        std::fs::write(dir.path().join(".git/foo.1"), ".TH\n").unwrap();
        std::fs::write(dir.path().join("target/foo.1"), ".TH\n").unwrap();
        let assets = collect_assets(dir.path());
        assert_eq!(assets.len(), 1, "{assets:?}");
        assert_eq!(assets[0].path, "man/foo.1");
        assert_eq!(assets[0].kind, "man page");
    }

    #[test]
    fn a_source_only_run_reports_what_it_could_not_check() {
        let dir = fixture("1.2.3-1");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"foo\"\nversion = \"1.2.3\"\n",
        )
        .unwrap();
        let opts = CheckOptions {
            repo: dir.path().to_path_buf(),
            debs: Vec::new(),
            json: false,
        };
        let found = run(&opts, &silent()).expect("a readable tree checks");
        // The versions agree, so the only thing to say is what was
        // not looked at for want of a built package.
        assert_eq!(names(&found), ["no-program-test", "packages-not-checked"]);
        assert!(!has_blocker(&found));
    }

    #[test]
    fn a_run_against_packaging_from_another_release_fails() {
        let dir = fixture("0.25.4-1");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"dbranch\"\nversion = \"0.26.0\"\n",
        )
        .unwrap();
        let opts = CheckOptions {
            repo: dir.path().to_path_buf(),
            debs: Vec::new(),
            json: false,
        };
        let found = run(&opts, &silent()).expect("a readable tree checks");
        assert!(names(&found).contains(&"stale-changelog-version"));
        assert!(
            has_blocker(&found),
            "packaging describing another release fails the run"
        );
    }

    #[test]
    fn a_tree_with_no_debian_directory_is_an_error_not_a_pass() {
        let dir = tempfile::tempdir().unwrap();
        let opts = CheckOptions {
            repo: dir.path().to_path_buf(),
            debs: Vec::new(),
            json: false,
        };
        let err = run(&opts, &silent()).expect_err("there is nothing to check");
        assert!(err.contains("no debian/"), "{err}");
    }

    #[test]
    fn the_report_leads_with_what_blocks_and_carries_each_command() {
        let findings = vec![
            Finding::new("a-note", Severity::Note, "just saying".into()),
            Finding::new("a-blocker", Severity::Blocker, "it does not run".into())
                .with_command("/usr/bin/foo --help".into()),
        ];
        let text = render(&findings);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "blocker: it does not run");
        assert_eq!(lines[1], "    /usr/bin/foo --help");
        assert_eq!(lines[2], "note: just saying");
        assert_eq!(render(&[]), "nothing to report\n");
    }

    #[test]
    fn a_dpkg_deb_listing_is_read_past_its_symlinks_and_directories() {
        // The last field is the path only for a plain file. A symlink
        // line ends with its target, and a directory is not something
        // that can be run -- `/usr/bin/` was reported as a program
        // that would not start before this was handled.
        assert_eq!(
            deb_entry_path("-rwxr-xr-x root/root 12 2026-10-19 10:00 ./usr/bin/foo").as_deref(),
            Some("./usr/bin/foo")
        );
        assert_eq!(
            deb_entry_path("lrwxrwxrwx root/root 0 2026-10-19 10:00 ./usr/bin/bar -> foo")
                .as_deref(),
            Some("./usr/bin/bar")
        );
        assert_eq!(
            deb_entry_path("drwxr-xr-x root/root 0 2026-10-19 10:00 ./usr/bin/"),
            None
        );
    }

    #[test]
    fn a_crates_own_version_is_read_from_its_package_table() {
        let toml = "[package]\nname = \"dbranch\"\nversion = \"0.26.0\"\n\n\
                    [dependencies]\nserde = \"1.0\"\n";
        assert_eq!(crate_version(toml).as_deref(), Some("0.26.0"));
    }

    #[test]
    fn a_dependencys_version_is_never_read_as_the_crates_own() {
        // Stop at the next table, or the first dependency's version
        // becomes the package's.
        let toml = "[dependencies]\nserde = \"1.0\"\n\n[package]\nname = \"x\"\n";
        assert_eq!(crate_version(toml), None);
        // An inherited version states nothing on its own.
        let inherited = "[package]\nname = \"x\"\nversion.workspace = true\n";
        assert_eq!(crate_version(inherited), None);
    }

    #[test]
    fn an_unpacked_directory_names_its_version_when_a_crate_does_not() {
        assert_eq!(
            dir_version("rust-dbranch-0.26.0").as_deref(),
            Some("0.26.0")
        );
        // A bare name is not a version, so it is not guessed at.
        assert_eq!(dir_version("dbranch"), None);
        assert_eq!(dir_version("some-package"), None);
    }

    #[test]
    fn the_assets_worth_installing_are_recognised_by_name() {
        assert_eq!(classify_asset("man/dbranch.1"), Some("man page"));
        assert_eq!(classify_asset("foo.desktop"), Some("desktop entry"));
        assert_eq!(
            classify_asset("a/foo.metainfo.xml"),
            Some("AppStream metadata")
        );
        assert_eq!(classify_asset("foo.service"), Some("systemd unit"));
        // Source is not an asset, and neither is a dotted name whose
        // tail merely looks like a section.
        assert_eq!(classify_asset("src/main.rs"), None);
        assert_eq!(classify_asset("README.md"), None);
        assert_eq!(classify_asset("libfoo.so"), None);
    }

    #[test]
    fn a_program_that_dies_on_help_is_a_blocker() {
        // The failure this module exists for: the binary is there,
        // the build passed, and it cannot start because a file it
        // loads at runtime was never installed.
        let results = [(
            "/usr/bin/fnlfmt".to_string(),
            Smoke::Failed {
                code: 1,
                output: "lua5.4: /usr/bin/fnlfmt:7: module 'indentation' not found:\n  no field"
                    .into(),
            },
        )];
        let found = smoke_findings(&results);
        assert_eq!(names(&found), ["binary-does-not-run"]);
        assert_eq!(found[0].severity, Severity::Blocker);
        assert!(found[0].message.contains("module 'indentation' not found"));
        assert!(has_blocker(&found));
    }

    #[test]
    fn a_program_that_runs_says_nothing_and_one_that_could_not_be_tried_says_so() {
        let results = [
            ("/usr/bin/dbranch".to_string(), Smoke::Ok),
            (
                "/usr/sbin/thing".to_string(),
                Smoke::NotRun("not executable here".into()),
            ),
        ];
        let found = smoke_findings(&results);
        assert_eq!(names(&found), ["binary-not-run"]);
        assert_eq!(found[0].severity, Severity::Note);
        assert!(!has_blocker(&found), "an untried binary is not a blocker");
    }
}
