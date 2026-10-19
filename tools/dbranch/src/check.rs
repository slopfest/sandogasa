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

    /// How the label is coloured, so a blocker is findable in a long
    /// report without reading it. Rendered through `anstream`, which
    /// drops the codes when output is not a terminal or `NO_COLOR`
    /// is set.
    pub fn style(self) -> anstyle::Style {
        let colour = match self {
            Severity::Blocker => anstyle::AnsiColor::Red,
            Severity::Warning => anstyle::AnsiColor::Yellow,
            Severity::Note => anstyle::AnsiColor::BrightBlack,
        };
        let style = anstyle::Style::new().fg_color(Some(colour.into()));
        match self {
            Severity::Blocker => style.bold(),
            _ => style,
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
        // Nothing to report. A packaging tree records its upstream
        // version nowhere, so this is not a check that was skipped
        // but one that does not apply, and saying so on every
        // Debian package would be noise. What the version can be
        // checked against is a build, which `packages-not-checked`
        // already offers.
        None => {}
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

/// What trying a patch against this source showed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchState {
    /// It applies cleanly.
    Applies,
    /// It is already in the source: the tree has patches applied, as
    /// gbp's layout leaves them.
    AlreadyApplied,
    /// It applies in neither direction, with what patch(1) said.
    Broken(String),
}

/// Findings for the patches that were tried.
///
/// The already-applied ones collapse into a single note, because the
/// thing worth saying is about the tree and not about any one patch:
/// here a patch you carry and a fix upstream has taken look exactly
/// alike, so the stale-patch question cannot be answered at all.
/// Saying nothing would imply it had been; saying it once per patch
/// would bury a package carrying twenty of them.
///
/// It stays a note rather than silence for the same reason the check
/// exists: this is the state where staleness hides. It is also why
/// the check matters most before a build, on a tree whose patches
/// are not yet applied.
pub fn patch_findings(states: &[(String, PatchState)], quilt_applied: bool) -> Vec<Finding> {
    let mut found: Vec<Finding> = states
        .iter()
        .filter_map(|(patch, state)| match state {
            PatchState::Broken(why) => Some(
                Finding::new(
                    "patch-does-not-apply",
                    Severity::Blocker,
                    format!("debian/patches/{patch} no longer applies ({why})"),
                )
                .with_command(format!("patch -p1 --dry-run -f -i debian/patches/{patch}")),
            ),
            _ => None,
        })
        .collect();
    let applied = states
        .iter()
        .filter(|(_, state)| *state == PatchState::AlreadyApplied)
        .count();
    if applied > 0 {
        // Two different states look alike from the content alone,
        // and they need different fixes. Quilt recording the patches
        // as applied means a build left them that way and popping
        // them puts it right. Quilt recording nothing means the
        // change is committed on the packaging branch *as well as*
        // carried as a patch -- popping would do nothing at all,
        // which is worse than useless advice.
        let (why, command) = match quilt_applied {
            true => (
                "most likely left so by a build",
                "QUILT_PATCHES=debian/patches quilt pop -a",
            ),
            false => (
                "though quilt records none applied, so the change is \
                 committed on this branch as well as carried as a patch",
                "QUILT_PATCHES=debian/patches quilt applied",
            ),
        };
        found.push(
            Finding::new(
                "patches-applied-in-tree",
                Severity::Note,
                format!(
                    "{applied} of {} patches are applied in this tree, \
                     {why}. Whether any is stale cannot be told here: \
                     once applied, a patch you carry and a fix upstream \
                     has taken look alike.",
                    states.len()
                ),
            )
            .with_command(command.to_string()),
        );
    }
    found
}

/// Where a patch's DEP-3 header stops and its diff begins.
fn patch_header(text: &str) -> &str {
    let end = text
        .lines()
        .scan(0usize, |at, line| {
            let start = *at;
            *at += line.len() + 1;
            Some((start, line))
        })
        .find(|(_, line)| {
            line.starts_with("--- ")
                || line.starts_with("diff --git ")
                || line.starts_with("Index: ")
                || *line == "---"
        })
        .map_or(text.len(), |(at, _)| at);
    &text[..end]
}

/// Whether a patch's header is readable as DEP-3.
///
/// Only two things are insisted on, being what DEP-3 calls required:
/// what the patch does, and where it came from. Everything else
/// about the header's shape is left alone -- see the note in the
/// body for why blank lines are not a finding.
pub fn dep3_findings(patch: &str, text: &str) -> Vec<Finding> {
    let header = patch_header(text);
    let has = |keys: &[&str]| {
        header
            .lines()
            .any(|l| keys.iter().any(|k| l.starts_with(k)))
    };
    let mut found = Vec::new();
    if !has(&["Description:", "Subject:"]) {
        found.push(dep3(
            patch,
            "says nowhere what it does (Description or Subject)",
        ));
    }
    if !has(&["Origin:", "Author:", "From:"]) {
        found.push(dep3(
            patch,
            "says nowhere where it came from (Origin, Author or From)",
        ));
    }
    // Deliberately not checked: blank lines in the header. DEP-3 is
    // RFC822-shaped, so in principle a blank line ends the field
    // block -- but lintian reads the fields below one anyway (a
    // patch whose `Forwarded:` sits under two blank lines does not
    // raise patch-not-forwarded-upstream), and `gbp pq export`
    // cannot emit anything else: it serialises the commit as mail,
    // so the blank after Subject is structural. Flagging it would
    // disagree with lintian and with git's own trailer convention,
    // which puts a blank line before Signed-off-by.
    found
}

/// One DEP-3 complaint about a patch.
fn dep3(patch: &str, what: &str) -> Finding {
    Finding::new(
        "patch-header-not-dep3",
        Severity::Note,
        format!("debian/patches/{patch} {what}"),
    )
    .with_command(format!("head -20 debian/patches/{patch}"))
}

/// Lintian overrides that name a tag and nothing else.
///
/// An override carrying no pattern silences that tag everywhere, for
/// ever -- including the occurrence nobody has introduced yet. The
/// one that prompted this silenced `very-long-line-length-in-source-
/// file` across a whole package to excuse two vendored files.
///
/// Reported as a note rather than a warning, because a bare tag is
/// right for tags that carry no file of their own: whether a given
/// one does is lintian's knowledge, not ours, so this points at the
/// question instead of answering it.
pub fn override_findings(path: &str, text: &str) -> Vec<Finding> {
    let bare: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        // An override may be scoped to a binary package first.
        .map(|l| l.rsplit(']').next().unwrap_or(l).trim())
        .filter(|l| !l.is_empty() && l.split_whitespace().count() == 1)
        .collect();
    if bare.is_empty() {
        return Vec::new();
    }
    vec![
        Finding::new(
            "unscoped-lintian-override",
            Severity::Note,
            format!(
                "{path} silences {} with no pattern, so it applies to \
                 every occurrence including ones not there yet — give \
                 a pattern to any that names a file",
                bare.join(", ")
            ),
        )
        .with_command(format!("lintian-explain-tags {}", bare[0])),
    ]
}

/// Whether a build will leave this tree with its patches applied.
///
/// `dpkg-source` applies the patches in its `--before-build` hook
/// and only puts them back if `unapply-patches` is set in
/// `debian/source/local-options`. Without it every build leaves the
/// working tree patched: `git status` and `git diff` then describe a
/// state nobody chose, `gbp pq` round-trips get confusing, and a
/// `.pc` directory sits untracked for ever.
///
/// gbp ships the fix as an example script, which is what the command
/// points at rather than two hand-typed lines.
pub fn unapply_findings(
    format: Option<&str>,
    has_patches: bool,
    local_options: Option<&str>,
    pc_ignored: bool,
) -> Vec<Finding> {
    if !has_patches || !format.is_some_and(|f| f.contains("3.0 (quilt)")) {
        return Vec::new();
    }
    let mut found = Vec::new();
    let configured = local_options.is_some_and(|o| {
        o.lines()
            .any(|l| matches!(l.trim(), "unapply-patches" | "--unapply-patches"))
    });
    if !configured {
        found.push(
            Finding::new(
                "patches-left-applied-after-build",
                Severity::Warning,
                "debian/source/local-options does not set unapply-patches, \
                 so every build leaves this tree with its patches applied \
                 — which is not the state a packaging branch is meant to \
                 be in. That file never ships in the source package, so \
                 setting it affects nobody else's build; see dpkg-source(1) \
                 under --unapply-patches and debian/source/local-options."
                    .into(),
            )
            // The line itself rather than gbp's example script: the
            // script is only present when git-buildpackage's docs
            // are installed, it also does the `.pc` half that has
            // its own finding, and a command you can read beats a
            // path you have to go and open.
            .with_command(
                "mkdir -p debian/source && echo unapply-patches \
                 >> debian/source/local-options"
                    .into(),
            ),
        );
    }
    if !pc_ignored {
        found.push(
            Finding::new(
                "pc-directory-not-ignored",
                Severity::Note,
                "quilt's .pc directory is not ignored, so it shows up as \
                 untracked after every build"
                    .into(),
            )
            .with_command("echo '.pc/' >> .git/info/exclude".into()),
        );
    }
    found
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

/// The source package a .deb belongs to, from `dpkg-deb -f` output.
///
/// `Source` names it when the binary package is named differently
/// (`librust-sandogasa-build-dev` comes from `rust-sandogasa-build`),
/// and is simply absent when the two agree, as for `fnlfmt`. It may
/// carry a version in parentheses, which is not part of the name.
pub fn deb_source_name(fields: &str) -> Option<String> {
    let field = |key: &str| {
        fields
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    field("Source:")
        .map(|v| v.split_whitespace().next().unwrap_or(v))
        .or_else(|| field("Package:"))
        .map(str::to_string)
}

/// Why a .deb does not belong with this source tree, if it does not.
///
/// Worth failing the whole run over rather than reporting: every
/// package check compares the source against the .deb, so the wrong
/// .deb does not produce wrong findings here and there -- it reports
/// every file the source ships as missing, which reads exactly like
/// a catastrophically broken package. `-C` moves the tree being
/// checked but not the `--deb` paths, which resolve against the
/// current directory, so the two really can name different packages.
pub fn source_mismatch(tree_source: &str, deb_source: Option<&str>, deb: &str) -> Option<String> {
    match deb_source {
        Some(name) if name == tree_source => None,
        Some(name) => Some(format!(
            "{deb} is a package of {name}, but debian/changelog here is \
             {tree_source} — `-C` moves the tree being checked, not the \
             --deb paths, which are relative to where you are"
        )),
        None => Some(format!("{deb} names no package — is it a .deb?")),
    }
}

/// Packages built from a different changelog entry than this tree's.
///
/// Only possible once a .deb is in hand, and worth saying because
/// the alternative is checking yesterday's build and believing the
/// answer. The version is the packaging's own, so this compares
/// like with like -- unlike the source version, which an unpacked
/// VCS tree does not record anywhere.
pub fn stale_build_findings(changelog_version: &str, debs: &[(String, String)]) -> Vec<Finding> {
    debs.iter()
        .filter(|(_, version)| version != changelog_version)
        .map(|(deb, version)| {
            Finding::new(
                "stale-build",
                Severity::Warning,
                format!(
                    "{deb} was built as {version}, but debian/changelog \
                     is at {changelog_version} — this checks the older \
                     build, not what the packaging now says"
                ),
            )
            .with_command("head -1 debian/changelog".into())
        })
        .collect()
}

/// Whether anything runs the programs this package installs.
///
/// debcargo generates test stanzas for a crate with a library
/// package, and none at all for one built binary-only -- which is
/// exactly the package whose program most wants running. A build
/// never runs it, so without this there is nothing between a broken
/// binary and its first user.
pub fn autopkgtest_findings(
    control: Option<&str>,
    testsuite: Option<&str>,
    debs: &[String],
) -> Vec<Finding> {
    if control.is_some_and(crate::autopkgtest::declares_test) {
        return Vec::new();
    }
    // A generated suite is not nothing, so "no autopkgtest" would be
    // false. What is true is narrower and more useful: none of them
    // runs the installed program. autodep8's Python generator emits
    // one `import` and marks it `superficial`, Debian's own word for
    // a test that proves little; its Ruby generator runs the gem's
    // own suite, which is the library's tests and is `skippable` when
    // there are none; debcargo's stanzas run cargo tests against the
    // crate. All three leave /usr/bin untouched.
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
        Finding::new("no-program-test", Severity::Warning, message).with_command(
            // Name the packages in hand rather than guessing where
            // they are. `../*.deb` is only right for a build that
            // left its output beside the source; pbuilder-dist,
            // which dbranch itself drives, puts it under
            // ~/pbuilder/<dist>_result/, where that glob matches
            // whatever unrelated package happens to be there.
            match debs.is_empty() {
                false => format!("dbranch autopkgtest --deb {}", debs.join(" ")),
                // A plain placeholder rather than a glob. Only the
                // operator knows where their build put the package,
                // and a glob guessed at from here matches whatever
                // else happens to be in that directory.
                true => "dbranch autopkgtest --deb path-to-deb".to_string(),
            },
        ),
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

/// The packages a `Depends:` field names, bare of versions and
/// alternatives.
///
/// Only the names are wanted, to ask dpkg whether each is installed.
/// An alternative (`a | b`) is satisfied by any of its branches, so
/// each is reported separately and the caller says which are absent.
pub fn depends_names(field: &str) -> Vec<String> {
    field
        .split(',')
        .flat_map(|dep| dep.split('|'))
        .filter_map(|dep| dep.split_whitespace().next())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

/// Why a program could not be run here, naming the dependencies that
/// are missing when any are.
///
/// Extracting a package installs nothing, so an interpreted program
/// finds its runtime only if the host happens to have it. Saying
/// which packages are absent turns a bare `env: 'lua5.4': No such
/// file or directory` into something actionable, and makes plain
/// that the answer is about this machine rather than the package.
pub fn missing_dependency_note(output: &str, missing: &[String]) -> String {
    match missing.is_empty() {
        true => format!(
            "{output} — install the package's dependencies, or run \
             this where they are"
        ),
        false => format!(
            "{output} — {} {} not installed here; autopkgtest installs \
             a package's dependencies in a testbed, which this does not",
            missing.join(", "),
            match missing.len() {
                1 => "is",
                _ => "are",
            }
        ),
    }
}

/// How the installed copy of a package compares with the one being
/// checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    /// Not installed here at all.
    Absent,
    /// Installed, and holding this package's own files.
    Same,
    /// Installed, but not this package.
    Differs(String),
}

/// Decide what is installed from what could be observed.
///
/// `checksums_match` is the real answer when dpkg recorded the
/// installed files' hashes: it compares *contents*, which is the
/// only thing that catches a rebuild. Iterating on packaging
/// produces the same version over and over, so a version comparison
/// would call an hours-old installed copy a match and quietly test
/// that instead of the package just built. Versions are the fallback
/// for a package shipping no `md5sums`.
///
/// Those really are MD5: it is the only digest dpkg records, and it
/// is enough here. The question is "did I rebuild this since
/// installing it", not "is this package forged" — nobody is crafting
/// a colliding .deb against their own build tree — so MD5's
/// collision weakness does not bear on the answer. Nothing in this
/// check should be read as verifying a package's integrity.
pub fn installed_state(
    installed_version: Option<&str>,
    deb_version: &str,
    checksums_match: Option<bool>,
) -> Installed {
    let Some(installed) = installed_version else {
        return Installed::Absent;
    };
    let versions_differ = format!("{installed} is installed, and this package is {deb_version}");
    match checksums_match {
        Some(true) => Installed::Same,
        Some(false) if installed == deb_version => Installed::Differs(format!(
            "the installed {installed} holds different files from the \
             package being checked, which only their contents reveal \
             because a rebuild keeps the version"
        )),
        Some(false) | None if installed != deb_version => Installed::Differs(versions_differ),
        Some(false) => Installed::Differs(versions_differ),
        None => Installed::Same,
    }
}

/// What to say when the installed copy is not the one being checked.
///
/// A warning rather than a blocker: nothing is wrong with the
/// package, but every answer the smoke test gives is about the other
/// copy, so it must not pass silently.
pub fn install_findings(state: &Installed, install: &str) -> Vec<Finding> {
    match state {
        Installed::Differs(why) => vec![
            Finding::new(
                "installed-copy-differs",
                Severity::Warning,
                format!(
                    "{why}. Running a program here exercises the \
                     installed copy's files, not this package's."
                ),
            )
            .with_command(install.to_string()),
        ],
        _ => Vec::new(),
    }
}

/// What the smoke test found: each program's outcome, and how the
/// installed copy compared with the package it ran from.
type SmokeReport = (Vec<(String, Smoke)>, Vec<Installed>);

/// What one attempt at asking a program for help amounts to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attempt {
    /// It answered; there is nothing more to try.
    Answered,
    /// No other verb will fare better either.
    Hopeless(Smoke),
    /// This verb was refused; another may still work.
    Refused(Smoke),
}

/// Read one attempt, given what the run cost and what can be
/// believed about it.
///
/// The whole judgement of the smoke test lives here: 127 means the
/// host is missing something rather than the package being broken,
/// and an ordinary failure is only the package's fault when the run
/// could be believed in the first place.
pub fn attempt_verdict(code: i32, output: &str, conclusive: bool, missing: &[String]) -> Attempt {
    match code {
        0 => Attempt::Answered,
        // "command not found": the interpreter or a dependency is
        // absent here, so every verb will fail the same way.
        127 => Attempt::Hopeless(Smoke::NotRun(missing_dependency_note(
            first_line(output),
            missing,
        ))),
        _ if !conclusive => Attempt::Refused(Smoke::NotRun(format!(
            "{} — this package ships files it may load at run time, and \
             an extracted copy finds those only once the package is \
             installed, so the failure says nothing about the package",
            first_line(output)
        ))),
        _ => Attempt::Refused(Smoke::Failed {
            code,
            output: output.to_string(),
        }),
    }
}

/// Whether a `dpkg-query` status line reports a package installed.
pub fn dpkg_reports_installed(status: &str) -> bool {
    status.contains("install ok installed")
}

/// Programs the package installs that do not run.
///
/// The one check that would have caught the failure this module was
/// written for: a `/usr/bin` entry whose supporting file was never
/// installed, so it dies on `--help` with a module-not-found. Nothing
/// in a build catches that, because the build is not what runs it.
pub fn smoke_findings(results: &[(String, Smoke)], install: &str) -> Vec<Finding> {
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
            Smoke::NotRun(why) => Some(
                Finding::new(
                    "binary-not-run",
                    Severity::Note,
                    format!("{binary} was not run: {why}"),
                )
                // The command that would make the answer mean
                // something. Suggested rather than run: installing a
                // package system-wide is not something a read-only
                // check does on your behalf, however useful.
                .with_command(install.to_string()),
            ),
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
        // The absolute path, not the literal `.`: being somewhere
        // other than you thought is exactly how this is reached.
        return Err(format!(
            "{} has no debian/ directory — use -C to point at the package",
            std::path::absolute(&opts.repo)
                .unwrap_or_else(|_| opts.repo.clone())
                .display()
        ));
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

    // Before anything else looks at them: a .deb from another package
    // would be compared against this source and report all of it
    // missing. Cheap to check, and catastrophic to miss.
    for deb in &opts.debs {
        let shown = std::path::absolute(deb)
            .unwrap_or_else(|_| deb.clone())
            .display()
            .to_string();
        if !deb.is_file() {
            return Err(format!("{shown}: no such file"));
        }
        let argv: Vec<String> = ["dpkg-deb", "-f", &shown, "Source", "Package"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (code, fields) = ui
            .run_capture(&argv, &opts.repo)
            .map_err(|e| format!("dpkg-deb -f {shown}: {e}"))?;
        if code != 0 {
            return Err(format!("dpkg-deb -f {shown} failed: {fields}"));
        }
        if let Some(why) =
            source_mismatch(&header.package, deb_source_name(&fields).as_deref(), &shown)
        {
            return Err(why);
        }
    }

    ui.step("Compare debian/patches against the series file");
    let series_path = debian.join("patches/series");
    if series_path.is_file() {
        let series = read(&series_path)?;
        let present = patch_files(&debian.join("patches"));
        findings.extend(series_findings(&series, &present));
        findings.extend(patch_findings(
            &patch_states(&series, ui, &opts.repo),
            quilt_records_applied(&opts.repo),
        ));
    }

    ui.step("Read each patch's header as DEP-3");
    for patch in patch_files(&debian.join("patches")) {
        if let Ok(text) = std::fs::read_to_string(debian.join("patches").join(&patch)) {
            findings.extend(dep3_findings(&patch, &text));
        }
    }

    ui.step("Look for lintian overrides with nothing to narrow them");
    for (path, text) in override_files(&debian) {
        findings.extend(override_findings(&path, &text));
    }

    ui.step("Check a build will not leave the tree patched");
    findings.extend(unapply_findings(
        std::fs::read_to_string(debian.join("source/format"))
            .ok()
            .as_deref(),
        debian.join("patches/series").is_file(),
        std::fs::read_to_string(debian.join("source/local-options"))
            .ok()
            .as_deref(),
        pc_is_ignored(&opts.repo),
    ));

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
        &opts
            .debs
            .iter()
            .map(|d| {
                std::path::absolute(d)
                    .unwrap_or_else(|_| d.clone())
                    .display()
                    .to_string()
            })
            .collect::<Vec<_>>(),
    ));

    ui.step("Look for comments left behind by an older release");
    findings.extend(stale_mention_findings(
        &debian_texts(&debian),
        upstream_version(&header.version),
    ));

    // Running without a built package is a mode, not an omission:
    // some of this is worth knowing before a build and the rest
    // needs one afterwards. Saying "pass --deb" every time would be
    // nagging someone who deliberately ran the first kind.
    if !opts.debs.is_empty() {
        ui.step("Check the packages were built from this packaging");
        let built: Vec<(String, String)> = opts
            .debs
            .iter()
            .filter_map(|deb| {
                let version = deb_field(deb, "Version", ui)?.trim().to_string();
                Some((deb.display().to_string(), version))
            })
            .collect();
        findings.extend(stale_build_findings(&header.version, &built));

        ui.step("Compare what the source ships against what the packages install");
        let mut packaged = BTreeSet::new();
        for deb in &opts.debs {
            packaged.extend(deb_contents(deb, ui, &opts.repo)?);
        }
        findings.extend(uninstalled_assets(&collect_assets(&opts.repo), &packaged));

        ui.step("Run each installed program, which is what a build never does");
        // Name the packages actually in hand, so the suggestion can
        // be run rather than adapted. Absolute, because apt reads a
        // bare name as a package rather than a file, and because the
        // command may well be pasted into another directory.
        let install = format!(
            "sudo apt install {}",
            opts.debs
                .iter()
                .map(|d| std::path::absolute(d)
                    .unwrap_or_else(|_| d.clone())
                    .display()
                    .to_string())
                .collect::<Vec<_>>()
                .join(" ")
        );
        let (results, states) = smoke_test(&opts.debs, &packaged, ui)?;
        for state in &states {
            findings.extend(install_findings(state, &install));
        }
        findings.extend(smoke_findings(&results, &install));
    }

    report(&findings, opts.json);
    Ok(findings)
}

/// Every lintian override file under `debian/`, with its text.
fn override_files(debian: &std::path::Path) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut take = |path: std::path::PathBuf, shown: String| {
        if let Ok(text) = std::fs::read_to_string(&path) {
            found.push((shown, text));
        }
    };
    take(
        debian.join("source/lintian-overrides"),
        "debian/source/lintian-overrides".to_string(),
    );
    if let Ok(entries) = std::fs::read_dir(debian) {
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name.ends_with(".lintian-overrides") {
                take(entry.path(), format!("debian/{name}"));
            }
        }
    }
    found.sort();
    found
}

