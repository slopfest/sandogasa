// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The `bundled()` Provides of a package that bundles upstream's git
//! submodules, read from upstream's repository at the release tag
//! (`bundled-provides`, #109).
//!
//! A spec restates each bundled library's version at every bump, and
//! by hand those lines drift: et's 7.1.1 spec said sentry-native 0.6.0
//! where the pin was 0.15.3. The pins are all in upstream's repository:
//! the tree at the tag holds each submodule's commit, `.gitmodules` its
//! URL, and the submodule's own history says which tag that commit is,
//! or follows.
//!
//! Everything is read with `git`, not a forge's API: a bare clone of a
//! submodule's history answers the exact tag, the nearest tag and the
//! commit date for any forge, with no rate limit, and is kept under
//! ebranch's cache so a later run asks the network only for what the
//! cache lacks.
//!
//! Each bundled component's license files (`LICENSE`, `COPYING*`, …, by
//! the names askalono's own crawl looks for) are read from the same
//! clones, at the pinned commit, and identified by `askalono` (Fedora's
//! `askalono-cli`, which go-vendor-tools uses for Go packages), in one
//! batch. Without it the Provides are still reported, the licenses not.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

/// Where a bundled library stands in the package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    /// Shipped as upstream bundles it: it gets a Provides.
    Bundled,
    /// Deleted in the spec's `%prep`, so not shipped.
    Removed,
    /// Replaced in `%prep` by the system's copy (a symlink).
    Replaced,
}

/// One license file of a bundled component, and what it holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LicenseFile {
    /// Its path in the component's tree: `LICENSE`, `COPYING.txt`.
    pub path: String,
    /// The SPDX expression askalono identified, `None` when it did not
    /// identify one or was not run.
    pub license: Option<String>,
}

/// A bundled component's licenses, from its license files.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Licenses {
    pub files: Vec<LicenseFile>,
    /// Every license identified in them, sorted and joined with ` AND `
    /// (`MIT`, `BSD-3-Clause AND MIT`); `None` when none was.
    pub expression: Option<String>,
    /// Why the files could not be read, when they could not.
    pub error: Option<String>,
    /// The files' contents, in `files`' order, until askalono has read
    /// them.
    #[serde(skip)]
    texts: Vec<Vec<u8>>,
}

/// One submodule at the tag, and what its pin is.
#[derive(Debug, Clone, Serialize)]
pub struct Pin {
    /// The name its Provides uses: the spec's, when it has a line for
    /// this one, else the directory's.
    pub name: String,
    /// Its path in upstream's tree.
    pub path: String,
    /// Its repository, `None` when `.gitmodules` does not name one.
    pub url: Option<String>,
    pub commit: String,
    /// The Fedora version of the pin (`4.3.0`, `1.0.1^20200819git424b969`,
    /// `0^20140926git9a42ec1`), `None` when it could not be read.
    pub version: Option<String>,
    /// Why the version is missing, when it is.
    pub error: Option<String>,
    pub state: State,
    /// Whether upstream's build files at the tag mention its directory.
    /// One they do not is likely bundled but never built.
    pub referenced: bool,
    /// Its license files at the pinned commit.
    pub licenses: Licenses,
}

/// A `bundled()` line of the spec that the pins do not bear out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Disagreement {
    pub name: String,
    /// What the spec says, `None` when it has no line for a pin.
    pub spec: Option<String>,
    /// What the pin says, `None` when no submodule has that name.
    pub pin: Option<String>,
}

/// What `bundled-provides` found.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub repo: String,
    pub tag: String,
    pub pins: Vec<Pin>,
    /// Directories among the bundled ones that no submodule pins: code
    /// copied in, to version by hand.
    pub unpinned: Vec<String>,
    /// The license files of each `unpinned` directory, at the tag.
    pub unpinned_licenses: BTreeMap<String, Licenses>,
    /// Whether askalono identified the license files: false when it is
    /// not installed, failed, or had none to read, and then no
    /// `license` is set.
    pub licenses_identified: bool,
    /// The spec's lines that disagree with the pins, when a spec was read.
    pub disagreements: Vec<Disagreement>,
    /// Every bundled directory's name, sorted as `LC_ALL=C sort -f`
    /// does, for a `%prep` check that fails when the set changes: as
    /// the spec's `%prep` leaves them when a spec was read, as the
    /// release ships them otherwise.
    pub directories: Vec<String>,
    /// Whether `directories` is what `%prep` leaves.
    pub after_prep: bool,
}

/// What a spec says about its bundled libraries: Source0, the
/// `bundled()` Provides with their versions, and the directories its
/// `%prep` deletes or points elsewhere, by directory name.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SpecFacts {
    pub source0: Option<String>,
    /// (name, version) per line, in the spec's order.
    pub provides: Vec<(String, String)>,
    /// Directory names `%prep` deletes (`rm -rf …/Catch2`).
    pub removed: Vec<String>,
    /// Directory names `%prep` links to a system copy (`ln -s … …/easyloggingpp/…`).
    pub replaced: Vec<String>,
}

/// What to read.
pub struct Options {
    /// A spec to read upstream, the tag and the current Provides from.
    pub spec: Option<PathBuf>,
    /// Upstream's repository, instead of the spec's Source0.
    pub repo: Option<String>,
    /// The tag, for a spec whose Source0 names none, or without a spec.
    pub tag: Option<String>,
    pub json: bool,
    pub verbose: bool,
}

/// The version a tag names: from its first digit that starts a word,
/// at the start, after a separator or after a `v` that does
/// (`v4.3.0`, `release-4.3.0`, `et-v7.1.1` and `bzip2-1.0.8` all give
/// the number), with `-` before a letter as RPM's `~` (`1.0-rc1` →
/// `1.0~rc1`) and any other `-` or `_` as `.`, since RPM versions take
/// neither. `None` for a tag with no such digit, which names no version.
pub fn tag_version(tag: &str) -> Option<String> {
    let chars: Vec<char> = tag.chars().collect();
    let sep = |c: char| matches!(c, '-' | '_' | '.' | '/');
    let starts_word = |i: usize| i == 0 || sep(chars[i - 1]);
    let start = (0..chars.len()).find(|&i| {
        chars[i].is_ascii_digit()
            && (starts_word(i) || (matches!(chars[i - 1], 'v' | 'V') && starts_word(i - 1)))
    })?;
    let mut out = String::with_capacity(chars.len() - start);
    for (i, c) in chars.iter().enumerate().skip(start) {
        match c {
            '-' | '_' => {
                let next_alpha = chars.get(i + 1).is_some_and(|n| n.is_ascii_alphabetic());
                out.push(if next_alpha { '~' } else { '.' });
            }
            c => out.push(*c),
        }
    }
    Some(out)
}

/// The Fedora version of a pinned commit (the snapshot rule of the
/// packaging guidelines): the exact tag's version when the commit is
/// one, else the nearest earlier tag's with `^<date>git<short>`, else
/// `0^<date>git<short>`. `date` is the commit's, `YYYYMMDD`, in UTC.
pub fn pin_version(exact: Option<&str>, nearest: Option<&str>, date: &str, commit: &str) -> String {
    if let Some(v) = exact.and_then(tag_version) {
        return v;
    }
    let short: String = commit.chars().take(7).collect();
    let base = nearest
        .and_then(tag_version)
        .unwrap_or_else(|| "0".to_string());
    format!("{base}^{date}git{short}")
}

/// The submodules `.gitmodules` declares, as (path, url), paths
/// without quotes or a trailing slash.
pub fn parse_gitmodules(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut path: Option<String> = None;
    let mut url: Option<String> = None;
    let mut flush = |path: &mut Option<String>, url: &mut Option<String>| {
        if let (Some(p), Some(u)) = (path.take(), url.take()) {
            out.push((p, u));
        }
    };
    let clean = |v: &str| v.trim().trim_matches('"').trim_end_matches('/').to_string();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            flush(&mut path, &mut url);
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "path" => path = Some(clean(value)),
            "url" => url = Some(clean(value)),
            _ => {}
        }
    }
    flush(&mut path, &mut url);
    out
}

