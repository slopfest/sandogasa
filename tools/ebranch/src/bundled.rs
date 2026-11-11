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
    if !gone.is_empty() {
        let _ = writeln!(out, "\nNot shipped, so no Provides: {}", gone.join(", "));
    }
    let unbuilt: Vec<&str> = shipped
        .iter()
        .filter(|p| !p.referenced)
        .map(|p| p.name.as_str())
        .collect();
    if !unbuilt.is_empty() {
        let _ = writeln!(
            out,
            "\nNot mentioned in upstream's build files at {}, so possibly never built \
             (remove in %prep if so): {}",
            report.tag,
            unbuilt.join(", ")
        );
    }
    if !report.unpinned.is_empty() {
        let _ = writeln!(
            out,
            "\nCopied in without a submodule, so with no pin to read (version by hand): {}",
            report.unpinned.join(", ")
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
            let _ = writeln!(out, "  {line}");
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
    let versions = read_pins(base, &subs, verbose);

    let (unpinned, bundled_dirs) = bundle_layout(&entries, &gitlinks);
    let mut pins: Vec<Pin> = subs
        .iter()
        .zip(versions)
        .map(|((path, url, commit), version)| {
            let base_name = dir_name(path).to_string();
            let name = spec
                .and_then(|s| {
                    s.provides
                        .iter()
                        .find(|(n, _)| n.eq_ignore_ascii_case(&base_name))
                        .map(|(n, _)| n.clone())
                })
                .unwrap_or_else(|| base_name.clone());
            let state = match spec {
                Some(s) if s.removed.iter().any(|d| d == &base_name) => State::Removed,
                Some(s) if s.replaced.iter().any(|d| d == &base_name) => State::Replaced,
                _ => State::Bundled,
            };
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
            }
        })
        .collect();
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
        disagreements,
        directories,
        after_prep: spec.is_some(),
    })
}

/// The `bundled-provides` subcommand.
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
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
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

/// The version of `commit` in the clone at `dir`.
fn read_pin(dir: &Path, commit: &str) -> Result<String, String> {
    if git(
        Some(dir),
        &["cat-file", "-e", &format!("{commit}^{{commit}}")],
    )
    .is_err()
    {
        // Pinned off every branch and tag: ask for it by name.
        git(Some(dir), &["fetch", "--quiet", "origin", commit])
            .map_err(|e| format!("commit {commit} not found: {e}"))?;
    }
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

/// Read every pin in `subs` (path, URL, commit): each repository is
/// cloned or brought up to date once, a few at a time, then each pin is
/// read from its clone. A submodule with no URL, or whose repository
/// cannot be read, gets why.
fn read_pins(
    base: &Path,
    subs: &[(String, Option<String>, String)],
    verbose: bool,
) -> Vec<Result<String, String>> {
    const AT_ONCE: usize = 4;
    // One clone per URL, holding every commit pinned from it.
    let mut wanted: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (_, url, commit) in subs {
        if let Some(url) = url {
            wanted
                .entry(url.as_str())
                .or_default()
                .push(commit.as_str());
        }
    }
    let urls: Vec<(&str, Vec<&str>)> = wanted.into_iter().collect();
    let mut dirs: BTreeMap<&str, Result<PathBuf, String>> = BTreeMap::new();
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
                        (*url, dir)
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
            let url = url
                .as_deref()
                .ok_or_else(|| format!(".gitmodules names no URL for {path}"))?;
            match dirs.get(url) {
                Some(Ok(dir)) => read_pin(dir, commit),
                Some(Err(e)) => Err(e.clone()),
                None => Err(format!("{url} was not read")),
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
                "Not shipped, so no Provides: Catch2 (removed in %prep), easyloggingpp \
                 (replaced by the system copy in %prep)"
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

    /// End to end against local repositories standing in for upstream
    /// and its submodules: one pinned on a tag, one ahead of one, one
    /// `.gitmodules` does not name, plus a directory copied in with no
    /// submodule. No network: file URLs.
    #[test]
    #[serial_test::serial]
    fn upstream_at_the_tag_gives_every_pin() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home").to_string_lossy().into_owned();
        let _env = sandogasa_config::test_support::EnvGuard::new(&[
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
        let folders_pin = commit(&folders, "b", "2021-02-03T00:00:00Z");
        run(&folders, &["tag", "v4.3.0"], "2021-02-03T00:00:00Z");
        commit(&folders, "c", "2022-01-01T00:00:00Z");
        // One two commits past v1.0.1 (annotated), committed late on
        // 2020-08-19 in UTC but the 20th at its committer's offset.
        let sole = new_repo("sole");
        commit(&sole, "a", "2020-01-01T00:00:00Z");
        run(
            &sole,
            &["tag", "-a", "-m", "r", "v1.0.1"],
            "2020-01-01T00:00:00Z",
        );
        commit(&sole, "b", "2020-05-01T00:00:00Z");
        let sole_pin = commit(&sole, "c", "2020-08-20T08:00:00+11:00");

        let upstream = new_repo("EternalTerminal");
        std::fs::create_dir_all(upstream.join("external/libvterm")).unwrap();
        std::fs::write(upstream.join("external/libvterm/vterm.c"), "x").unwrap();
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
        assert!(text.contains("possibly never built"), "{text}");

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
    }
}