/// Whether quilt records any patch as currently applied.
///
/// `.pc/applied-patches` is quilt's own stack. A `.pc` directory can
/// exist with an empty or absent stack, which means the patched
/// content in the tree did not get there by quilt.
fn quilt_records_applied(repo: &std::path::Path) -> bool {
    std::fs::read_to_string(repo.join(".pc/applied-patches"))
        .is_ok_and(|text| text.lines().any(|l| !l.trim().is_empty()))
}

/// Whether quilt's `.pc` is kept out of git, or is not there to
/// matter. Both `.gitignore` and the repository's own exclude file
/// count, which is where gbp's example script puts it.
fn pc_is_ignored(repo: &std::path::Path) -> bool {
    if !repo.join(".pc").exists() {
        return true;
    }
    [repo.join(".gitignore"), repo.join(".git/info/exclude")]
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .any(|text| {
            text.lines()
                .any(|l| matches!(l.trim().trim_end_matches('/'), ".pc"))
        })
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

/// Try each of the series' patches, forwards then backwards.
///
/// The reverse attempt is what keeps this honest. A patch already in
/// the source fails to apply forwards exactly as a stale one does --
/// `1 out of 1 hunk FAILED` either way -- and reporting that as
/// broken is crying wolf at every gbp tree, which have their patches
/// applied and no `.pc` directory to notice it by.
fn patch_states(
    series: &str,
    ui: &crate::ui::Ui,
    repo: &std::path::Path,
) -> Vec<(String, PatchState)> {
    let mut states = Vec::new();
    for line in series.lines() {
        let patch = line.trim();
        if patch.is_empty() || patch.starts_with('#') {
            continue;
        }
        let patch = patch.split_whitespace().next().unwrap_or(patch);
        let try_patch = |reverse: bool| -> Result<(i32, String), std::io::Error> {
            let mut argv: Vec<String> = ["patch", "-p1", "--dry-run", "-f", "-s"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            if reverse {
                argv.push("-R".to_string());
            }
            argv.push("-i".to_string());
            argv.push(format!("debian/patches/{patch}"));
            ui.run_capture(&argv, repo)
        };
        let state = match try_patch(false) {
            Ok((0, out)) if out.trim().is_empty() => PatchState::Applies,
            Ok((_, forward)) => match try_patch(true) {
                Ok((0, out)) if out.trim().is_empty() => PatchState::AlreadyApplied,
                _ => PatchState::Broken(first_line(forward.trim()).to_string()),
            },
            // patch(1) missing is not the package's fault.
            Err(e) => PatchState::Broken(format!("could not run patch: {e}")),
        };
        states.push((patch.to_string(), state));
    }
    states
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

/// One field of a .deb's control, or `None` if it cannot be read.
fn deb_field(deb: &std::path::Path, field: &str, ui: &crate::ui::Ui) -> Option<String> {
    let argv: Vec<String> = ["dpkg-deb", "-f", &deb.display().to_string(), field]
        .iter()
        .map(|s| s.to_string())
        .collect();
    match ui.run_capture(&argv, std::path::Path::new(".")) {
        Ok((0, out)) => Some(out),
        _ => None,
    }
}

/// Which of `names` dpkg does not report as installed.
///
/// Best effort: a package dpkg cannot be asked about is treated as
/// present, because this exists to explain a failure rather than to
/// be another check, and guessing absent would invent one.
fn not_installed(names: &[String], ui: &crate::ui::Ui) -> Vec<String> {
    let mut missing = Vec::new();
    for name in names {
        let argv: Vec<String> = ["dpkg-query", "-W", "-f=${Status}", name]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let installed = matches!(
            ui.run_capture(&argv, std::path::Path::new(".")),
            Ok((0, status)) if dpkg_reports_installed(&status)
        );
        if !installed && !missing.contains(name) {
            missing.push(name.clone());
        }
    }
    missing
}

/// Dependencies of these packages that are not installed here.
fn uninstalled_dependencies(debs: &[std::path::PathBuf], ui: &crate::ui::Ui) -> Vec<String> {
    let names: Vec<String> = debs
        .iter()
        .filter_map(|deb| deb_field(deb, "Depends", ui))
        .flat_map(|field| depends_names(&field))
        .collect();
    not_installed(&names, ui)
}

/// Whether these packages ship anything a program might need while
/// running: anything outside `/usr/bin`, `/usr/sbin`, and the
/// documentation a program never loads.
///
/// This is what decides whether a failure from an extracted run can
/// be believed. Extraction installs nothing, so a program that loads
/// a file by absolute path finds it only if the package is already
/// installed -- true of a script looking for its own modules, and
/// equally of a statically linked binary reading its data files.
///
/// When a package ships *only* its programs, there is nothing for
/// extraction to have hidden, and a failure is the package's own:
/// the fixture that sources a `helper.sh` it never packages fails
/// for a reason installing it would not fix.
pub fn ships_runtime_files(packaged: &BTreeSet<String>) -> bool {
    packaged.iter().any(|path| {
        let path = path.trim_start_matches('.');
        !(path.starts_with("/usr/bin/")
            || path.starts_with("/usr/sbin/")
            || path.starts_with("/usr/share/doc/")
            || path.starts_with("/usr/share/man/")
            || path.ends_with('/'))
    })
}

/// dpkg's record of a package's installed file hashes, if it kept
/// one. The name carries the architecture for a multi-arch package.
fn md5sums_record(package: &str) -> Option<std::path::PathBuf> {
    let info = std::path::Path::new("/var/lib/dpkg/info");
    let plain = info.join(format!("{package}.md5sums"));
    if plain.is_file() {
        return Some(plain);
    }
    std::fs::read_dir(info).ok()?.flatten().find_map(|entry| {
        let name = entry.file_name().to_str()?.to_string();
        let matches = name.starts_with(&format!("{package}:")) && name.ends_with(".md5sums");
        matches.then(|| entry.path())
    })
}

/// Whether the extracted package's files are the installed ones.
///
/// `md5sums` lists paths relative to `/`, so checking it from the
/// extraction root compares this package's contents against what
/// dpkg recorded when the installed copy went in. `None` when there
/// is no record to compare against.
fn checksums_match(package: &str, root: &std::path::Path, ui: &crate::ui::Ui) -> Option<bool> {
    let record = md5sums_record(package)?;
    let argv: Vec<String> = ["md5sum", "-c", "--quiet", &record.display().to_string()]
        .iter()
        .map(|s| s.to_string())
        .collect();
    match ui.run_capture(&argv, root) {
        Ok((0, _)) => Some(true),
        Ok(_) => Some(false),
        Err(_) => None,
    }
}

/// How each package being checked compares with what is installed.
fn installed_states(
    debs: &[std::path::PathBuf],
    root: &std::path::Path,
    ui: &crate::ui::Ui,
) -> Vec<Installed> {
    debs.iter()
        .filter_map(|deb| {
            let package = deb_field(deb, "Package", ui)?.trim().to_string();
            let deb_version = deb_field(deb, "Version", ui)?.trim().to_string();
            let argv: Vec<String> = ["dpkg-query", "-W", "-f=${Status}|${Version}", &package]
                .iter()
                .map(|s| s.to_string())
                .collect();
            let installed = match ui.run_capture(&argv, std::path::Path::new(".")) {
                Ok((0, out)) => out
                    .split_once('|')
                    .filter(|(status, _)| dpkg_reports_installed(status))
                    .map(|(_, version)| version.trim().to_string()),
                _ => None,
            };
            Some(installed_state(
                installed.as_deref(),
                &deb_version,
                checksums_match(&package, root, ui),
            ))
        })
        .collect()
}

/// Extract each package and run the programs it installs.
///
/// Running from an extraction rather than an installation is the
/// point: a program that needs a file at an absolute path only finds
/// it if that file was actually packaged, which is the bug being
/// looked for.
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
) -> Result<SmokeReport, String> {
    let programs = installed_programs(packaged);
    if programs.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let missing = uninstalled_dependencies(debs, ui);
    let missing = &missing;
    let root = extract_all(debs, ui)?;
    // Whether a failure here can be believed. The installed copy has
    // to be *this* package, compared by contents rather than by
    // version, since rebuilding while iterating leaves the version
    // alone. Failing that, it is believable only if the package
    // ships nothing a program could have been looking for.
    let states = installed_states(debs, root.path(), ui);
    let conclusive = states.iter().all(|s| *s == Installed::Same) || !ships_runtime_files(packaged);
    let mut results = Vec::new();
    for program in programs {
        let path = root.path().join(program.trim_start_matches('/'));
        if !path.is_file() {
            results.push((program, Smoke::NotRun("not in the extracted tree".into())));
            continue;
        }
        // Every way of asking, in the same order `autopkgtest`
        // probes them. Trying only `--help` reported a working
        // subcommand-style program as broken, which is the kind of
        // confident wrong answer that gets a checker ignored.
        // `timeout` so a program that waits for input cannot hang
        // the check, and because it stays a runnable command.
        let mut outcome = None;
        for verb in crate::autopkgtest::HELP_VERBS {
            let argv: Vec<String> = ["timeout", "10", &path.display().to_string(), verb]
                .iter()
                .map(|s| s.to_string())
                .collect();
            let attempt = match ui.run_capture(&argv, root.path()) {
                Ok((code, output)) => attempt_verdict(code, output.trim(), conclusive, missing),
                Err(e) => Attempt::Refused(Smoke::NotRun(e.to_string())),
            };
            match attempt {
                Attempt::Answered => {
                    outcome = Some(Smoke::Ok);
                    break;
                }
                Attempt::Hopeless(smoke) => {
                    outcome = Some(smoke);
                    break;
                }
                // Keep the first refusal: `--help` is the convention,
                // so its complaint is the one worth quoting.
                Attempt::Refused(smoke) => {
                    outcome.get_or_insert(smoke);
                }
            }
        }
        results.push((
            program,
            outcome.unwrap_or_else(|| Smoke::NotRun("no way to ask it for help".into())),
        ));
    }
    Ok((results, states))
}

/// The findings as a person reads them: worst first, each followed by
/// the command that shows the same thing.
///
/// `colour` styles the severity word and dims the command. Only
/// those: the message itself stays plain so it can be quoted into a
/// bug report without carrying escape codes along.
pub fn render_styled(findings: &[Finding], colour: bool) -> String {
    if findings.is_empty() {
        return "nothing to report\n".to_string();
    }
    let mut sorted: Vec<&Finding> = findings.iter().collect();
    sorted.sort_by_key(|f| f.severity);
    let dim = anstyle::Style::new().fg_color(Some(anstyle::AnsiColor::BrightBlack.into()));
    let mut out = String::new();
    for finding in sorted {
        let (label, message) = (finding.severity.label(), &finding.message);
        match colour {
            true => {
                let s = finding.severity.style();
                out.push_str(&format!(
                    "{}{label}{}: {message}\n",
                    s.render(),
                    s.render_reset()
                ));
            }
            false => out.push_str(&format!("{label}: {message}\n")),
        }
        if let Some(command) = &finding.command {
            match colour {
                true => out.push_str(&format!(
                    "    {}{command}{}\n",
                    dim.render(),
                    dim.render_reset()
                )),
                false => out.push_str(&format!("    {command}\n")),
            }
        }
    }
    out
}

/// [`render_styled`] without colour, which is what a test reads.
pub fn render(findings: &[Finding]) -> String {
    render_styled(findings, false)
}

/// The findings as a document for another tool to read.
pub fn render_json(findings: &[Finding]) -> Result<String, String> {
    serde_json::to_string_pretty(findings).map_err(|e| e.to_string())
}

/// Print the findings, as JSON or as prose.
fn report(findings: &[Finding], json: bool) {
    if json {
        match render_json(findings) {
            Ok(text) => println!("{text}"),
            Err(e) => eprintln!("could not render JSON: {e}"),
        }
        return;
    }
    // anstream drops the codes when stdout is not a terminal or
    // NO_COLOR is set, so this stays safe to pipe.
    anstream::print!("{}", render_styled(findings, true));
}

/// The first line of a tool's output, for quoting in a message.
///
/// Trailing punctuation goes too. A tool that is about to list
/// details ends its first line with a colon -- lua's "module 'x' not
/// found:" -- and quoting that mid-sentence leaves the colon butting
/// against whatever follows.
fn first_line(text: &str) -> &str {
    text.lines()
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches([':', ';', ','])
        .trim_end()
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
    fn a_tree_that_states_no_source_version_is_not_nagged_about_it() {
        // A packaging tree records its upstream version nowhere, so
        // this is a check that does not apply rather than one that
        // was skipped — reporting it would fire on every Debian
        // package dbranch exists for.
        let found = version_findings("0.26.0-1", "unstable", None);
        assert!(found.is_empty(), "{found:?}");
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
    fn a_package_nothing_runs_is_reported_with_the_command_that_fixes_it() {
        let script = "Tests: smoke\nDepends: @\n";
        assert!(autopkgtest_findings(Some(script), None, &[]).is_empty());
        let found = autopkgtest_findings(None, None, &[]);
        assert_eq!(names(&found), ["no-program-test"]);
        // With no package in hand it must not guess a location:
        // `../*.deb` is right only when the build left its output
        // beside the source, and matches an unrelated package
        // otherwise.
        assert_eq!(
            found[0].command.as_deref(),
            Some("dbranch autopkgtest --deb path-to-deb")
        );
        // Given one, it names it.
        let known = autopkgtest_findings(None, None, &["/tmp/foo_1-1_all.deb".to_string()]);
        assert_eq!(
            known[0].command.as_deref(),
            Some("dbranch autopkgtest --deb /tmp/foo_1-1_all.deb")
        );
    }

    #[test]
    fn a_generated_suite_is_named_rather_than_called_nothing() {
        // autopkgtest-pkg-python emits one `import` marked
        // `superficial`; pkg-ruby runs the gem's own suite and skips
        // when there is none. Both are real autopkgtests, so saying
        // "no autopkgtest" would be false -- what is true is that
        // neither runs the installed program.
        let found = autopkgtest_findings(None, Some("autopkgtest-pkg-python"), &[]);
        assert_eq!(names(&found), ["no-program-test"]);
        assert!(
            found[0]
                .message
                .contains("autopkgtest-pkg-python exercises"),
            "{}",
            found[0].message
        );
        assert!(found[0].message.contains("/usr/bin"));
    }

    #[test]
    fn debcargo_stanzas_alone_still_leave_the_programs_untested() {
        // A control file with only Test-Command: stanzas runs cargo
        // tests against the crate, never the binary.
        let generated = "Test-Command: /usr/share/cargo/bin/cargo-auto-test dbranch 0.26.0\n";
        let found = autopkgtest_findings(Some(generated), None, &[]);
        assert_eq!(names(&found), ["no-program-test"]);
    }

    #[test]
    fn the_declared_testsuite_is_read_from_debian_control() {
        let control = "Source: damo\nSection: admin\nTestsuite: autopkgtest-pkg-python\n";
        assert_eq!(
            declared_testsuite(control).as_deref(),
            Some("autopkgtest-pkg-python")
        );
        assert_eq!(declared_testsuite("Source: foo\n"), None);
        // An empty field declares nothing.
        assert_eq!(declared_testsuite("Testsuite:\n"), None);
    }

    #[test]
    fn a_debs_source_is_its_source_field_or_else_its_own_name() {
        // A binary package named differently from its source says so.
        let rust = "Package: librust-sandogasa-build-dev\nSource: rust-sandogasa-build\n";
        assert_eq!(
            deb_source_name(rust).as_deref(),
            Some("rust-sandogasa-build")
        );
        // When they agree, dpkg omits Source entirely.
        assert_eq!(
            deb_source_name("Package: fnlfmt\n").as_deref(),
            Some("fnlfmt")
        );
        // Source may carry a version, which is not part of the name.
        assert_eq!(
            deb_source_name("Package: foo-bin\nSource: foo (1.2-3)\n").as_deref(),
            Some("foo")
        );
        assert_eq!(deb_source_name(""), None);
    }

    #[test]
    fn a_deb_from_another_package_stops_the_run() {
        // The mistake is easy: -C moves the tree, but --deb paths
        // resolve against the current directory, so the two can name
        // different packages. Every package check compares source
        // against .deb, so the wrong one reports all of the source
        // missing -- which reads like a catastrophically broken
        // package rather than like operator error.
        let why = source_mismatch("fnlfmt", Some("rust-sandogasa-build"), "../x.deb")
            .expect("a foreign package must stop this");
        assert!(why.contains("is a package of rust-sandogasa-build"));
        assert!(why.contains("debian/changelog here is fnlfmt"));
        assert!(why.contains("not the --deb paths"), "it says why: {why}");
    }

    #[test]
    fn the_right_deb_passes_and_an_unreadable_one_does_not() {
        assert_eq!(source_mismatch("fnlfmt", Some("fnlfmt"), "../f.deb"), None);
        assert!(
            source_mismatch("fnlfmt", None, "../junk.deb")
                .is_some_and(|w| w.contains("names no package"))
        );
    }

    #[test]
    fn an_override_with_no_pattern_silences_a_tag_everywhere() {
        // fnlfmt's: two vendored files excused by silencing the tag
        // for the whole package, future files included.
        let text = "# bundled from fennel\nvery-long-line-length-in-source-file\n";
        let found = override_findings("debian/source/lintian-overrides", text);
        assert_eq!(names(&found), ["unscoped-lintian-override"]);
        assert_eq!(found[0].severity, Severity::Note);
        assert!(
            found[0]
                .message
                .contains("very-long-line-length-in-source-file")
        );
        assert_eq!(
            found[0].command.as_deref(),
            Some("lintian-explain-tags very-long-line-length-in-source-file")
        );
    }

    #[test]
    fn an_override_carrying_a_pattern_is_already_narrow() {
        let text = "very-long-line-length-in-source-file [fennel/fennel.lua]\n                    package-contains-thing *.pyc\n";
        assert!(override_findings("x", text).is_empty());
        // Comments and blank lines are not overrides.
        assert!(override_findings("x", "# just a note\n\n").is_empty());
    }

    #[test]
    fn a_binary_package_scope_is_not_mistaken_for_a_pattern() {
        // `[pkg] tag` is still a bare tag: the brackets scope it to a
        // binary package, not to a file within one.
        let found = override_findings("debian/foo.lintian-overrides", "[foo] some-tag\n");
        assert_eq!(names(&found), ["unscoped-lintian-override"]);
        assert!(
            found[0].message.contains("some-tag"),
            "{}",
            found[0].message
        );
    }

    #[test]
    fn quilt_knowing_it_applied_them_gets_different_advice() {
        let states = [("0001-x.patch".to_string(), PatchState::AlreadyApplied)];
        // Quilt has them on its stack: popping puts it right.
        let popped = patch_findings(&states, true);
        assert!(popped[0].message.contains("left so by a build"));
        assert_eq!(
            popped[0].command.as_deref(),
            Some("QUILT_PATCHES=debian/patches quilt pop -a")
        );
        // Quilt records nothing, so the content is committed on the
        // branch as well as carried as a patch. Popping would do
        // nothing at all, and advising it would be worse than silence.
        let committed = patch_findings(&states, false);
        assert!(
            committed[0].message.contains("committed on this branch"),
            "{}",
            committed[0].message
        );
        assert_eq!(
            committed[0].command.as_deref(),
            Some("QUILT_PATCHES=debian/patches quilt applied"),
            "the command shows the claim rather than acting on it"
        );
    }

    #[test]
    fn a_patch_header_needs_to_say_what_and_whence() {
        let bare = "--- a/x\n+++ b/x\n";
        let found = dep3_findings("0001-x.patch", bare);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found.iter().all(|f| f.check == "patch-header-not-dep3"));
        assert!(found[0].message.contains("what it does"));
        assert!(found[1].message.contains("where it came from"));
    }

    #[test]
    fn a_header_that_keeps_going_with_dots_is_fine() {
        let text = "Description: Fix it\n .\n More prose\nOrigin: upstream\n--- a/x\n";
        assert!(dep3_findings("0001-x.patch", text).is_empty());
    }

    #[test]
    fn a_package_built_from_another_changelog_entry_is_flagged() {
        let debs = [("../foo_0.3.2-1_all.deb".to_string(), "0.3.2-1".to_string())];
        let found = stale_build_findings("0.4.0-1", &debs);
        assert_eq!(names(&found), ["stale-build"]);
        assert!(found[0].message.contains("built as 0.3.2-1"));
        assert!(found[0].message.contains("is at 0.4.0-1"));
        // The matching build says nothing.
        let same = [("../foo_0.4.0-1_all.deb".to_string(), "0.4.0-1".to_string())];
        assert!(stale_build_findings("0.4.0-1", &same).is_empty());
    }

    #[test]
    fn quilts_stack_is_read_rather_than_the_directorys_existence() {
        let dir = fixture("1.2.3-1");
        std::fs::create_dir_all(dir.path().join(".pc")).unwrap();
        // A .pc directory with no stack: quilt has nothing applied,
        // which is the state that made `quilt pop -a` useless advice.
        assert!(!quilt_records_applied(dir.path()));
        std::fs::write(dir.path().join(".pc/applied-patches"), "\n").unwrap();
        assert!(!quilt_records_applied(dir.path()), "blank is still none");
        std::fs::write(dir.path().join(".pc/applied-patches"), "0001-x.patch\n").unwrap();
        assert!(quilt_records_applied(dir.path()));
    }

    #[test]
    fn a_pc_directory_counts_as_ignored_when_it_is_not_there() {
        let dir = fixture("1.2.3-1");
        // Nothing to ignore, so nothing to say.
        assert!(pc_is_ignored(dir.path()));
        std::fs::create_dir_all(dir.path().join(".pc")).unwrap();
        assert!(!pc_is_ignored(dir.path()));
        // Either file counts; gbp's script writes the git one.
        std::fs::create_dir_all(dir.path().join(".git/info")).unwrap();
        std::fs::write(dir.path().join(".git/info/exclude"), "# stuff\n.pc/\n").unwrap();
        assert!(pc_is_ignored(dir.path()));
    }

    #[test]
    fn override_files_are_found_by_both_the_names_they_take() {
        let dir = fixture("1.2.3-1");
        let debian = dir.path().join("debian");
        std::fs::create_dir_all(debian.join("source")).unwrap();
        std::fs::write(debian.join("source/lintian-overrides"), "tag-a\n").unwrap();
        std::fs::write(debian.join("foo.lintian-overrides"), "tag-b\n").unwrap();
        std::fs::write(debian.join("control"), "Source: foo\n").unwrap();
        let found = override_files(&debian);
        let names: Vec<&str> = found.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            names,
            [
                "debian/foo.lintian-overrides",
                "debian/source/lintian-overrides"
            ]
        );
    }

    #[test]
    fn a_dep3_header_stops_at_the_diff_however_it_starts() {
        assert_eq!(
            patch_header("Subject: x\n--- a/f\n+++ b/f\n"),
            "Subject: x\n"
        );
        assert_eq!(
            patch_header("Subject: x\ndiff --git a/f b/f\n"),
            "Subject: x\n"
        );
        // git format-patch's bare three dashes before the diffstat.
        assert_eq!(patch_header("Subject: x\n---\n f | 2 +-\n"), "Subject: x\n");
        // No diff at all: the whole thing is header.
        assert_eq!(patch_header("Subject: x\n"), "Subject: x\n");
    }

    #[test]
    fn a_quilt_package_without_unapply_patches_is_flagged() {
        // fnlfmt's state: 3.0 (quilt) with patches and no
        // local-options, so dpkg-source applies them at build and
        // never puts them back.
        let found = unapply_findings(Some("3.0 (quilt)\n"), true, None, true);
        assert_eq!(names(&found), ["patches-left-applied-after-build"]);
        assert_eq!(found[0].severity, Severity::Warning);
        // A command that can be read and run, not a path to a
        // script that may not be installed.
        assert!(
            found[0]
                .command
                .as_deref()
                .is_some_and(|c| c.contains("echo unapply-patches")),
            "{:?}",
            found[0].command
        );
    }

    #[test]
    fn a_package_that_already_sets_it_is_quiet() {
        let opts = "unapply-patches\nabort-on-upstream-changes\n";
        assert!(unapply_findings(Some("3.0 (quilt)\n"), true, Some(opts), true).is_empty());
        // dpkg's own long spelling counts too.
        assert!(
            unapply_findings(
                Some("3.0 (quilt)\n"),
                true,
                Some("--unapply-patches\n"),
                true
            )
            .is_empty()
        );
    }

    #[test]
    fn the_question_only_arises_for_a_quilt_package_with_patches() {
        // Nothing to apply, so nothing is left applied.
        assert!(unapply_findings(Some("3.0 (quilt)\n"), false, None, true).is_empty());
        // A native package has no patch system at all.
        assert!(unapply_findings(Some("3.0 (native)\n"), true, None, true).is_empty());
        assert!(unapply_findings(None, true, None, true).is_empty());
    }

    #[test]
    fn an_unignored_pc_directory_is_its_own_note() {
        let found = unapply_findings(
            Some("3.0 (quilt)\n"),
            true,
            Some("unapply-patches\n"),
            false,
        );
        assert_eq!(names(&found), ["pc-directory-not-ignored"]);
        assert_eq!(found[0].severity, Severity::Note);
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
    fn a_source_only_run_reports_only_what_is_wrong() {
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
        // The versions agree, so what is left is the two things this
        // tree genuinely lacks: anything that runs the installed
        // programs, and a built package to check against.
        // Only what is actually wrong: running without a built
        // package is a mode, so nothing reports its absence.
        assert_eq!(names(&found), ["no-program-test"]);
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
        // Naming the absolute path matters: running from the wrong
        // directory is how this is reached, and a bare "." would not
        // tell you which directory that was.
        assert!(
            err.contains(&dir.path().display().to_string()),
            "the error names where it looked: {err}"
        );
        assert!(err.contains("-C"), "and how to point it elsewhere: {err}");
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
    fn patches_already_in_the_tree_collapse_into_one_note() {
        // fnlfmt's tree is gbp's: patches applied, no .pc to notice
        // it by. Forward application fails exactly as a stale patch
        // does, so a blocker here was crying wolf -- and one note per
        // patch would bury a package carrying twenty.
        let states = [
            ("0001-one.patch".to_string(), PatchState::AlreadyApplied),
            ("0002-two.patch".to_string(), PatchState::AlreadyApplied),
        ];
        let found = patch_findings(&states, true);
        assert_eq!(names(&found), ["patches-applied-in-tree"]);
        assert_eq!(found[0].severity, Severity::Note);
        assert!(!has_blocker(&found));
        assert!(
            found[0].message.starts_with("2 of 2 patches"),
            "{}",
            found[0].message
        );
        // It must admit what it could not settle.
        assert!(found[0].message.contains("look alike"));
    }

    #[test]
    fn a_patch_that_applies_in_neither_direction_is_still_a_blocker() {
        let states = [
            ("good.patch".to_string(), PatchState::Applies),
            (
                "stale.patch".to_string(),
                PatchState::Broken("1 out of 1 hunk FAILED".into()),
            ),
        ];
        let found = patch_findings(&states, true);
        assert_eq!(names(&found), ["patch-does-not-apply"]);
        assert!(has_blocker(&found));
        assert_eq!(
            found[0].command.as_deref(),
            Some("patch -p1 --dry-run -f -i debian/patches/stale.patch")
        );
    }

    #[test]
    fn a_depends_field_yields_bare_package_names() {
        assert_eq!(depends_names("fennel, lua5.4"), ["fennel", "lua5.4"]);
        // Versions and architecture qualifiers are not part of a name.
        assert_eq!(
            depends_names("libc6 (>= 2.34), python3:any"),
            ["libc6", "python3:any"]
        );
        // Either branch of an alternative can satisfy it, so both are
        // worth asking dpkg about.
        assert_eq!(depends_names("lua5.4 | lua5.3"), ["lua5.4", "lua5.3"]);
        assert!(depends_names("").is_empty());
    }

    #[test]
    fn the_not_run_note_names_what_the_host_is_missing() {
        let note = missing_dependency_note(
            "env: 'lua5.4': No such file or directory",
            &["fennel".to_string(), "lua5.4".to_string()],
        );
        assert!(note.contains("fennel, lua5.4 are not installed here"));
        // It has to say the verdict is about the machine, not the
        // package, and where a real answer comes from.
        assert!(note.contains("autopkgtest installs a package's dependencies"));
        // One missing dependency reads as one thing, not two.
        let one = missing_dependency_note("env: 'x'", &["lua5.4".to_string()]);
        assert!(one.contains("lua5.4 is not installed here"), "{one}");
    }

    #[test]
    fn with_nothing_identifiably_missing_the_note_still_explains_itself() {
        let note = missing_dependency_note("env: 'x': No such file", &[]);
        assert!(note.contains("install the package's dependencies"));
        assert!(!note.contains("has not got"));
    }

    #[test]
    fn a_package_of_programs_alone_hides_nothing_from_an_extraction() {
        // The fixture that sources a helper.sh it never packages:
        // nothing was left behind by extracting, so its failure is
        // its own and installing would not fix it.
        let packaged = BTreeSet::from([
            "./usr/bin/foo".to_string(),
            "./usr/share/doc/foo/copyright".to_string(),
            "./usr/share/man/man1/foo.1.gz".to_string(),
        ]);
        assert!(!ships_runtime_files(&packaged));
    }

    #[test]
    fn a_package_shipping_modules_or_data_can_hide_them() {
        // fnlfmt ships its own Lua module, which an extracted copy
        // looks for on the host and does not find -- a failure about
        // this machine, not about the package.
        let packaged = BTreeSet::from([
            "./usr/bin/fnlfmt".to_string(),
            "./usr/share/lua/5.4/fnlfmt.lua".to_string(),
        ]);
        assert!(ships_runtime_files(&packaged));
        // A statically linked binary with data files is the same
        // case, which is why this does not ask about interpreters.
        let with_data = BTreeSet::from([
            "./usr/bin/thing".to_string(),
            "./usr/share/thing/table.bin".to_string(),
        ]);
        assert!(ships_runtime_files(&with_data));
    }

    #[test]
    fn a_quoted_tool_message_does_not_trail_into_the_sentence() {
        // lua ends its first line with a colon before listing the
        // paths it searched, which read badly before an em dash.
        assert_eq!(
            first_line("lua5.4: /usr/bin/x:4: module 'x' not found:\n  no field"),
            "lua5.4: /usr/bin/x:4: module 'x' not found"
        );
        assert_eq!(first_line("plain message"), "plain message");
        assert_eq!(first_line(""), "");
    }

    #[test]
    fn the_programs_a_package_installs_are_its_bin_entries() {
        let packaged = BTreeSet::from([
            "./usr/bin/foo".to_string(),
            "./usr/sbin/bard".to_string(),
            "./usr/share/lua/5.4/foo.lua".to_string(),
            "./usr/share/doc/foo/copyright".to_string(),
        ]);
        // Sorted as the set holds them: ./usr/bin before ./usr/sbin.
        assert_eq!(
            installed_programs(&packaged),
            ["/usr/bin/foo", "/usr/sbin/bard"]
        );
    }

    #[test]
    fn the_json_document_carries_the_slug_and_the_command() {
        let findings = vec![
            Finding::new("a-check", Severity::Blocker, "it broke".into())
                .with_command("run this".into()),
        ];
        let text = render_json(&findings).expect("findings serialize");
        assert!(text.contains("\"check\": \"a-check\""), "{text}");
        assert!(text.contains("\"severity\": \"blocker\""), "{text}");
        assert!(text.contains("\"command\": \"run this\""), "{text}");
        // A finding with no command omits the key rather than
        // carrying a null for a reader to special-case.
        let bare = render_json(&[Finding::new("b", Severity::Note, "x".into())]).unwrap();
        assert!(!bare.contains("command"), "{bare}");
    }

    #[test]
    fn a_deb_that_is_not_there_stops_the_run_naming_where_it_looked() {
        let dir = fixture("1.2.3-1");
        let opts = CheckOptions {
            repo: dir.path().to_path_buf(),
            debs: vec![std::path::PathBuf::from("definitely-not-here.deb")],
            json: false,
        };
        let err = run(&opts, &silent()).expect_err("a missing package stops this");
        assert!(err.contains("no such file"), "{err}");
        // Absolute, because the path was read against the current
        // directory and not against -C, which is the whole trap.
        assert!(err.starts_with('/'), "it says where it looked: {err}");
    }

    #[test]
    fn a_rebuild_of_the_same_version_is_caught_by_contents_alone() {
        // The trap this exists for: iterating on packaging produces
        // 0.4.0-1 again and again, so versions match while the files
        // do not, and the smoke test would quietly exercise the copy
        // installed hours ago.
        let state = installed_state(Some("0.4.0-1"), "0.4.0-1", Some(false));
        let Installed::Differs(why) = &state else {
            panic!("differing contents must be noticed: {state:?}");
        };
        assert!(why.contains("only their contents reveal"), "{why}");
        let found = install_findings(&state, "sudo apt install ./x.deb");
        assert_eq!(names(&found), ["installed-copy-differs"]);
        assert_eq!(found[0].severity, Severity::Warning);
    }

    #[test]
    fn matching_contents_are_the_package_being_checked() {
        assert_eq!(
            installed_state(Some("0.4.0-1"), "0.4.0-1", Some(true)),
            Installed::Same
        );
        assert!(install_findings(&Installed::Same, "x").is_empty());
        assert_eq!(installed_state(None, "0.4.0-1", None), Installed::Absent);
        assert!(install_findings(&Installed::Absent, "x").is_empty());
    }

    #[test]
    fn without_a_checksum_record_the_versions_are_the_fallback() {
        // A package shipping no md5sums can still be told apart by
        // version, which is weaker but better than nothing.
        assert_eq!(
            installed_state(Some("0.4.0-1"), "0.4.0-1", None),
            Installed::Same
        );
        let Installed::Differs(why) = installed_state(Some("0.3.2-1"), "0.4.0-1", None) else {
            panic!("a different version is a different package");
        };
        assert!(why.contains("0.3.2-1 is installed"), "{why}");
        assert!(why.contains("this package is 0.4.0-1"), "{why}");
    }

    #[test]
    fn a_program_that_answers_ends_the_asking() {
        assert_eq!(
            attempt_verdict(0, "usage: ...", true, &[]),
            Attempt::Answered
        );
    }

    #[test]
    fn command_not_found_is_hopeless_and_blames_the_host() {
        // 127 from an extracted package means its dependencies are
        // not installed here, so no other verb will fare better.
        let Attempt::Hopeless(Smoke::NotRun(why)) = attempt_verdict(
            127,
            "env: 'lua5.4': No such file or directory",
            false,
            &["lua5.4".to_string()],
        ) else {
            panic!("a missing interpreter is hopeless, not a refusal");
        };
        assert!(why.contains("lua5.4 is not installed here"), "{why}");
    }

    #[test]
    fn a_refusal_is_the_packages_fault_only_when_it_can_be_believed() {
        // The fixture that ships only its program: nothing was
        // hidden by extracting, so its failure is its own.
        let Attempt::Refused(Smoke::Failed { code, .. }) =
            attempt_verdict(2, "cannot open helper.sh", true, &[])
        else {
            panic!("a conclusive refusal is a failure");
        };
        assert_eq!(code, 2);

        // fnlfmt, which ships a module an extracted copy cannot see.
        let Attempt::Refused(Smoke::NotRun(why)) =
            attempt_verdict(1, "module 'fnlfmt' not found:", false, &[])
        else {
            panic!("an inconclusive refusal is not a failure");
        };
        assert!(why.contains("says nothing about the package"), "{why}");
        // The quoted line loses its trailing colon on the way in.
        assert!(why.contains("module 'fnlfmt' not found —"), "{why}");
    }

    #[test]
    fn dpkg_status_is_read_for_the_installed_state_alone() {
        assert!(dpkg_reports_installed("install ok installed"));
        assert!(!dpkg_reports_installed("deinstall ok config-files"));
        assert!(!dpkg_reports_installed(""));
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
        let found = smoke_findings(&results, "sudo apt install ./x.deb");
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
        let found = smoke_findings(&results, "sudo apt install ./x.deb");
        assert_eq!(names(&found), ["binary-not-run"]);
        assert_eq!(found[0].severity, Severity::Note);
        assert!(!has_blocker(&found), "an untried binary is not a blocker");
    }
}