/// A submodule URL relative to upstream's (`../sole.git`) made whole.
pub fn resolve_url(base: &str, url: &str) -> String {
    if !url.starts_with("../") && !url.starts_with("./") {
        return url.to_string();
    }
    let mut parts: Vec<&str> = base.trim_end_matches('/').split('/').collect();
    for seg in url.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Upstream's repository and the tag a spec's Source0 names, for the
/// forms forges serve release archives at: GitHub's
/// `/archive/<tag>/…`, `/archive/refs/tags/<tag>.tar.gz`,
/// `/archive/<tag>.tar.gz` and `/releases/download/<tag>/…`, and
/// GitLab's and Forgejo's `/-/archive/<tag>/…` and `/archive/<tag>.tar.gz`.
pub fn repo_and_tag_from_source(source: &str) -> Option<(String, String)> {
    let source = source.split('#').next().unwrap_or(source);
    let (repo, rest) = if let Some((repo, rest)) = source.split_once("/-/archive/") {
        (repo, rest)
    } else if let Some((repo, rest)) = source.split_once("/releases/download/") {
        (repo, rest)
    } else {
        source.split_once("/archive/")?
    };
    let rest = rest.strip_prefix("refs/tags/").unwrap_or(rest);
    let tag = match rest.split_once('/') {
        Some((tag, _)) => tag.to_string(),
        None => {
            let mut t = rest;
            for ext in [".tar.gz", ".tar.xz", ".tar.bz2", ".tgz", ".zip"] {
                if let Some(s) = t.strip_suffix(ext) {
                    t = s;
                    break;
                }
            }
            t.to_string()
        }
    };
    if tag.is_empty() || !repo.contains("://") {
        return None;
    }
    Some((repo.to_string(), tag))
}

/// Read [`SpecFacts`] from a spec's text, with its macros expanded
/// (`rpmspec -P`) or not: only Source0's needs to be. `%prep` lines are
/// read with their continuations joined and one level of braces
/// expanded (`rm -rf external/{Catch2,json}`).
pub fn parse_spec(text: &str) -> SpecFacts {
    const SECTIONS: [&str; 10] = [
        "prep",
        "build",
        "install",
        "check",
        "files",
        "changelog",
        "description",
        "package",
        "generate_buildrequires",
        "conf",
    ];
    let mut facts = SpecFacts::default();
    let mut section = String::new();
    // A `for <var> in <words>; do` loop in %prep, while in its body.
    let mut prep_loop: Option<(String, Vec<String>)> = None;
    for line in join_continuations(text) {
        let trimmed = line.trim();
        if let Some(name) = trimmed.strip_prefix('%')
            && let Some(word) = name.split_whitespace().next()
            && SECTIONS.contains(&word)
        {
            section = word.to_string();
            continue;
        }
        if section.is_empty() || section == "package" {
            let lower = trimmed.to_ascii_lowercase();
            if (lower.starts_with("source0:") || lower.starts_with("source:"))
                && facts.source0.is_none()
            {
                facts.source0 = trimmed.split_once(':').map(|(_, v)| v.trim().to_string());
            }
            if lower.starts_with("provides:") {
                let value = trimmed.split_once(':').map(|(_, v)| v.trim()).unwrap_or("");
                for item in value.split(',') {
                    if let Some(p) = parse_bundled(item.trim()) {
                        facts.provides.push(p);
                    }
                }
            }
        }
        if section == "prep" {
            read_prep_shell(trimmed, &mut prep_loop, &mut facts);
        }
    }
    facts
}

/// Compare the spec's `bundled()` lines with the pins, by name
/// regardless of case. A line for a directory copied in without a
/// submodule (`unpinned`, by path) is the packager's own and not
/// questioned.
pub fn disagreements(
    provides: &[(String, String)],
    pins: &[Pin],
    unpinned: &[String],
) -> Vec<Disagreement> {
    let shipped: Vec<&Pin> = pins.iter().filter(|p| p.state == State::Bundled).collect();
    let mut out = Vec::new();
    for p in &shipped {
        let Some(version) = &p.version else { continue };
        match provides
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(&p.name))
        {
            Some((_, v)) if v == version => {}
            Some((_, v)) => out.push(Disagreement {
                name: p.name.clone(),
                spec: Some(v.clone()),
                pin: Some(version.clone()),
            }),
            None => out.push(Disagreement {
                name: p.name.clone(),
                spec: None,
                pin: Some(version.clone()),
            }),
        }
    }
    let unpinned_names: Vec<&str> = unpinned.iter().map(|p| dir_name(p)).collect();
    for (n, v) in provides {
        let known = shipped.iter().any(|p| p.name.eq_ignore_ascii_case(n))
            || unpinned_names.iter().any(|u| u.eq_ignore_ascii_case(n));
        if !known {
            out.push(Disagreement {
                name: n.clone(),
                spec: Some(v.clone()),
                pin: None,
            });
        }
    }
    out
}

/// Sort as `LC_ALL=C sort -f` does: by bytes, ASCII letters folded to
/// upper case.
pub fn sort_f(names: &mut [String]) {
    names.sort_by_key(|n| n.to_ascii_uppercase());
}

/// Whether a file named `name` (no directory) is a license file, by the
/// names askalono's crawl looks for, as go-vendor-tools takes them:
/// `LICENSE`, `LICENCE.txt`, `COPYING-LGPL`, `COPYRIGHT`, `UNLICENSE`,
/// `LICENSE_1_0.txt` (Boost's, which askalono's crawl misses),
/// `foo-LICENSE.md`, `gpl-3.0.txt`, `APACHE-2.0.txt`, `MIT-LICENSE`.
/// Source files named like one (`license.c`, `license.py`) are code.
pub fn is_license_file(name: &str) -> bool {
    // Code and build files: `license.c`, `license-check.sh`,
    // `LICENSE-header.cmake`.
    const CODE: &[&str] = &[
        "ac", "am", "bash", "bat", "c", "cc", "cmake", "cpp", "cs", "cxx", "go", "h", "hh", "hpp",
        "java", "js", "kt", "lua", "m", "m4", "mk", "mm", "php", "pl", "ps1", "py", "rb", "rs",
        "sh", "swift", "ts",
    ];
    if name
        .rsplit_once('.')
        .is_some_and(|(_, ext)| CODE.contains(&ext.to_ascii_lowercase().as_str()))
    {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    // The name, or the name followed by `.`, `-` or `_` and anything.
    let named = |stem: &str| {
        lower
            .strip_prefix(stem)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(['.', '-', '_']))
    };
    // A license's own name with its version: `apache-2.0.txt`.
    let versioned = |stem: &str| {
        lower
            .strip_prefix(stem)
            .is_some_and(|rest| rest.chars().any(|c| c.is_ascii_digit()))
    };
    [
        "copying",
        "copyright",
        "eula",
        "licence",
        "license",
        "unlicence",
        "unlicense",
    ]
    .iter()
    .any(|s| named(s))
        || ["agpl", "gpl", "lgpl"].iter().any(|s| {
            lower
                .strip_prefix(s)
                .is_some_and(|r| r.starts_with(['.', '-']))
        })
        || [
            "agpl-", "apache-", "bsd-", "gfdl-", "gnu-", "gpl-", "lgpl-", "mit-", "mpl-", "ofl-",
        ]
        .iter()
        .any(|s| versioned(s))
        || lower.starts_with("cc-by-")
        || lower.contains(".licen")
        || lower.contains("-licen")
        || lower.ends_with("-bsd.txt")
}

/// The license one line of `askalono --format json identify --batch`
/// gives, with the path it names: every license a file contains, joined
/// with ` AND `, else the one it is, else `None` (not identified, or an
/// error for that file).
pub fn parse_askalono_line(line: &str) -> Option<(String, Option<String>)> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let path = v.get("path")?.as_str()?.trim_end_matches('\n').to_string();
    let result = v.get("result");
    let mut containing: Vec<&str> = result
        .and_then(|r| r.get("containing"))
        .and_then(|c| c.as_array())
        .into_iter()
        .flatten()
        .filter_map(|e| e.get("license")?.get("name")?.as_str())
        .collect();
    containing.sort_unstable();
    containing.dedup();
    let license = if containing.is_empty() {
        result
            .and_then(|r| r.get("license"))
            .and_then(|l| l.get("name"))
            .and_then(|n| n.as_str())
            .map(str::to_string)
    } else {
        Some(containing.join(" AND "))
    };
    Some((path, license))
}

/// The expression for a component's files: every license identified,
/// split on ` AND `, sorted and joined again; `None` when none was.
pub fn license_expression(files: &[LicenseFile]) -> Option<String> {
    let mut all: Vec<&str> = files
        .iter()
        .filter_map(|f| f.license.as_deref())
        .flat_map(|l| l.split(" AND "))
        .collect();
    all.sort_unstable();
    all.dedup();
    (!all.is_empty()).then(|| all.join(" AND "))
}

/// What a component's licenses come to, as one line: `MIT, from
/// LICENSE`, with the files not identified named, or why there are
/// none.
pub fn describe_licenses(l: &Licenses, identified: bool) -> String {
    if let Some(e) = &l.error {
        return format!("license files not read: {e}");
    }
    if l.files.is_empty() {
        return "no license file found".to_string();
    }
    let paths = |files: Vec<&LicenseFile>| {
        files
            .iter()
            .map(|f| f.path.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !identified {
        return format!("license in {}", paths(l.files.iter().collect()));
    }
    let (known, unknown): (Vec<&LicenseFile>, Vec<&LicenseFile>) =
        l.files.iter().partition(|f| f.license.is_some());
    let mut out = match &l.expression {
        Some(e) => format!("{e}, from {}", paths(known)),
        None => String::new(),
    };
    if !unknown.is_empty() {
        if !out.is_empty() {
            out.push_str("; ");
        }
        out.push_str(&format!("{} not identified", paths(unknown)));
    }
    out
}

/// The report as text.
pub fn render(report: &Report) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let shipped: Vec<&Pin> = report
        .pins
        .iter()
        .filter(|p| p.state == State::Bundled)
        .collect();
    let _ = writeln!(
        out,
        "{} at {}: {} submodule(s), {} bundled",
        report.repo,
        report.tag,
        report.pins.len(),
        shipped.len()
    );
    let _ = writeln!(out);
    for p in &shipped {
        // A pin that could not be read is said once, below.
        if p.version.is_some() || p.licenses.error.is_none() {
            let _ = writeln!(
                out,
                "# {}: {}",
                p.name,
                describe_licenses(&p.licenses, report.licenses_identified)
            );
        }
        match &p.version {
            Some(v) => {
                let _ = writeln!(out, "Provides:       bundled({}) = {v}", p.name);
            }
            None => {
                let _ = writeln!(
                    out,
                    "# bundled({}): {}",
                    p.name,
                    p.error.as_deref().unwrap_or("version unknown")
                );
            }
        }
    }
    let gone: Vec<String> = report
        .pins
        .iter()
        .filter(|p| p.state != State::Bundled)
        .map(|p| {
            format!(
                "{} ({})",
                p.name,
                if p.state == State::Removed {
                    "removed in %prep"
                } else {
                    "replaced by the system copy in %prep"
                }
            )
        })
        .collect();
    // One per line: these run long, and a list across the screen is
    // hard to read.
    let bullets = |items: &[String]| {
        items
            .iter()
            .map(|i| format!("  - {i}\n"))
            .collect::<String>()
    };
    if !gone.is_empty() {
        let _ = write!(out, "\nNot shipped, so no Provides:\n{}", bullets(&gone));
    }
    let unbuilt: Vec<String> = shipped
        .iter()
        .filter(|p| !p.referenced)
        .map(|p| p.name.clone())
        .collect();
    if !unbuilt.is_empty() {
        let _ = write!(
            out,
            "\nNot mentioned in upstream's build files at {}, so possibly never built \
             (remove in %prep if so):\n{}",
            report.tag,
            bullets(&unbuilt)
        );
    }
    if !report.unpinned.is_empty() {
        let dirs: Vec<String> = report
            .unpinned
            .iter()
            .map(|d| match report.unpinned_licenses.get(d) {
                Some(l) => format!("{d} ({})", describe_licenses(l, report.licenses_identified)),
                None => d.clone(),
            })
            .collect();
        let _ = write!(
            out,
            "\nCopied in without a submodule, so with no pin to read (version by hand):\n{}",
            bullets(&dirs)
        );
    }
    let any_files = report
        .pins
        .iter()
        .map(|p| &p.licenses)
        .chain(report.unpinned_licenses.values())
        .any(|l| !l.files.is_empty());
    if any_files && !report.licenses_identified {
        let _ = writeln!(
            out,
            "\nLicenses not identified, as askalono did not run (sudo dnf install \
             askalono-cli), so only where they are is shown."
        );
    } else if any_files {
        let _ = writeln!(
            out,
            "\nThe spec's License: has to cover the bundled components' licenses too."
        );
    }
    if !report.disagreements.is_empty() {
        let _ = writeln!(out, "\nThe spec disagrees:");
        for d in &report.disagreements {
            let line = match (&d.spec, &d.pin) {
                (Some(s), Some(p)) => format!("bundled({}) = {s}, the pin is {p}", d.name),
                (None, Some(p)) => format!("no bundled({}), the pin is {p}", d.name),
                (Some(s), None) => {
                    format!(
                        "bundled({}) = {s}, but no shipped submodule has that name",
                        d.name
                    )
                }
                (None, None) => continue,
            };
            let _ = writeln!(out, "  - {line}");
        }
    }
    if !report.directories.is_empty() {
        let _ = writeln!(
            out,
            "\nBundled directories {}, as `ls | LC_ALL=C sort -f | xargs` lists them:\n  {}",
            if report.after_prep {
                "as %prep leaves them"
            } else {
                "as the release ships them"
            },
            report.directories.join(" ")
        );
    }
    out
}

/// Build the report: upstream at the tag, every submodule's pin, and,
/// with a spec, how its lines and `%prep` compare.
pub fn report(
    base: &Path,
    repo: &str,
    tag: &str,
    spec: Option<&SpecFacts>,
    verbose: bool,
) -> Result<Report, String> {
    // Checked first, so the warning comes before the clones, not after.
    let askalono = askalono_available();
    if !askalono {
        eprintln!(
            "warning: askalono is not installed (sudo dnf install askalono-cli), so the \
             bundled components' licenses will not be identified"
        );
    }
    eprintln!("Reading {repo} at {tag}...");
    let (dir, upstream) = upstream_clone(base, repo, tag)?;
    let tree = git(Some(&dir), &["ls-tree", "-r", "-t", tag])
        .map_err(|e| format!("{tag} not found in {repo}: {e}"))?;
    let entries = parse_ls_tree(&tree);
    let gitlinks: BTreeMap<String, String> = entries
        .iter()
        .filter(|(mode, kind, _, _)| mode == "160000" && kind == "commit")
        .map(|(_, _, obj, path)| (path.clone(), obj.clone()))
        .collect();
    let urls: BTreeMap<String, String> = if gitlinks.is_empty() {
        BTreeMap::new()
    } else {
        let declared = git(Some(&dir), &["show", &format!("{tag}:.gitmodules")])
            .map_err(|e| format!("{tag} has submodules but no readable .gitmodules: {e}"))?;
        parse_gitmodules(&declared).into_iter().collect()
    };
    // A submodule `.gitmodules` does not name is still one: reported
    // with why it has no version, not dropped.
    let subs: Vec<(String, Option<String>, String)> = gitlinks
        .iter()
        .map(|(path, commit)| {
            (
                path.clone(),
                urls.get(path).map(|u| resolve_url(&upstream, u)),
                commit.clone(),
            )
        })
        .collect();
    eprintln!(
        "Reading {} submodule(s) from their repositories...",
        subs.len()
    );
    let state_of = |base_name: &str| match spec {
        Some(s) if s.removed.iter().any(|d| d == base_name) => State::Removed,
        Some(s) if s.replaced.iter().any(|d| d == base_name) => State::Replaced,
        _ => State::Bundled,
    };
    // Only what ships needs its licenses read.
    let licensed: Vec<bool> = subs
        .iter()
        .map(|(path, _, _)| state_of(dir_name(path)) == State::Bundled)
        .collect();
    let read = read_pins(base, &subs, &licensed, verbose);

    let (unpinned, bundled_dirs) = bundle_layout(&entries, &gitlinks);
    let mut pins: Vec<Pin> = subs
        .iter()
        .zip(read)
        .map(|((path, url, commit), (version, licenses))| {
            let base_name = dir_name(path).to_string();
            let name = spec
                .and_then(|s| {
                    s.provides
                        .iter()
                        .find(|(n, _)| n.eq_ignore_ascii_case(&base_name))
                        .map(|(n, _)| n.clone())
                })
                .unwrap_or_else(|| base_name.clone());
            let state = state_of(&base_name);
            let (version, error) = match version {
                Ok(v) => (Some(v), None),
                Err(e) => (None, Some(e)),
            };
            Pin {
                name,
                path: path.clone(),
                url: url.clone(),
                commit: commit.clone(),
                version,
                error,
                state,
                referenced: referenced(&dir, tag, &base_name, &bundled_dirs),
                licenses: unread_licenses(licenses),
            }
        })
        .collect();
    let mut unpinned_licenses: BTreeMap<String, Licenses> = unpinned
        .iter()
        .map(|d| {
            (
                d.clone(),
                unread_licenses(license_files(&dir, tag, Some(d))),
            )
        })
        .collect();
    let licenses_identified = {
        let mut all: Vec<&mut Licenses> = pins
            .iter_mut()
            .map(|p| &mut p.licenses)
            .chain(unpinned_licenses.values_mut())
            .collect();
        askalono && identify_into(&mut all)
    };
    pins.sort_by_key(|p| p.name.to_ascii_lowercase());

    let mut directories: Vec<String> = pins
        .iter()
        .filter(|p| p.state != State::Removed)
        .map(|p| dir_name(&p.path).to_string())
        .chain(unpinned.iter().map(|p| dir_name(p).to_string()))
        .collect();
    sort_f(&mut directories);
    directories.dedup();

    let disagreements = spec
        .map(|s| disagreements(&s.provides, &pins, &unpinned))
        .unwrap_or_default();
    Ok(Report {
        repo: repo.to_string(),
        tag: tag.to_string(),
        pins,
        unpinned,
        unpinned_licenses,
        licenses_identified,
        disagreements,
        directories,
        after_prep: spec.is_some(),
    })
}

/// A component's license files, as read, their contents kept with them
/// until askalono has read them.
fn unread_licenses(read: Result<Vec<LicenseText>, String>) -> Licenses {
    match read {
        Ok(files) => {
            let (files, texts) = files
                .into_iter()
                .map(|(path, text)| {
                    (
                        LicenseFile {
                            path,
                            license: None,
                        },
                        text,
                    )
                })
                .unzip();
            Licenses {
                files,
                texts,
                ..Default::default()
            }
        }
        Err(e) => Licenses {
            error: Some(e),
            ..Default::default()
        },
    }
}

/// Identify every component's license files with one askalono run, and
/// set each file's license and each component's expression; the
/// contents are dropped either way. Whether askalono identified them:
/// false when there were none, or it failed, which is said once.
fn identify_into(all: &mut [&mut Licenses]) -> bool {
    let texts: Vec<Vec<u8>> = all
        .iter_mut()
        .flat_map(|l| std::mem::take(&mut l.texts))
        .collect();
    if texts.is_empty() {
        return false;
    }
    eprintln!(
        "Identifying {} license file(s) with askalono...",
        texts.len()
    );
    let flat: Vec<&[u8]> = texts.iter().map(Vec::as_slice).collect();
    let found = match identify(&flat) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("warning: {e}; the licenses are not identified");
            return false;
        }
    };
    let mut found = found.into_iter();
    for l in all.iter_mut() {
        for f in &mut l.files {
            f.license = found.next().flatten();
        }
        l.expression = license_expression(&l.files);
    }
    true
}

/// The tag to read: the one the spec's Source0 names, or `--tag` where it
/// names none (no spec, a Source0 that is not a forge archive, or one
/// whose tag still holds a macro). A spec describes one release, so a
/// `--tag` naming another is refused rather than compared against it.
pub fn pick_tag(explicit: Option<&str>, from_spec: Option<&str>) -> Result<String, String> {
    match (explicit, from_spec) {
        (Some(e), Some(s)) if e != s && !s.contains('%') => Err(format!(
            "the spec's Source0 names {s} but --tag says {e}: drop --tag, or update the \
             spec's Version to the release to read"
        )),
        (Some(e), _) => Ok(e.to_string()),
        (None, Some(s)) => Ok(s.to_string()),
        (None, None) => Err("no tag: give a spec whose Source0 names one, or --tag".into()),
    }
}

/// The `bundled-provides` subcommand.
pub fn run(opts: &Options) -> Result<(), String> {
    sandogasa_cli::require_tools(&[("git", "sudo dnf install git-core", Some("--version"))])?;
    let spec = match &opts.spec {
        Some(path) => Some(parse_spec(&expanded_spec(path)?)),
        None => None,
    };
    let from_source = spec
        .as_ref()
        .and_then(|s| s.source0.as_deref())
        .and_then(repo_and_tag_from_source);
    let repo = opts
        .repo
        .clone()
        .or_else(|| from_source.as_ref().map(|(r, _)| r.clone()))
        .ok_or("no upstream repository: give a spec whose Source0 is a forge archive, or --repo")?;
    let tag = pick_tag(
        opts.tag.as_deref(),
        from_source.as_ref().map(|(_, t)| t.as_str()),
    )?;
    if repo.contains('%') || tag.contains('%') {
        return Err(format!(
            "{repo} at {tag} still holds a macro: install rpmspec (sudo dnf install \
             rpm-build), or pass --repo and --tag"
        ));
    }
    let base = dirs::cache_dir()
        .ok_or("cannot locate the cache directory: neither $XDG_CACHE_HOME nor $HOME is set")?
        .join("ebranch")
        .join("bundled");
    let report = report(&base, &repo, &tag, spec.as_ref(), opts.verbose)?;
    if opts.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("JSON serialization failed")
        );
    } else {
        print!("{}", render(&report));
    }
    Ok(())
}

// ---- parsing helpers ----

/// The last path component of `path`.
fn dir_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The entries of `git ls-tree` output, as (mode, kind, object, path).
fn parse_ls_tree(text: &str) -> Vec<(String, String, String, String)> {
    text.lines()
        .filter_map(|line| {
            let (meta, path) = line.split_once('\t')?;
            let mut parts = meta.split_whitespace();
            Some((
                parts.next()?.to_string(),
                parts.next()?.to_string(),
                parts.next()?.to_string(),
                path.to_string(),
            ))
        })
        .collect()
}

/// `bundled(name) = version` as (name, version); a line with no
/// version has `""`.
fn parse_bundled(item: &str) -> Option<(String, String)> {
    let rest = item.strip_prefix("bundled(")?;
    let (name, after) = rest.split_once(')')?;
    let version = after
        .trim()
        .strip_prefix('=')
        .map(|v| v.trim().to_string())
        .unwrap_or_default();
    Some((name.to_string(), version))
}

/// The spec's lines with `\`-continued ones joined.
fn join_continuations(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut pending = String::new();
    for line in text.lines() {
        match line.strip_suffix('\\') {
            Some(head) => {
                pending.push_str(head);
                pending.push(' ');
            }
            None => {
                pending.push_str(line);
                out.push(std::mem::take(&mut pending));
            }
        }
    }
    if !pending.is_empty() {
        out.push(pending);
    }
    out
}

/// `word` with one level of shell braces expanded:
/// `external/{Catch2,json}` is `external/Catch2` and `external/json`.
fn expand_braces(word: &str) -> Vec<String> {
    let (Some(open), Some(close)) = (word.find('{'), word.find('}')) else {
        return vec![word.to_string()];
    };
    if close < open {
        return vec![word.to_string()];
    }
    let (head, tail) = (&word[..open], &word[close + 1..]);
    word[open + 1..close]
        .split(',')
        .map(|alt| format!("{head}{alt}{tail}"))
        .collect()
}

/// Read one `%prep` line, following a `for <var> in <words>; do … done`
/// loop: inside one, each command is read once per word with `$var` or
/// `${var}` standing for it. `;`-separated commands on one line are
/// read in turn, so a loop on a single line works too.
fn read_prep_shell(
    line: &str,
    prep_loop: &mut Option<(String, Vec<String>)>,
    facts: &mut SpecFacts,
) {
    for part in line.split(';') {
        let part = part.trim();
        let words: Vec<&str> = part.split_whitespace().collect();
        if let ["for", var, "in", rest @ ..] = words.as_slice() {
            let items = rest
                .iter()
                .take_while(|w| **w != "do")
                .flat_map(|w| expand_braces(w))
                .collect();
            *prep_loop = Some((var.to_string(), items));
            continue;
        }
        let part = part.strip_prefix("do ").unwrap_or(part).trim();
        if part == "do" || part.is_empty() {
            continue;
        }
        if part == "done" {
            *prep_loop = None;
            continue;
        }
        match prep_loop {
            Some((var, items)) => {
                for item in items.iter() {
                    let cmd = part
                        .replace(&format!("${{{var}}}"), item)
                        .replace(&format!("${var}"), item);
                    read_prep_line(&cmd, facts);
                }
            }
            None => read_prep_line(part, facts),
        }
    }
}

/// Note what one `%prep` line removes or links over in `facts`.
fn read_prep_line(line: &str, facts: &mut SpecFacts) {
    let words: Vec<&str> = line.split_whitespace().collect();
    let Some(cmd) = words.first() else {
        return;
    };
    let args = || {
        words
            .iter()
            .skip(1)
            .filter(|w| !w.starts_with('-'))
            .flat_map(|w| expand_braces(w))
            .collect::<Vec<_>>()
    };
    match *cmd {
        "rm" => {
            for w in args() {
                facts
                    .removed
                    .push(dir_name(w.trim_end_matches('/')).to_string());
            }
        }
        "ln" => {
            // The link's path is the last argument; the library's
            // directory is the one right under the bundle dir, so take
            // every directory the link sits in and let the caller match
            // its names.
            if let Some(target) = args().last() {
                for dir in target.trim_end_matches('/').split('/') {
                    if !dir.is_empty() && !dir.starts_with('%') && dir != "." {
                        facts.replaced.push(dir.to_string());
                    }
                }
            }
        }
        _ => {}
    }
}

/// Where the libraries sit in upstream's tree, from its entries and its
/// submodules (path to commit): the directories copied in beside the
/// submodules with no pin, by path, and every directory a library is
/// bundled in.
///
/// A bundle root is a directory at least half of whose subdirectories,
/// and two or more, are named as submodules are, outside any other
/// root: the submodules' own parent (`external/`), and a committed copy
/// of them that a release ships instead (`external_imported/`, which
/// et's CMake files build from). Every subdirectory of a root is a
/// bundled directory, as is every submodule wherever it sits, so a
/// mention in one of their build files is the library's own, not
/// upstream building it; one named as no submodule is copied in
/// unpinned (et's libvterm), listed once by name.
fn bundle_layout(
    entries: &[(String, String, String, String)],
    gitlinks: &BTreeMap<String, String>,
) -> (Vec<String>, Vec<String>) {
    let names: Vec<&str> = gitlinks.keys().map(|p| dir_name(p)).collect();
    // Each directory's subdirectories (and submodules), by parent.
    let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (mode, kind, _, path) in entries {
        let is_dir = kind == "tree" || (mode == "160000" && kind == "commit");
        if let Some((parent, _)) = path.rsplit_once('/')
            && is_dir
        {
            children
                .entry(parent.to_string())
                .or_default()
                .push(path.clone());
        }
    }
    // At least two submodules' names, or one when there is only one,
    // and not inside another root's library: a benchmark directory in
    // a bundled library can hold a copy named like one.
    let wanted = names.len().min(2);
    let candidates: Vec<&String> = children
        .iter()
        .filter(|(_, kids)| {
            let named = kids.iter().filter(|k| names.contains(&dir_name(k))).count();
            named >= wanted && named > 0 && named * 2 >= kids.len()
        })
        .map(|(root, _)| root)
        .collect();
    let roots: Vec<&String> = candidates
        .iter()
        .filter(|r| {
            !candidates
                .iter()
                .any(|o| o != *r && r.starts_with(&format!("{o}/")))
        })
        .copied()
        .collect();
    let mut bundled_dirs: Vec<String> = roots
        .iter()
        .flat_map(|r| children[r.as_str()].iter().cloned())
        .chain(gitlinks.keys().cloned())
        .collect();
    bundled_dirs.sort();
    bundled_dirs.dedup();
    let mut unpinned: Vec<String> = Vec::new();
    for path in &bundled_dirs {
        let name = dir_name(path);
        if !names.contains(&name) && !unpinned.iter().any(|u| dir_name(u) == name) {
            unpinned.push(path.clone());
        }
    }
    unpinned.sort();
    (unpinned, bundled_dirs)
}

// ---- git ----

/// What a clone keeps: a submodule's needs only its history, upstream's
/// its trees and files too, for its tree and build files at the tag.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CloneKind {
    History,
    Full,
}

/// Run `git` with `args` in `dir` (when given), returning its stdout.
fn git(dir: Option<&Path>, args: &[&str]) -> Result<String, String> {
    git_bytes(dir, args).map(|out| String::from_utf8_lossy(&out).into_owned())
}

/// [`git`], its stdout as it is: a file's contents need not be text.
fn git_bytes(dir: Option<&Path>, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("git");
    if let Some(d) = dir {
        cmd.arg("-C").arg(d);
    }
    cmd.args(args);
    // Dates in UTC, so a snapshot's date does not depend on the offset
    // its committer was at.
    cmd.env("TZ", "UTC");
    // Never prompt: a private or mistyped URL, or an ssh one with no
    // key loaded, fails rather than waiting on a terminal.
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        cmd.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    let out = cmd.output().map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

/// The cache directory for `url`'s clone of `kind`: one directory per
/// repository and kind, directly under `base`, so `prune-cache` removes
/// each whole. A readable name, then a hash of the URL as given, so two
/// URLs that read alike never share one.
fn clone_dir(base: &Path, url: &str, kind: CloneKind) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let readable: String = url
        .split("://")
        .last()
        .unwrap_or(url)
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    let suffix = match kind {
        CloneKind::History => "history",
        CloneKind::Full => "full",
    };
    base.join(format!("{readable}-{:08x}-{suffix}.git", h.finish() as u32))
}

/// A bare clone of `url` under `base` holding `want` (a tag or commit,
/// as `git cat-file -e` takes it): one already cached that has it is
/// used as it is, one that lacks it is fetched, and none is cloned.
fn mirror(base: &Path, url: &str, kind: CloneKind, want: &str) -> Result<PathBuf, String> {
    let dir = clone_dir(base, url, kind);
    if dir.join("HEAD").exists() {
        if git(Some(&dir), &["cat-file", "-e", want]).is_err() {
            // Through the clone's own remote, so a history-only clone
            // keeps its filter.
            git(
                Some(&dir),
                &[
                    "fetch",
                    "--quiet",
                    "--force",
                    "--prune",
                    "origin",
                    "+refs/heads/*:refs/heads/*",
                    "+refs/tags/*:refs/tags/*",
                ],
            )?;
        }
        sandogasa_cli::cache::mark_used(&dir);
        return Ok(dir);
    }
    std::fs::create_dir_all(base).map_err(|e| format!("cannot create {}: {e}", base.display()))?;
    let dir_s = dir.to_string_lossy().into_owned();
    let mut args = vec!["clone", "--quiet", "--bare"];
    if kind == CloneKind::History {
        args.push("--filter=tree:0");
    }
    args.extend([url, dir_s.as_str()]);
    git(None, &args)?;
    sandogasa_cli::cache::mark_used(&dir);
    Ok(dir)
}

/// Of several tags on one commit, the one naming the highest version.
fn best_tag<'a>(tags: impl Iterator<Item = &'a str>) -> Option<String> {
    tags.filter_map(|t| tag_version(t.trim()).map(|v| (v, t.trim().to_string())))
        .max_by(|a, b| sandogasa_rpmvercmp::rpmvercmp(&a.0, &b.0))
        .map(|(_, t)| t)
}

/// Make sure the clone at `dir` has `commit`: one pinned off every
/// branch and tag is not fetched with them, so it is asked for by name.
fn ensure_commit(dir: &Path, commit: &str) -> Result<(), String> {
    if git(
        Some(dir),
        &["cat-file", "-e", &format!("{commit}^{{commit}}")],
    )
    .is_err()
    {
        git(Some(dir), &["fetch", "--quiet", "origin", commit])
            .map_err(|e| format!("commit {commit} not found: {e}"))?;
    }
    Ok(())
}

/// The version of `commit` in the clone at `dir`.
fn read_pin(dir: &Path, commit: &str) -> Result<String, String> {
    ensure_commit(dir, commit)?;
    let exact = git(Some(dir), &["tag", "--points-at", commit])?;
    let exact = best_tag(exact.lines());
    // The nearest earlier tag that names a version: one with a digit,
    // which is what `tag_version` needs before it reads one.
    let nearest = match exact {
        Some(_) => None,
        None => git(
            Some(dir),
            &[
                "describe",
                "--tags",
                "--abbrev=0",
                "--match",
                "*[0-9]*",
                commit,
            ],
        )
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| tag_version(t).is_some()),
    };
    let date = git(
        Some(dir),
        &[
            "show",
            "-s",
            "--format=%cd",
            "--date=format-local:%Y%m%d",
            commit,
        ],
    )?;
    Ok(pin_version(
        exact.as_deref(),
        nearest.as_deref(),
        date.trim(),
        commit,
    ))
}

/// Upstream's repository to read `tag` from, and the URL its relative
/// submodule URLs (`../json.git`) are relative to. A local checkout
/// (`repo` an existing directory) is read in place, never fetched, and
/// its `origin` is what relative URLs follow, as `git submodule` does;
/// anything else is cloned or brought up to date under `base`.
fn upstream_clone(base: &Path, repo: &str, tag: &str) -> Result<(PathBuf, String), String> {
    let local = Path::new(repo);
    if !local.is_dir() {
        let dir = mirror(base, repo, CloneKind::Full, &format!("{tag}^{{commit}}"))?;
        return Ok((dir, repo.to_string()));
    }
    git(
        Some(local),
        &["cat-file", "-e", &format!("{tag}^{{commit}}")],
    )
    .map_err(|_| {
        format!(
            "{tag} is not in the checkout at {repo}: fetch it (git -C {repo} fetch --tags) \
             or name a tag it has"
        )
    })?;
    let origin = git(Some(local), &["remote", "get-url", "origin"])
        .ok()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| repo.to_string());
    Ok((local.to_path_buf(), origin))
}

/// Whether upstream's own build files at `tag` mention `name` as a
/// word: CMake, Meson and make files outside every bundled directory
/// in `bundled`, since a library's own CMakeLists.txt, or another
/// bundled one's, says nothing about whether upstream builds it.
fn referenced(dir: &Path, tag: &str, name: &str, bundled: &[String]) -> bool {
    let Ok(files) = git(
        Some(dir),
        &[
            "grep",
            "-l",
            "-w",
            "-F",
            name,
            tag,
            "--",
            "*CMakeLists.txt",
            "*.cmake",
            "*meson.build",
            "*Makefile*",
            "*.mk",
            "*configure.ac",
        ],
    ) else {
        return false;
    };
    let prefix = format!("{tag}:");
    files.lines().any(|f| {
        let path = f.strip_prefix(&prefix).unwrap_or(f);
        !bundled.iter().any(|b| {
            path.strip_prefix(b.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
        })
    })
}

/// A license file's path and contents.
type LicenseText = (String, Vec<u8>);

/// Whether the clone at `dir` has `oid`, without fetching it.
fn has_object(dir: &Path, oid: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["cat-file", "-e", oid])
        .env("GIT_NO_LAZY_FETCH", "1")
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The license files at `rev` in the clone at `dir`, with their contents:
/// at the top of the tree, or in the directory `under` when given (a
/// copied-in library in upstream's tree), named relative to it. Only
/// where there is none there, every file of a REUSE `LICENSES/`
/// directory: it holds the license of anything anywhere in the
/// repository, tests and tooling included (nlohmann/json's has
/// GPL-3.0-only beside the MIT its header is under). A history-only
/// clone lacks the files: those it lacks are fetched in one request, so
/// a later run reads them from the cache.
fn license_files(dir: &Path, rev: &str, under: Option<&str>) -> Result<Vec<LicenseText>, String> {
    let prefix = under
        .map(|u| format!("{}/", u.trim_end_matches('/')))
        .unwrap_or_default();
    let list = |path: &str| {
        let mut args = vec!["ls-tree", rev];
        if !path.is_empty() {
            args.extend(["--", path]);
        }
        git(Some(dir), &args).map(|out| parse_ls_tree(&out))
    };
    let top = list(&prefix)?;
    let mut found: Vec<(String, String)> = top
        .iter()
        .filter(|(_, kind, _, path)| kind == "blob" && is_license_file(dir_name(path)))
        .map(|(_, _, obj, path)| (path.clone(), obj.clone()))
        .collect();
    let reuse = top.iter().find(|(_, kind, _, path)| {
        kind == "tree" && dir_name(path).eq_ignore_ascii_case("LICENSES")
    });
    if found.is_empty()
        && let Some((_, _, _, path)) = reuse
    {
        for (_, kind, obj, path) in list(&format!("{path}/"))? {
            if kind == "blob" {
                found.push((path, obj));
            }
        }
    }
    let missing: Vec<&str> = found
        .iter()
        .map(|(_, obj)| obj.as_str())
        .filter(|obj| !has_object(dir, obj))
        .collect();
    if !missing.is_empty() {
        // As git's own on-demand fetch does: with no negotiation, as
        // the clone has the commits that reach these files, which would
        // tell the forge it has them too.
        let mut args = vec![
            "-c",
            "fetch.negotiationAlgorithm=noop",
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            "--filter=blob:none",
            "origin",
        ];
        args.extend(&missing);
        git(Some(dir), &args)?;
    }
    found
        .into_iter()
        .map(|(path, obj)| {
            let text = git_bytes(Some(dir), &["cat-file", "blob", &obj])?;
            let rel = path.strip_prefix(&prefix).unwrap_or(&path).to_string();
            Ok((rel, text))
        })
        .collect()
}

/// Whether `askalono` can be run.
fn askalono_available() -> bool {
    Command::new("askalono")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// What askalono identifies in each of `texts`, in order, from one
/// `askalono --format json identify --batch` run over them written out
/// to a temporary directory. An error when askalono is not installed or
/// fails, so no license is taken from a run that did not happen.
fn identify(texts: &[&[u8]]) -> Result<Vec<Option<String>>, String> {
    use std::io::Write;
    let tmp = tempfile::tempdir().map_err(|e| format!("cannot make a temporary directory: {e}"))?;
    let mut paths = Vec::with_capacity(texts.len());
    for (i, text) in texts.iter().enumerate() {
        let rel = format!("{i}/LICENSE");
        std::fs::create_dir_all(tmp.path().join(i.to_string()))
            .and_then(|()| std::fs::write(tmp.path().join(&rel), text))
            .map_err(|e| format!("cannot write a license file to identify: {e}"))?;
        paths.push(rel);
    }
    let mut child = Command::new("askalono")
        .args(["--format", "json", "identify", "--batch"])
        .current_dir(tmp.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => "askalono is not installed".to_string(),
            _ => format!("cannot run askalono: {e}"),
        })?;
    let input = paths.join("\n") + "\n";
    let mut stdin = child.stdin.take().expect("piped stdin");
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = child
        .wait_with_output()
        .map_err(|e| format!("cannot run askalono: {e}"))?;
    let _ = writer.join();
    if !out.status.success() {
        return Err(format!(
            "askalono failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let found: BTreeMap<String, Option<String>> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(parse_askalono_line)
        .collect();
    paths
        .iter()
        .map(|p| {
            found.get(p).cloned().ok_or_else(|| {
                format!("askalono said nothing about {p}, so its answers cannot be matched up")
            })
        })
        .collect()
}

/// Read every pin in `subs` (path, URL, commit): each repository is
/// cloned or brought up to date once, a few at a time, with the license
/// files at each pinned commit (which a history-only clone fetches),
/// then each pin is read from its clone. A submodule with no URL, or
/// whose repository cannot be read, gets why, for both.
#[allow(clippy::type_complexity)]
fn read_pins(
    base: &Path,
    subs: &[(String, Option<String>, String)],
    licensed: &[bool],
    verbose: bool,
) -> Vec<(Result<String, String>, Result<Vec<LicenseText>, String>)> {
    const AT_ONCE: usize = 4;
    // One clone per URL, holding every commit pinned from it.
    let mut wanted: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    // The commits whose license files are wanted: those of pins shipped.
    let mut want_licenses: std::collections::BTreeSet<&str> = Default::default();
    for ((_, url, commit), licensed) in subs.iter().zip(licensed) {
        if let Some(url) = url {
            wanted
                .entry(url.as_str())
                .or_default()
                .push(commit.as_str());
        }
        if *licensed {
            want_licenses.insert(commit.as_str());
        }
    }
    let want_licenses = &want_licenses;
    let urls: Vec<(&str, Vec<&str>)> = wanted.into_iter().collect();
    type Read = (PathBuf, BTreeMap<String, Result<Vec<LicenseText>, String>>);
    let mut dirs: BTreeMap<&str, Result<Read, String>> = BTreeMap::new();
    for chunk in urls.chunks(AT_ONCE) {
        std::thread::scope(|s| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|(url, commits)| {
                    s.spawn(move || {
                        if verbose {
                            eprintln!("[bundled] reading {url}");
                        }
                        // Fetched again if any pinned commit is missing.
                        let mut dir = mirror(base, url, CloneKind::History, commits[0]);
                        for c in &commits[1..] {
                            dir = dir.and_then(|_| mirror(base, url, CloneKind::History, c));
                        }
                        let read = dir.map(|dir| {
                            let licenses = commits
                                .iter()
                                .filter(|c| want_licenses.contains(*c))
                                .map(|c| {
                                    let files = ensure_commit(&dir, c)
                                        .and_then(|()| license_files(&dir, c, None));
                                    (c.to_string(), files)
                                })
                                .collect();
                            (dir, licenses)
                        });
                        (*url, read)
                    })
                })
                .collect();
            for h in handles {
                if let Ok((url, dir)) = h.join() {
                    dirs.insert(url, dir);
                }
            }
        });
    }
    subs.iter()
        .map(|(path, url, commit)| {
            let Some(url) = url.as_deref() else {
                let why = format!(".gitmodules names no URL for {path}");
                return (Err(why.clone()), Err(why));
            };
            match dirs.get(url) {
                Some(Ok((dir, licenses))) => (
                    read_pin(dir, commit),
                    // None for a pin not shipped, whose licenses were
                    // not wanted.
                    licenses
                        .get(commit.as_str())
                        .cloned()
                        .unwrap_or(Ok(Vec::new())),
                ),
                Some(Err(e)) => (Err(e.clone()), Err(e.clone())),
                None => {
                    let why = format!("{url} was not read");
                    (Err(why.clone()), Err(why))
                }
            }
        })
        .collect()
}

/// The spec's text with its macros expanded by `rpmspec -P`. Without
/// rpmspec, or when it fails, the raw text, with a warning saying why:
/// that serves when Source0 and the Provides name their values outright.
fn expanded_spec(path: &Path) -> Result<String, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    match Command::new("rpmspec").arg("-P").arg(path).output() {
        Ok(out) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        Ok(out) => {
            eprintln!(
                "warning: rpmspec -P {} failed ({}); reading the spec unexpanded, so \
                 macros in it are compared as written",
                path.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
            Ok(raw)
        }
        Err(_) => {
            eprintln!(
                "warning: rpmspec is not installed (sudo dnf install rpm-build); reading \
                 the spec unexpanded, so macros in it are compared as written"
            );
            Ok(raw)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_names_its_version_from_the_first_digit_that_starts_a_word() {
        assert_eq!(tag_version("v4.3.0").as_deref(), Some("4.3.0"));
        assert_eq!(tag_version("4.26").as_deref(), Some("4.26"));
        assert_eq!(tag_version("et-v7.1.1").as_deref(), Some("7.1.1"));
        assert_eq!(tag_version("release-0.15.3").as_deref(), Some("0.15.3"));
        assert_eq!(tag_version("1.0-rc1").as_deref(), Some("1.0~rc1"));
        assert_eq!(tag_version("2024-01-05").as_deref(), Some("2024.01.05"));
        // A digit inside the project's name is not the version.
        assert_eq!(tag_version("bzip2-1.0.8").as_deref(), Some("1.0.8"));
        assert_eq!(tag_version("pcre2-10.42").as_deref(), Some("10.42"));
        assert_eq!(tag_version("SDL2-2.30.0").as_deref(), Some("2.30.0"));
        assert_eq!(tag_version("c2-v1.0").as_deref(), Some("1.0"));
        assert_eq!(tag_version("nightly"), None);
        assert_eq!(tag_version("sdl2"), None);
    }

    /// The fixture of #109, et-v7.1.1's pins.
    #[test]
    fn a_pin_is_versioned_by_the_snapshot_rule() {
        let c = "424b969f00c9c9dd8c0c9a1e4ae8e0bd4c0b5fb6";
        assert_eq!(pin_version(Some("v4.3.0"), None, "20250101", c), "4.3.0");
        assert_eq!(
            pin_version(None, Some("v1.0.1"), "20200819", c),
            "1.0.1^20200819git424b969"
        );
        assert_eq!(
            pin_version(
                None,
                None,
                "20140926",
                "9a42ec1329f259a5f4881a291db1dcb8f2ad9040"
            ),
            "0^20140926git9a42ec1"
        );
        // A tag that names no version is no tag at all.
        assert_eq!(
            pin_version(Some("latest"), Some("nightly"), "20140926", "9a42ec1"),
            "0^20140926git9a42ec1"
        );
    }

    #[test]
    fn gitmodules_gives_each_path_its_url() {
        let text = "\
[submodule \"external/sole\"]
\tpath = external/sole
\turl = https://github.com/r-lyeh-archived/sole.git
[submodule \"external/json\"]
\turl = ../json.git
\tpath = \"external/json/\"
\tbranch = develop
";
        assert_eq!(
            parse_gitmodules(text),
            [
                (
                    "external/sole".to_string(),
                    "https://github.com/r-lyeh-archived/sole.git".to_string()
                ),
                ("external/json".to_string(), "../json.git".to_string()),
            ]
        );
        assert_eq!(
            resolve_url(
                "https://github.com/MisterTea/EternalTerminal",
                "../json.git"
            ),
            "https://github.com/MisterTea/json.git"
        );
        assert_eq!(resolve_url("https://x/a/b", "https://y/c"), "https://y/c");
    }

    #[test]
    fn a_tag_given_with_a_spec_must_be_the_one_it_names() {
        assert_eq!(pick_tag(None, Some("v1.2")).unwrap(), "v1.2");
        assert_eq!(pick_tag(Some("v1.2"), Some("v1.2")).unwrap(), "v1.2");
        assert_eq!(pick_tag(Some("v1.3"), None).unwrap(), "v1.3");
        // A tag still holding a macro names nothing yet, so --tag fills it.
        assert_eq!(pick_tag(Some("v1.3"), Some("v%{version}")).unwrap(), "v1.3");
        let e = pick_tag(Some("v1.3"), Some("v1.2")).unwrap_err();
        assert!(e.contains("names v1.2 but --tag says v1.3"), "{e}");
        assert!(pick_tag(None, None).unwrap_err().starts_with("no tag"));
    }

    #[test]
    fn source0_names_the_repository_and_tag() {
        let cases = [
            (
                "https://github.com/MisterTea/EternalTerminal/archive/et-v7.1.1/EternalTerminal-7.1.1.tar.gz",
                "https://github.com/MisterTea/EternalTerminal",
                "et-v7.1.1",
            ),
            (
                "https://github.com/o/r/archive/refs/tags/v1.2.tar.gz",
                "https://github.com/o/r",
                "v1.2",
            ),
            (
                "https://github.com/o/r/archive/v1.2.tar.gz",
                "https://github.com/o/r",
                "v1.2",
            ),
            (
                "https://github.com/o/r/releases/download/v1.2/r-1.2.tar.xz",
                "https://github.com/o/r",
                "v1.2",
            ),
            (
                "https://gitlab.com/g/p/-/archive/v3/p-v3.tar.gz",
                "https://gitlab.com/g/p",
                "v3",
            ),
            (
                "https://github.com/o/r/archive/v1.2/r-1.2.tar.gz#/r.tgz",
                "https://github.com/o/r",
                "v1.2",
            ),
        ];
        for (source, repo, tag) in cases {
            assert_eq!(
                repo_and_tag_from_source(source),
                Some((repo.to_string(), tag.to_string())),
                "{source}"
            );
        }
        assert_eq!(repo_and_tag_from_source("r-1.2.tar.gz"), None);
    }

    /// et's 6.2.11 spec, cut down: what it provides and what its %prep
    /// takes out or swaps for the system copy, in the shapes a %prep
    /// writes them.
    #[test]
    fn a_spec_says_what_it_provides_and_what_prep_takes_out() {
        let spec = "\
Name:           et
Source0:        https://github.com/MisterTea/EternalTerminal/archive/et-v6.2.11/EternalTerminal-6.2.11.tar.gz
Provides:       bundled(base64) = 0
Provides:       bundled(PlatformFolders) = 4.0.0, bundled(sole) = 1.0.1
Provides:       bundled(ThreadPool)

%description
Provides: bundled(not-this) = 1

%prep
%autosetup -n EternalTerminal-et-v6.2.11
rm -rf external_imported/Catch2
rm -rf external_imported/{cpp-httplib,cxxopts}
rm -rf \\
    external_imported/json \\
    external_imported/simpleini/
mkdir -p external_imported/easyloggingpp/src
ln -sf /usr/include external_imported/easyloggingpp/src

%build
rm -rf something-else
";
        let facts = parse_spec(spec);
        assert_eq!(
            facts.source0.as_deref(),
            Some(
                "https://github.com/MisterTea/EternalTerminal/archive/et-v6.2.11/EternalTerminal-6.2.11.tar.gz"
            )
        );
        assert_eq!(
            facts.provides,
            [
                ("base64".to_string(), "0".to_string()),
                ("PlatformFolders".to_string(), "4.0.0".to_string()),
                ("sole".to_string(), "1.0.1".to_string()),
                ("ThreadPool".to_string(), String::new()),
            ]
        );
        assert_eq!(
            facts.removed,
            ["Catch2", "cpp-httplib", "cxxopts", "json", "simpleini"]
        );
        assert!(facts.replaced.contains(&"easyloggingpp".to_string()));
        assert!(!facts.removed.contains(&"something-else".to_string()));
    }

    /// et's 7.1.1 %prep removes its unbundled and unused libraries in a
    /// loop, on several lines or one.
    #[test]
    fn prep_loops_are_followed() {
        let facts = parse_spec(
            "%prep\n\
             for d in Catch2 cotire json; do\n\
             \x20 rm -r external_imported/$d\n\
             done\n\
             for x in vcpkg; do rm -rf external_imported/${x}; done\n\
             rm -rf after/loop\n",
        );
        assert_eq!(facts.removed, ["Catch2", "cotire", "json", "vcpkg", "loop"]);
    }

    #[test]
    fn braces_expand_one_level() {
        assert_eq!(expand_braces("a/{b,c}/d"), ["a/b/d", "a/c/d"]);
        assert_eq!(expand_braces("plain"), ["plain"]);
        assert_eq!(expand_braces("odd}{"), ["odd}{"]);
    }

    #[test]
    fn directories_sort_as_sort_f_does() {
        let mut names: Vec<String> = ["sole", "Catch2", "base64", "PlatformFolders", "_x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        sort_f(&mut names);
        assert_eq!(names, ["base64", "Catch2", "PlatformFolders", "sole", "_x"]);
    }

    fn pin(name: &str, version: &str, state: State) -> Pin {
        Pin {
            name: name.to_string(),
            path: format!("external/{name}"),
            url: None,
            commit: String::new(),
            version: Some(version.to_string()),
            error: None,
            state,
            referenced: true,
            licenses: Licenses::default(),
        }
    }

    #[test]
    fn the_spec_disagrees_where_its_lines_and_the_pins_differ() {
        let provides = vec![
            ("PlatformFolders".to_string(), "4.0.0".to_string()),
            ("sole".to_string(), "1.0.1^20200819git424b969".to_string()),
            ("cotire".to_string(), "1.8.0".to_string()),
            // Versioned by hand, as the report asks for it.
            ("libvterm".to_string(), "0.3.3".to_string()),
        ];
        let pins = vec![
            pin("PlatformFolders", "4.3.0", State::Bundled),
            pin("sole", "1.0.1^20200819git424b969", State::Bundled),
            pin("json", "3.12.0", State::Bundled),
            pin("Catch2", "3.15.2", State::Removed),
        ];
        assert_eq!(
            disagreements(&provides, &pins, &["external/libvterm".to_string()]),
            [
                Disagreement {
                    name: "PlatformFolders".to_string(),
                    spec: Some("4.0.0".to_string()),
                    pin: Some("4.3.0".to_string()),
                },
                Disagreement {
                    name: "json".to_string(),
                    spec: None,
                    pin: Some("3.12.0".to_string()),
                },
                Disagreement {
                    name: "cotire".to_string(),
                    spec: Some("1.8.0".to_string()),
                    pin: None,
                },
            ]
        );
    }

    /// The text report says what is not shipped and why, and each way
    /// the spec can disagree.
    #[test]
    fn the_report_names_what_is_not_shipped_and_each_disagreement() {
        let report = Report {
            repo: "https://github.com/MisterTea/EternalTerminal".to_string(),
            tag: "et-v6.2.11".to_string(),
            pins: vec![
                pin("Catch2", "3.4.0", State::Removed),
                pin("easyloggingpp", "9.97.0", State::Replaced),
                pin("sole", "1.0.1", State::Bundled),
            ],
            unpinned: Vec::new(),
            unpinned_licenses: BTreeMap::new(),
            licenses_identified: true,
            disagreements: vec![
                Disagreement {
                    name: "sole".to_string(),
                    spec: None,
                    pin: Some("1.0.1".to_string()),
                },
                Disagreement {
                    name: "msgpack".to_string(),
                    spec: Some("3.3.0".to_string()),
                    pin: None,
                },
            ],
            directories: Vec::new(),
            after_prep: true,
        };
        let text = render(&report);
        assert!(
            text.contains(
                "Not shipped, so no Provides:\n  - Catch2 (removed in %prep)\n  - \
                 easyloggingpp (replaced by the system copy in %prep)\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("no bundled(sole), the pin is 1.0.1"),
            "{text}"
        );
        assert!(
            text.contains("bundled(msgpack) = 3.3.0, but no shipped submodule has that name"),
            "{text}"
        );
        assert!(text.contains(": 3 submodule(s), 1 bundled"), "{text}");
    }

    /// Submodules at the repository's root are bundled directories too,
    /// so their own build files do not count as upstream building them.
    #[test]
    fn root_level_submodules_are_bundled_directories() {
        let entry = |mode: &str, kind: &str, path: &str| {
            (
                mode.to_string(),
                kind.to_string(),
                "0".to_string(),
                path.to_string(),
            )
        };
        let entries = vec![
            entry("160000", "commit", "googletest"),
            entry("040000", "tree", "src"),
        ];
        let gitlinks = BTreeMap::from([("googletest".to_string(), "abc".to_string())]);
        let (unpinned, bundled) = bundle_layout(&entries, &gitlinks);
        assert!(unpinned.is_empty());
        assert_eq!(bundled, ["googletest"]);
    }

    #[test]
    fn clones_of_lookalike_urls_do_not_share_a_directory() {
        let base = Path::new("/c");
        let a = clone_dir(base, "https://github.com/a/b_c", CloneKind::History);
        let b = clone_dir(base, "https://github.com/a_b/c", CloneKind::History);
        assert_ne!(a, b);
        assert_ne!(
            clone_dir(base, "https://x/r", CloneKind::History),
            clone_dir(base, "https://x/r", CloneKind::Full)
        );
    }

    #[test]
    fn license_files_are_known_by_name() {
        for name in [
            "LICENSE",
            "LICENSE.txt",
            "LICENCE",
            "license.md",
            "License",
            "COPYING",
            "COPYING.LESSER",
            "COPYING-LGPL",
            "LICENSE_1_0.txt",
            "COPYRIGHT",
            "UNLICENSE",
            "foo-LICENSE.md",
            "MIT-LICENSE",
            "MIT-1.0.txt",
            "gpl-3.0.txt",
            "APACHE-2.0.txt",
            "CC-BY-4.0",
            "zlib-bsd.txt",
        ] {
            assert!(is_license_file(name), "{name}");
        }
        for name in [
            "license.c",
            "license.py",
            "LICENSE.go",
            "README.md",
            "licenses_test",
            "gplot.c",
            "mit",
            "APACHE",
            "CMakeLists.txt",
        ] {
            assert!(!is_license_file(name), "{name}");
        }
    }

    #[test]
    fn askalono_says_which_license_a_file_is_or_holds() {
        let one = r#"{"path":"0/LICENSE\n","result":{"score":1.0,"license":{"name":"Zlib","kind":"original","aliases":[]},"containing":[]}}"#;
        assert_eq!(
            parse_askalono_line(one),
            Some(("0/LICENSE".to_string(), Some("Zlib".to_string())))
        );
        let several = r#"{"path":"1/LICENSE","result":{"score":0.4,"license":null,"containing":[
            {"score":0.98,"license":{"name":"MIT","kind":"original","aliases":[]},"line_range":[1,20]},
            {"score":0.97,"license":{"name":"BSD-3-Clause","kind":"original","aliases":[]},"line_range":[22,50]},
            {"score":0.97,"license":{"name":"MIT","kind":"original","aliases":[]},"line_range":[52,70]}]}}"#
            .replace('\n', "");
        assert_eq!(
            parse_askalono_line(&several),
            Some((
                "1/LICENSE".to_string(),
                Some("BSD-3-Clause AND MIT".to_string())
            ))
        );
        let unknown =
            r#"{"path":"2/LICENSE","result":{"score":0.2,"license":null,"containing":[]}}"#;
        assert_eq!(
            parse_askalono_line(unknown),
            Some(("2/LICENSE".to_string(), None))
        );
        let error = r#"{"path":"3/LICENSE","error":"not UTF-8"}"#;
        assert_eq!(
            parse_askalono_line(error),
            Some(("3/LICENSE".to_string(), None))
        );
        assert_eq!(parse_askalono_line("not json"), None);
    }

    #[test]
    fn a_components_licenses_come_to_one_expression_and_line() {
        let file = |path: &str, license: Option<&str>| LicenseFile {
            path: path.to_string(),
            license: license.map(str::to_string),
        };
        let files = vec![
            file("LICENSE", Some("MIT AND BSD-3-Clause")),
            file("COPYING", Some("MIT")),
            file("LICENSE.docs", None),
        ];
        let expression = license_expression(&files);
        assert_eq!(expression.as_deref(), Some("BSD-3-Clause AND MIT"));
        let l = Licenses {
            files,
            expression,
            ..Default::default()
        };
        assert_eq!(
            describe_licenses(&l, true),
            "BSD-3-Clause AND MIT, from LICENSE, COPYING; LICENSE.docs not identified"
        );
        assert_eq!(
            describe_licenses(&l, false),
            "license in LICENSE, COPYING, LICENSE.docs"
        );
        assert_eq!(
            describe_licenses(&Licenses::default(), true),
            "no license file found"
        );
        let unread = Licenses {
            error: Some("commit 123 not found".to_string()),
            ..Default::default()
        };
        assert_eq!(
            describe_licenses(&unread, true),
            "license files not read: commit 123 not found"
        );
        let none = Licenses {
            files: vec![file("LICENSE", None)],
            ..Default::default()
        };
        assert_eq!(describe_licenses(&none, true), "LICENSE not identified");
    }

    /// How a fake askalono behaves.
    #[derive(Clone, Copy, PartialEq)]
    enum Fake {
        /// MIT for a text granting permission, Zlib for one naming zlib,
        /// nothing for anything else.
        Works,
        /// Fails when asked to identify anything.
        Fails,
        /// Answers about files it was not given.
        Misnames,
    }

    /// A fake `askalono` in `bin`, behaving as `how`.
    fn fake_askalono(bin: &Path, how: Fake) {
        use std::os::unix::fs::PermissionsExt;
        let script = if how == Fake::Fails {
            "#!/bin/sh\n[ \"$1\" = --version ] && exit 0\necho 'askalono: broken' >&2\nexit 1\n"
                .to_string()
        } else if how == Fake::Misnames {
            "#!/bin/sh\nwhile read -r p; do printf '{\"path\":\"./%s\"}\\n' \"$p\"; done\n"
                .to_string()
        } else {
            r#"#!/bin/sh
license() {
  printf '{"score":1.0,"license":{"name":"%s","kind":"original","aliases":[]},"containing":[]}' "$1"
}
while read -r p; do
  if grep -q "Permission is hereby granted" "$p"; then r=$(license MIT)
  elif grep -qi "zlib" "$p"; then r=$(license Zlib)
  else r='{"score":0.1,"license":null,"containing":[]}'
  fi
  printf '{"path":"%s\\n","result":%s}\n' "$p" "$r"
done
"#
            .to_string()
        };
        std::fs::create_dir_all(bin).unwrap();
        let path = bin.join("askalono");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// End to end against local repositories standing in for upstream
    /// and its submodules: one pinned on a tag, one ahead of one, one
    /// `.gitmodules` does not name, plus a directory copied in with no
    /// submodule. No network: file URLs.
    #[test]
    #[serial_test::serial]
    fn upstream_at_the_tag_gives_every_pin() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home").to_string_lossy().into_owned();
        let bin = tmp.path().join("bin");
        fake_askalono(&bin, Fake::Works);
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let _env = sandogasa_config::test_support::EnvGuard::new(&[
            ("PATH", &path),
            ("GIT_AUTHOR_EMAIL", "t@example.com"),
            ("GIT_AUTHOR_NAME", "T"),
            ("GIT_COMMITTER_EMAIL", "t@example.com"),
            ("GIT_COMMITTER_NAME", "T"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("HOME", &home),
        ]);
        let run = |dir: &Path, args: &[&str], date: &str| {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .env("GIT_AUTHOR_DATE", date)
                .env("GIT_COMMITTER_DATE", date)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let new_repo = |name: &str| {
            let d = tmp.path().join(name);
            std::fs::create_dir_all(&d).unwrap();
            run(&d, &["init", "-q", "-b", "main"], "2020-01-01T00:00:00Z");
            // Serve history-only clones and objects by id, as a forge
            // does, so the clones are partial as they are for real.
            for key in ["uploadpack.allowFilter", "uploadpack.allowAnySHA1InWant"] {
                run(&d, &["config", key, "true"], "2020-01-01T00:00:00Z");
            }
            d
        };
        let commit = |d: &Path, file: &str, date: &str| {
            std::fs::write(d.join(file), date).unwrap();
            run(d, &["add", file], date);
            run(d, &["commit", "-q", "-m", file], date);
            run(d, &["rev-parse", "HEAD"], date)
        };
        // A submodule pinned exactly on v4.3.0.
        let folders = new_repo("PlatformFolders");
        commit(&folders, "a", "2020-01-01T00:00:00Z");
        std::fs::write(
            folders.join("LICENSE"),
            "MIT License\n\nPermission is hereby granted, free of charge, ...\n",
        )
        .unwrap();
        // Code named like a license file is not one, and a REUSE
        // directory beside a LICENSE is the repository's, not its.
        std::fs::write(folders.join("license.c"), "int x;\n").unwrap();
        std::fs::create_dir_all(folders.join("LICENSES")).unwrap();
        std::fs::write(folders.join("LICENSES/GPL-3.0-only.txt"), "GPL\n").unwrap();
        run(&folders, &["add", "."], "2020-06-01T00:00:00Z");
        run(
            &folders,
            &["commit", "-q", "-m", "license"],
            "2020-06-01T00:00:00Z",
        );
        let folders_pin = commit(&folders, "b", "2021-02-03T00:00:00Z");
        run(&folders, &["tag", "v4.3.0"], "2021-02-03T00:00:00Z");
        commit(&folders, "c", "2022-01-01T00:00:00Z");
        // One two commits past v1.0.1 (annotated), committed late on
        // 2020-08-19 in UTC but the 20th at its committer's offset.
        let sole = new_repo("sole");
        // REUSE only: its LICENSES/ directory is what it has.
        std::fs::create_dir_all(sole.join("LICENSES")).unwrap();
        std::fs::write(sole.join("LICENSES/Zlib.txt"), "the zlib license\n").unwrap();
        std::fs::write(
            sole.join("LICENSES/MIT.txt"),
            "Permission is hereby granted, free of charge, ...\n",
        )
        .unwrap();
        run(&sole, &["add", "."], "2020-01-01T00:00:00Z");
        commit(&sole, "a", "2020-01-01T00:00:00Z");
        run(
            &sole,
            &["tag", "-a", "-m", "r", "v1.0.1"],
            "2020-01-01T00:00:00Z",
        );
        commit(&sole, "b", "2020-05-01T00:00:00Z");
        let sole_pin = commit(&sole, "c", "2020-08-20T08:00:00+11:00");
        // Pinned off every branch and tag: fetched by name.
        run(
            &sole,
            &["reset", "-q", "--hard", "HEAD~1"],
            "2020-08-20T08:00:00Z",
        );

        let upstream = new_repo("EternalTerminal");
        std::fs::create_dir_all(upstream.join("external/libvterm")).unwrap();
        std::fs::write(upstream.join("external/libvterm/vterm.c"), "x").unwrap();
        std::fs::write(
            upstream.join("external/libvterm/LICENSE"),
            "Some license askalono does not know\n",
        )
        .unwrap();
        std::fs::write(
            upstream.join("CMakeLists.txt"),
            "add_subdirectory(external/sole)\n",
        )
        .unwrap();
        let url = |d: &Path| format!("file://{}", d.display());
        std::fs::write(
            upstream.join(".gitmodules"),
            format!(
                "[submodule \"PlatformFolders\"]\n\tpath = external/PlatformFolders\n\turl = {}\n\
                 [submodule \"sole\"]\n\tpath = external/sole\n\turl = {}\n",
                url(&folders),
                url(&sole)
            ),
        )
        .unwrap();
        let date = "2026-01-01T00:00:00Z";
        run(&upstream, &["add", "."], date);
        for (pin, path) in [
            (folders_pin.as_str(), "external/PlatformFolders"),
            (sole_pin.as_str(), "external/sole"),
            // A gitlink .gitmodules forgot.
            (folders_pin.as_str(), "external/orphan"),
        ] {
            run(
                &upstream,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("160000,{pin},{path}"),
                ],
                date,
            );
        }
        run(&upstream, &["commit", "-q", "-m", "release"], date);
        run(&upstream, &["tag", "et-v7.1.1"], date);

        let spec = parse_spec(
            "Provides: bundled(platformfolders) = 4.0.0\n\
             Provides: bundled(sole) = 1.0.1^20200819git0000000\n\
             Provides: bundled(libvterm) = 0.3\n",
        );
        let cache = tmp.path().join("cache");
        let report = report(&cache, &url(&upstream), "et-v7.1.1", Some(&spec), false).unwrap();
        let got: Vec<(&str, Option<&str>, bool)> = report
            .pins
            .iter()
            .map(|p| (p.name.as_str(), p.version.as_deref(), p.referenced))
            .collect();
        let sole_short: String = sole_pin.chars().take(7).collect();
        let sole_version = format!("1.0.1^20200819git{sole_short}");
        assert_eq!(
            got,
            [
                ("orphan", None, false),
                // The spec's spelling of the name is kept.
                ("platformfolders", Some("4.3.0"), false),
                ("sole", Some(sole_version.as_str()), true),
            ]
        );
        assert!(
            report.pins[0]
                .error
                .as_deref()
                .is_some_and(|e| e.contains(".gitmodules names no URL")),
            "{:?}",
            report.pins[0].error
        );
        assert_eq!(report.unpinned, ["external/libvterm"]);
        assert_eq!(
            report.directories,
            ["libvterm", "orphan", "PlatformFolders", "sole"]
        );
        // PlatformFolders and sole differ; libvterm is the packager's.
        assert_eq!(report.disagreements.len(), 2, "{:?}", report.disagreements);
        let text = render(&report);
        assert!(
            text.contains("Provides:       bundled(platformfolders) = 4.3.0"),
            "{text}"
        );
        assert!(
            text.contains("# bundled(orphan): .gitmodules names no URL"),
            "{text}"
        );
        assert!(!text.contains("# orphan:"), "said once: {text}");
        assert!(text.contains("possibly never built"), "{text}");

        // Each component's license, from its files at the pin.
        assert!(report.licenses_identified);
        let licenses: Vec<Option<&str>> = report
            .pins
            .iter()
            .map(|p| p.licenses.expression.as_deref())
            .collect();
        assert_eq!(licenses, [None, Some("MIT"), Some("MIT AND Zlib")]);
        let sole_files: Vec<&str> = report.pins[2]
            .licenses
            .files
            .iter()
            .map(|f| f.path.as_str())
            .collect();
        assert_eq!(sole_files, ["LICENSES/MIT.txt", "LICENSES/Zlib.txt"]);
        assert_eq!(
            report.pins[1].licenses.files,
            [LicenseFile {
                path: "LICENSE".to_string(),
                license: Some("MIT".to_string()),
            }]
        );
        assert!(
            report.pins[0]
                .licenses
                .error
                .as_deref()
                .is_some_and(|e| e.contains(".gitmodules names no URL")),
            "{:?}",
            report.pins[0].licenses
        );
        assert_eq!(
            report.unpinned_licenses["external/libvterm"].files,
            [LicenseFile {
                path: "LICENSE".to_string(),
                license: None,
            }]
        );
        assert!(
            text.contains(
                "# platformfolders: MIT, from LICENSE\nProvides:       bundled(platformfolders)"
            ),
            "{text}"
        );
        assert!(
            text.contains("# sole: MIT AND Zlib, from LICENSES/MIT.txt, LICENSES/Zlib.txt"),
            "{text}"
        );
        assert!(
            text.contains("version by hand):\n  - external/libvterm (LICENSE not identified)\n"),
            "{text}"
        );
        assert!(text.contains("License: has to cover"), "{text}");

        // askalono failing: the files are named, no license is.
        fake_askalono(&bin, Fake::Fails);
        let unidentified =
            super::report(&cache, &url(&upstream), "et-v7.1.1", None, false).unwrap();
        assert!(!unidentified.licenses_identified);
        assert_eq!(unidentified.pins[1].licenses.expression, None);
        let text = render(&unidentified);
        assert!(
            text.contains("# PlatformFolders: license in LICENSE"),
            "{text}"
        );
        assert!(text.contains("askalono did not run"), "{text}");
        // Answers that name other files are not taken.
        fake_askalono(&bin, Fake::Misnames);
        let misnamed = super::report(&cache, &url(&upstream), "et-v7.1.1", None, false).unwrap();
        assert!(!misnamed.licenses_identified);
        // Not installed: said before anything is cloned.
        std::fs::remove_file(bin.join("askalono")).unwrap();
        if !askalono_available() {
            let without = super::report(&cache, &url(&upstream), "et-v7.1.1", None, false).unwrap();
            assert!(!without.licenses_identified);
            assert_eq!(without.pins[1].licenses.files.len(), 1);
        }
        fake_askalono(&bin, Fake::Works);

        // Upstream as a local checkout is read in place, and gives the
        // same pins; its origin is what relative URLs would follow.
        let local = super::report(
            &cache,
            &upstream.to_string_lossy(),
            "et-v7.1.1",
            None,
            false,
        )
        .unwrap();
        assert_eq!(local.pins.len(), 3);
        assert_eq!(local.pins[1].version.as_deref(), Some("4.3.0"));
        run(
            &upstream,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/MisterTea/EternalTerminal",
            ],
            date,
        );
        let (dir, origin) =
            upstream_clone(&cache, &upstream.to_string_lossy(), "et-v7.1.1").unwrap();
        assert_eq!(dir, upstream);
        assert_eq!(origin, "https://github.com/MisterTea/EternalTerminal");
        let err = upstream_clone(&cache, &upstream.to_string_lossy(), "v9").unwrap_err();
        assert!(err.contains("v9 is not in the checkout"), "{err}");

        // A second run reuses the clones, with upstream gone: what the
        // cache holds is all it needs.
        std::fs::remove_dir_all(&upstream).unwrap();
        std::fs::remove_dir_all(&sole).unwrap();
        std::fs::remove_dir_all(&folders).unwrap();
        let again = super::report(&cache, &url(&upstream), "et-v7.1.1", None, false).unwrap();
        assert_eq!(again.pins.len(), 3);
        assert_eq!(again.pins[1].version.as_deref(), Some("4.3.0"));
        assert_eq!(
            again.pins[2].licenses.expression.as_deref(),
            Some("MIT AND Zlib")
        );
    }
}
