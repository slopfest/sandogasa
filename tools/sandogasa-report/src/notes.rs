// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Notes on why a piece of work was done, kept one per file so that
//! machines syncing the notes directory never edit the same file.
//!
//! A note is a few header lines (`url:`, `date:`, `by:`, and any
//! others, which are kept as they are), a blank line, then free text:
//!
//! ```text
//! url: https://github.com/owner/repo/pull/12
//! date: 2026-11-18
//! by: Claude Code:claude-opus-5-5
//!
//! Reviewed to unblock the foo update.
//! ```
//!
//! Files live at `<dir>/<year>/<date>T<HHMM>-<host>.md`, in local
//! time, with `-2`, `-3` appended on a clash within the same minute.
//!
//! Notes are read from several directories: the public ones, which
//! may be synced, and private ones, kept local for notes on embargoed
//! work. A report shows notes from a private directory only when it is
//! not to be shared (`--private`), and an embargoed note is written
//! only to a private directory, until `notes release` moves it out
//! once the embargo has lifted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

/// One note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Note {
    /// The web page of the item the note is about; none for work no
    /// report lists (a meeting, mentoring).
    pub url: Option<String>,
    /// The day the work was done.
    pub date: NaiveDate,
    /// Who wrote the text, when it was not the person themselves
    /// (an assistant, as `Agent:model`).
    pub by: Option<String>,
    /// Headers other than `url`, `date` and `by` (such as
    /// `embargo:`), in file order; an object in JSON.
    #[serde(serialize_with = "headers_as_object")]
    pub headers: Vec<(String, String)>,
    /// The note itself.
    pub text: String,
}

/// A note read from disk, with where it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Stored {
    pub path: PathBuf,
    /// Whether it is in a private directory.
    pub private: bool,
    /// Whether its URL and text are withheld from what is printed,
    /// for an embargoed note (see [`hide_embargoed`]).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    #[serde(flatten)]
    pub note: Note,
}

/// A note as `report --notes --json` gives it: the stored note, the
/// key its URL is matched by, and whether an item in the report took
/// it.
#[derive(Debug, Clone, Serialize)]
pub struct ReportNote<'a> {
    #[serde(flatten)]
    pub stored: &'a Stored,
    /// [`url_key`] of the note's URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Whether the note is about an item the report lists (at full
    /// detail).
    pub attached: bool,
}

/// What `report --notes` does with the notes no item in the report
/// took.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Unattached {
    /// Close the report with them, grouped by where they point.
    #[default]
    Include,
    /// Leave them out.
    Exclude,
    /// Ask at the terminal; include them without one.
    Ask,
}

/// What `notes` lists.
#[derive(Debug, Default, Clone)]
pub struct Filter {
    pub since: Option<NaiveDate>,
    pub until: Option<NaiveDate>,
    pub url: Option<String>,
}

/// A directory notes are read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteDir {
    pub path: PathBuf,
    /// Kept local, for notes on embargoed work: its notes go only in
    /// reports not to be shared.
    pub private: bool,
}

/// `value` for the `name` header, which has to fit on one line.
pub fn header_value(name: &str, value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.contains(['\n', '\r']) {
        return Err(format!("--{name} has to be one non-empty line"));
    }
    Ok(value.to_string())
}

/// Parse a note file's contents.
pub fn parse(text: &str) -> Result<Note, String> {
    let mut url = None;
    let mut date = None;
    let mut by = None;
    let mut headers = Vec::new();
    let mut lines = text.lines();
    let mut ended = false;
    for line in lines.by_ref() {
        if line.trim().is_empty() {
            ended = true;
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(format!("not a header line: {line:?}"));
        };
        let key = key.trim();
        if key.is_empty() || key.contains(char::is_whitespace) {
            return Err(format!("not a header line: {line:?}"));
        }
        let value = value.trim().to_string();
        match key {
            "url" => url = Some(value),
            "date" => {
                date = Some(
                    NaiveDate::parse_from_str(&value, "%Y-%m-%d")
                        .map_err(|e| format!("date {value:?}: {e}"))?,
                )
            }
            "by" => by = Some(value),
            _ => headers.push((key.to_string(), value)),
        }
    }
    let date = date.ok_or("no date: header")?;
    let body: Vec<&str> = if ended { lines.collect() } else { Vec::new() };
    let text = body.join("\n").trim().to_string();
    if text.is_empty() {
        return Err("no text after the headers".into());
    }
    Ok(Note {
        url,
        date,
        by,
        headers,
        text,
    })
}

/// A note as its file holds it.
pub fn render(note: &Note) -> String {
    let mut out = String::new();
    if let Some(url) = &note.url {
        out.push_str(&format!("url: {url}\n"));
    }
    out.push_str(&format!("date: {}\n", note.date.format("%Y-%m-%d")));
    if let Some(by) = &note.by {
        out.push_str(&format!("by: {by}\n"));
    }
    for (key, value) in &note.headers {
        out.push_str(&format!("{key}: {value}\n"));
    }
    out.push('\n');
    for line in note.text.lines() {
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// The machine's short host name (`hostname -s`), from `raw` as the
/// kernel gives it.
pub fn short_host(raw: &str) -> Option<String> {
    let name = raw.trim().split('.').next().unwrap_or("").to_string();
    if name.is_empty() || name.contains(['/', '\\']) {
        None
    } else {
        Some(name)
    }
}

/// This machine's short host name.
pub fn host() -> Result<String, String> {
    ["/proc/sys/kernel/hostname", "/etc/hostname"]
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .find_map(|raw| short_host(&raw))
        .ok_or_else(|| {
            "cannot tell this machine's host name (read /proc/sys/kernel/hostname \
             and /etc/hostname)"
                .into()
        })
}

/// Check a configured notes directory: absolute, or under `~/`, so
/// it does not depend on where a command runs.
pub fn check_dir(dir: &str) -> Result<(), String> {
    if dir == "~" || dir.starts_with("~/") || Path::new(dir).is_absolute() {
        Ok(())
    } else {
        Err(format!(
            "{dir:?} is neither absolute nor under ~/; \
             a relative path would depend on where the command runs"
        ))
    }
}

/// Where notes go: the configured directory, `~/` expanded against
/// `home`, else `<data_dir>/sandogasa-report/notes`.
pub fn notes_dir(
    configured: Option<&str>,
    home: Option<PathBuf>,
    data_dir: Option<PathBuf>,
) -> Result<PathBuf, String> {
    match configured {
        Some(dir) => expand_dir("[notes] dir", dir, home.as_deref()),
        None => data_dir
            .map(|d| d.join("sandogasa-report").join("notes"))
            .ok_or_else(|| {
                "no notes directory: set [notes] dir in the config, or set \
                 $XDG_DATA_HOME or $HOME"
                    .into()
            }),
    }
}

/// Every directory notes are read from, in order: the public ones
/// (`[notes] dir`, then `dirs`; the data directory's when neither is
/// set), then the `private` ones. A directory may be listed once, and
/// a private one may not be inside a public one, which may be synced,
/// nor hold one.
pub fn note_dirs(
    dir: Option<&str>,
    more: &[&str],
    private: &[&str],
    home: Option<PathBuf>,
    data_dir: Option<PathBuf>,
) -> Result<Vec<NoteDir>, String> {
    let mut dirs = Vec::new();
    if dir.is_some() || more.is_empty() {
        dirs.push(NoteDir {
            path: notes_dir(dir, home.clone(), data_dir)?,
            private: false,
        });
    }
    for (key, list, private) in [
        ("[notes] dirs", more, false),
        ("[notes] private", private, true),
    ] {
        for d in list {
            dirs.push(NoteDir {
                path: expand_dir(key, d, home.as_deref())?,
                private,
            });
        }
    }
    for (i, d) in dirs.iter().enumerate() {
        if dirs[..i].iter().any(|e| e.path == d.path) {
            return Err(format!(
                "{} is listed twice in [notes]; list a directory once, under \
                 private if it is",
                d.path.display()
            ));
        }
    }
    for p in dirs.iter().filter(|d| d.private) {
        if let Some(public) = dirs
            .iter()
            .filter(|d| !d.private)
            .find(|d| p.path.starts_with(&d.path) || d.path.starts_with(&p.path))
        {
            return Err(format!(
                "the private notes directory {} and the public one {} are one \
                 inside the other; keep private notes out of a directory that \
                 may be synced",
                p.path.display(),
                public.path.display()
            ));
        }
    }
    Ok(dirs)
}

/// The directory `note` writes to: `chosen` (as `--dir` gives it),
/// which has to be one of `dirs`, else the first private one for an
/// embargoed note and the first public one for any other. An embargoed
/// note goes only to a private directory, so it is never synced.
pub fn write_dir<'a>(
    dirs: &'a [NoteDir],
    chosen: Option<&str>,
    embargoed: bool,
    home: Option<&Path>,
) -> Result<&'a NoteDir, String> {
    let dir = match chosen {
        Some(chosen) => {
            let path = expand_dir("--dir", chosen, home)?;
            dirs.iter().find(|d| d.path == path).ok_or_else(|| {
                format!(
                    "--dir {chosen} is not a notes directory; the config lists {}",
                    list_paths(dirs)
                )
            })?
        }
        None if embargoed => dirs.iter().find(|d| d.private).ok_or(
            "an embargoed note goes in a private directory, and [notes] private \
             lists none",
        )?,
        None => dirs
            .iter()
            .find(|d| !d.private)
            .ok_or("[notes] lists no public directory")?,
    };
    if embargoed && !dir.private {
        return Err(format!(
            "{} is not a private directory; an embargoed note goes in one of \
             [notes] private, so it is never synced",
            dir.path.display()
        ));
    }
    Ok(dir)
}

/// The file a note written at `now` on `host` goes to, the first one
/// not taken yet.
pub fn file_for(dir: &Path, now: NaiveDateTime, host: &str) -> PathBuf {
    let year = dir.join(now.format("%Y").to_string());
    let stem = format!("{}-{host}", now.format("%Y-%m-%dT%H%M"));
    let mut path = year.join(format!("{stem}.md"));
    let mut n = 2;
    while path.exists() {
        path = year.join(format!("{stem}-{n}.md"));
        n += 1;
    }
    path
}

/// Write `note` to a new file, never over an existing one.
pub fn save(dir: &Path, note: &Note, now: NaiveDateTime, host: &str) -> Result<PathBuf, String> {
    use std::io::Write;
    loop {
        let path = file_for(dir, now, host);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                if let Err(e) = f
                    .write_all(render(note).as_bytes())
                    .and_then(|()| f.sync_all())
                {
                    // Leave no empty or partial note behind to be synced.
                    let _ = std::fs::remove_file(&path);
                    return Err(format!("cannot write {}: {e}", path.display()));
                }
                return Ok(path);
            }
            // Taken between the look and the write: try the next name.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("cannot write {}: {e}", path.display())),
        }
    }
}

/// The text `note` records: the words given, joined by spaces, or
/// standard input for a lone `-`.
pub fn text_from(words: &[String], stdin: &mut dyn std::io::Read) -> Result<String, String> {
    let text = if words.len() == 1 && words[0] == "-" {
        let mut s = String::new();
        stdin
            .read_to_string(&mut s)
            .map_err(|e| format!("cannot read the note from standard input: {e}"))?;
        s
    } else {
        words.join(" ")
    };
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("the note is empty".into());
    }
    Ok(text)
}

/// Show `note` and where it would go through `confirm`, and write it
/// if that answers yes. `None` when it was declined.
pub fn record(
    dir: &Path,
    note: &Note,
    now: NaiveDateTime,
    host: &str,
    confirm: &mut dyn FnMut(&str) -> Result<bool, String>,
) -> Result<Option<PathBuf>, String> {
    let preview = format!(
        "{}\nTo {}\n",
        render(note),
        file_for(dir, now, host).display()
    );
    if !confirm(&preview)? {
        return Ok(None);
    }
    save(dir, note, now, host).map(Some)
}

/// Every note under `dir`, oldest first, and a warning for each file
/// that could not be read as one. A missing directory holds no notes.
pub fn load_all(dir: &Path) -> (Vec<Stored>, Vec<String>) {
    let mut found = Vec::new();
    let mut warnings = Vec::new();
    let Ok(years) = std::fs::read_dir(dir) else {
        return (found, warnings);
    };
    let mut files = Vec::new();
    for year in years.flatten() {
        let Ok(entries) = std::fs::read_dir(year.path()) else {
            continue;
        };
        files.extend(
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "md")),
        );
    }
    files.sort();
    for path in files {
        match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| parse(&t))
        {
            Ok(note) => found.push(Stored {
                path,
                private: false,
                hidden: false,
                note,
            }),
            Err(e) => warnings.push(format!("skipping {}: {e}", path.display())),
        }
    }
    found.sort_by(|a, b| (a.note.date, &a.path).cmp(&(b.note.date, &b.path)));
    (found, warnings)
}

/// Every note in `dirs`, oldest first, each marked private when its
/// directory is, and a warning for each file that could not be read
/// as one.
pub fn load_dirs(dirs: &[NoteDir]) -> (Vec<Stored>, Vec<String>) {
    let mut found = Vec::new();
    let mut warnings = Vec::new();
    for dir in dirs {
        let (notes, w) = load_all(&dir.path);
        found.extend(notes.into_iter().map(|s| Stored {
            private: dir.private,
            ..s
        }));
        warnings.extend(w);
    }
    found.sort_by(|a, b| (a.note.date, &a.path).cmp(&(b.note.date, &b.path)));
    (found, warnings)
}

/// The notes `filter` selects.
pub fn select(notes: Vec<Stored>, filter: &Filter) -> Vec<Stored> {
    notes
        .into_iter()
        .filter(|s| filter.since.is_none_or(|d| s.note.date >= d))
        .filter(|s| filter.until.is_none_or(|d| s.note.date <= d))
        .filter(|s| {
            filter
                .url
                .as_deref()
                .is_none_or(|u| s.note.url.as_deref() == Some(u))
        })
        .collect()
}

/// Notes as `notes` prints them: the date and URL, marked when the
/// note is in a private directory, then the text indented, with who
/// wrote it.
pub fn format_list(notes: &[Stored]) -> String {
    let mut out = String::new();
    for (i, s) in notes.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let n = &s.note;
        out.push_str(&n.date.to_string());
        if let Some(url) = &n.url {
            out.push_str(&format!(" {url}"));
        }
        if s.private {
            out.push_str(" (private)");
        }
        out.push('\n');
        for line in n.text.lines() {
            if line.trim().is_empty() {
                out.push('\n');
            } else {
                out.push_str(&format!("  {line}\n"));
            }
        }
        if let Some(by) = &n.by {
            out.push_str(&format!("  (by {by})\n"));
        }
    }
    out
}

/// What two URLs for the same item have in common: no scheme, a
/// lowercase host, no fragment, trailing slash or tracking parameters
/// (`utm_*`, `notification_referrer_id`); a Bugzilla bug as its
/// `show_bug.cgi?id=N` page, from any form (`rhbz#N`, `/N`, other
/// query parameters); a GitHub API URL as its web page; and on GitHub
/// and GitLab, whose names ignore case, a lowercase path.
pub fn url_key(url: &str) -> String {
    let url = url.trim();
    if let Some(id) = rhbz_id(url) {
        return format!("bugzilla.redhat.com/show_bug.cgi?id={id}");
    }
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let rest = rest.split('#').next().unwrap_or("");
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_lowercase();
    let path = path.trim_end_matches('/');
    let params: Vec<&str> = query
        .split('&')
        .filter(|kv| {
            !kv.is_empty()
                && !kv.starts_with("utm_")
                && !kv.starts_with("notification_referrer_id=")
        })
        .collect();
    let bug_id = params
        .iter()
        .find_map(|kv| kv.strip_prefix("id="))
        .filter(|id| !id.is_empty());
    if let (true, Some(id)) = (path.ends_with("show_bug.cgi"), bug_id) {
        return format!("{host}/{path}?id={id}");
    }
    // debbugs's long form of a bug's page, as its short one (`/N`); its
    // links separate parameters with `;` as well as `&`.
    if path == "cgi-bin/bugreport.cgi"
        && let Some(n) = query
            .split(['&', ';'])
            .find_map(|kv| kv.strip_prefix("bug="))
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    {
        return format!("{host}/{n}");
    }
    if host.starts_with("bugzilla.") && !path.is_empty() && path.bytes().all(|b| b.is_ascii_digit())
    {
        return format!("{host}/show_bug.cgi?id={path}");
    }
    if host == "api.github.com" {
        let parts: Vec<&str> = path.split('/').collect();
        if let ["repos", owner, repo, kind, n] = parts[..] {
            let kind = match kind {
                "pulls" => "pull",
                "commits" => "commit",
                other => other,
            };
            return format!("github.com/{owner}/{repo}/{kind}/{n}").to_lowercase();
        }
    }
    // GitLab gives a project's issue two pages: `/-/issues/N` and, as
    // a work item, `/-/work_items/N`. A group's work item (an epic) has
    // no issue page.
    let path = match path.split_once("/-/work_items/") {
        Some((project, n)) if !project.starts_with("groups/") => {
            format!("{project}/-/issues/{n}")
        }
        _ => path.to_string(),
    };
    let mut key = if path.is_empty() {
        host.clone()
    } else {
        format!("{host}/{path}")
    };
    if !params.is_empty() {
        key = format!("{key}?{}", params.join("&"));
    }
    if host == "github.com" || host.contains("gitlab") {
        key = key.to_lowercase();
    }
    key
}

/// `url` with the prefix `moved` maps it from replaced: each entry
/// maps an old `host/path` to a new one (a scheme on either is
/// ignored), matched on whole path segments and without regard to
/// ASCII case, the longest one winning. `None` when no entry covers
/// `url`.
pub fn moved_url(url: &str, moved: &BTreeMap<String, String>) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (scheme, rest),
        None => ("https", url),
    };
    // A GitHub API URL is matched as the web page it stands for, and
    // stays an API URL.
    const API: &str = "api.github.com/repos/";
    if rest.len() > API.len() && rest[..API.len()].eq_ignore_ascii_case(API) {
        let web = format!("github.com/{}", &rest[API.len()..]);
        let moved_web = moved_url(&web, moved)?;
        let path = moved_web.strip_prefix("https://")?;
        return Some(match path.strip_prefix("github.com/") {
            Some(repo_path) => format!("{scheme}://{API}{repo_path}"),
            None => format!("{scheme}://{path}"),
        });
    }
    let (old_len, new) = moved
        .iter()
        .filter_map(|(old, new)| {
            let old = bare_prefix(old);
            let head = rest.get(..old.len())?;
            let after = &rest[old.len()..];
            (!old.is_empty()
                && head.eq_ignore_ascii_case(old)
                && (after.is_empty()
                    || after.starts_with(['/', '?', '#'])
                    || after.eq_ignore_ascii_case(".git")))
            .then_some((old.len(), bare_prefix(new)))
        })
        .max_by_key(|(len, _)| *len)?;
    Some(format!("{scheme}://{new}{}", &rest[old_len..]))
}

/// `url` after every move `moved` gives it, in turn, for an item that
/// moved more than once (`a/r` to `b/r`, then `b/r` to `c/r`); `None`
/// when no entry covers it. A loop of entries stops after ten steps.
pub fn resolved_url(url: &str, moved: &BTreeMap<String, String>) -> Option<String> {
    let mut now = moved_url(url, moved)?;
    for _ in 0..10 {
        match moved_url(&now, moved) {
            Some(next) if next != now => now = next,
            _ => break,
        }
    }
    Some(now)
}

/// Point the notes `moved` covers at where their items are now (see
/// [`resolved_url`]); the number of notes changed.
pub fn apply_moves(notes: &mut [Stored], moved: &BTreeMap<String, String>) -> usize {
    let mut changed = 0;
    for stored in notes.iter_mut() {
        let Some(url) = &stored.note.url else {
            continue;
        };
        if let Some(new) = resolved_url(url, moved)
            && new != *url
        {
            stored.note.url = Some(new);
            changed += 1;
        }
    }
    changed
}

/// The github.com repositories (`owner/repo`, lowercase, sorted, each
/// once) the notes at `indices` link into, leaving out notes from a
/// private directory and notes still embargoed on `today`, whose
/// repositories are not to be sent anywhere.
pub fn github_repos(notes: &[Stored], indices: &[usize], today: NaiveDate) -> Vec<String> {
    let mut repos: Vec<String> = indices
        .iter()
        .filter_map(|&i| notes.get(i))
        .filter(|s| !s.private && !embargoed(&s.note, today))
        .filter_map(|s| s.note.url.as_deref())
        .filter_map(|url| {
            let key = url_key(url);
            let path = key.strip_prefix("github.com/")?;
            let path = path.split(['?', '#']).next().unwrap_or("");
            let mut parts = path.split('/');
            let (owner, repo) = (parts.next()?, parts.next()?);
            let repo = repo.strip_suffix(".git").unwrap_or(repo);
            (!owner.is_empty() && !repo.is_empty()).then(|| format!("{owner}/{repo}"))
        })
        .collect();
    repos.sort();
    repos.dedup();
    repos
}

/// The moves of the github.com repositories the notes at `indices`
/// link into, as `lookup` answers for all of them at once (`owner/repo`
/// to its current name, `None` when not found), leaving out notes that
/// are private or embargoed on `today`: old prefix to new prefix, as
/// `[notes.moved]` takes them. `lookup` is not called when there is
/// nothing to ask.
pub fn github_moves(
    notes: &[Stored],
    indices: &[usize],
    today: NaiveDate,
    lookup: impl FnOnce(&[String]) -> Result<BTreeMap<String, Option<String>>, String>,
) -> Result<BTreeMap<String, String>, String> {
    let repos = github_repos(notes, indices, today);
    if repos.is_empty() {
        return Ok(BTreeMap::new());
    }
    Ok(lookup(&repos)?
        .into_iter()
        .filter_map(|(old, now)| {
            let now = now?;
            (!now.eq_ignore_ascii_case(&old))
                .then(|| (format!("github.com/{old}"), format!("github.com/{now}")))
        })
        .collect())
}

/// Point the github.com notes no item in `listed` takes at where their
/// repositories are now (see [`github_moves`]); the moves found.
pub fn follow_github_moves(
    notes: &mut [Stored],
    listed: &str,
    today: NaiveDate,
    lookup: impl FnOnce(&[String]) -> Result<BTreeMap<String, Option<String>>, String>,
) -> Result<Vec<(String, String)>, String> {
    let (_, rest) = attach(listed, notes);
    let moves = github_moves(notes, &rest, today, lookup)?;
    let mut unplaced: Vec<Stored> = rest.iter().map(|&i| notes[i].clone()).collect();
    apply_moves(&mut unplaced, &moves);
    for (&i, moved) in rest.iter().zip(unplaced) {
        notes[i] = moved;
    }
    Ok(moves.into_iter().collect())
}

/// The notes among `notes` whose URL `moved` covers, with the URL
/// each would get: (index, new URL).
pub fn planned_rewrites(
    notes: &[Stored],
    moved: &BTreeMap<String, String>,
) -> Vec<(usize, String)> {
    notes
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let url = s.note.url.as_deref()?;
            let new = resolved_url(url, moved)?;
            (new != url).then_some((i, new))
        })
        .collect()
}

/// The list `notes --rewrite-moved` asks about: per note, its date,
/// file, and old and new URL, except that a note still embargoed on
/// `today` is only counted unless `show`, since its URL names the
/// project.
pub fn format_rewrites(
    notes: &[Stored],
    plan: &[(usize, String)],
    today: NaiveDate,
    show: bool,
) -> String {
    let mut out = String::new();
    let mut hidden = 0;
    for (i, new) in plan {
        let stored = &notes[*i];
        if !show && embargoed(&stored.note, today) {
            hidden += 1;
            continue;
        }
        out.push_str(&format!(
            "{} {}\n  {}\n  -> {new}\n",
            stored.note.date,
            stored.path.display(),
            stored.note.url.as_deref().unwrap_or_default()
        ));
    }
    if hidden > 0 {
        out.push_str(&format!(
            "{hidden} embargoed note(s) would change too; --show-embargoed lists them.\n"
        ));
    }
    out
}

/// Change the `url:` header of the note file at `path` to `new`,
/// keeping the rest of the file byte for byte, its permissions, and a
/// symlink (the file it points to is changed). The header is found as
/// [`parse`] reads it: the last `url` key among the lines before the
/// first blank one. Written to a temporary file beside it and renamed
/// over it, so a reader never sees half a note.
pub fn rewrite_url(path: &Path, new: &str) -> Result<(), String> {
    let path =
        std::fs::canonicalize(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut offset = 0;
    let mut span = None;
    for line in text.split_inclusive('\n') {
        if line.trim().is_empty() {
            break;
        }
        let body = line.trim_end_matches(['\n', '\r']);
        if body
            .split_once(':')
            .is_some_and(|(key, _)| key.trim() == "url")
        {
            span = Some((offset, offset + body.len()));
        }
        offset += line.len();
    }
    let (start, end) = span.ok_or_else(|| format!("{}: no url: header", path.display()))?;
    let out = format!("{}url: {new}{}", &text[..start], &text[end..]);
    let permissions = std::fs::metadata(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?
        .permissions();
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let written = std::fs::write(&tmp, &out)
        .and_then(|()| std::fs::set_permissions(&tmp, permissions))
        .and_then(|()| std::fs::rename(&tmp, &path));
    written.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot rewrite {}: {e}", path.display())
    })
}

/// Add `moves` (old prefix, new prefix) to `[notes.moved]` in the
/// user config `cf`, keeping everything else the file holds.
pub fn save_moves(
    cf: &sandogasa_config::ConfigFile,
    moves: &[(String, String)],
) -> Result<(), String> {
    let moved: toml::Table = moves
        .iter()
        .map(|(old, new)| (old.clone(), toml::Value::String(new.clone())))
        .collect();
    let mut notes = toml::Table::new();
    notes.insert("moved".into(), toml::Value::Table(moved));
    let mut root = toml::Table::new();
    root.insert("notes".into(), toml::Value::Table(notes));
    cf.save(&toml::Value::Table(root))
        .map_err(|e| format!("cannot write {}: {e}", cf.path().display()))
}

/// The bug number of a Red Hat Bugzilla short form: `rhbz#N`,
/// `rhbz N`, `RHBZ:N`, any case.
fn rhbz_id(url: &str) -> Option<&str> {
    let head = url.get(..4)?;
    if !head.eq_ignore_ascii_case("rhbz") {
        return None;
    }
    let id = url[4..].trim_start_matches(['#', ':', ' ']);
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then_some(id)
}

/// A `[notes.moved]` prefix without its scheme or a trailing `/`.
fn bare_prefix(prefix: &str) -> &str {
    let prefix = prefix.trim();
    let prefix = prefix
        .strip_prefix("https://")
        .or_else(|| prefix.strip_prefix("http://"))
        .unwrap_or(prefix);
    prefix.trim_end_matches('/')
}

/// The forge sections of a report, by their `###` heading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Forge {
    Forgejo,
    Github,
    Gitlab,
    Sourcehut,
}

impl Forge {
    fn from_heading(line: &str) -> Option<Forge> {
        match line.trim_end() {
            // Reports before the heading named Gitea too.
            "### Forgejo/Gitea" | "### Forgejo" => Some(Forge::Forgejo),
            "### GitHub" => Some(Forge::Github),
            "### GitLab" => Some(Forge::Gitlab),
            "### Sourcehut" => Some(Forge::Sourcehut),
            _ => None,
        }
    }
}

/// The forge and repository of a commit URL's key, which a report
/// counts commits under: GitLab's `/-/commit/` form, a GitHub host,
/// sourcehut's git.sr.ht, and any other `/commit/` taken as Forgejo.
fn commit_repo(key: &str) -> Option<(Forge, &str)> {
    let (host, path) = key.split_once('/')?;
    if let Some((repo, _)) = path.split_once("/-/commit/") {
        return Some((Forge::Gitlab, repo));
    }
    let (repo, _) = path.split_once("/commit/")?;
    let forge = if host.contains("github") {
        Forge::Github
    } else if host == "git.sr.ht" {
        Forge::Sourcehut
    } else {
        Forge::Forgejo
    };
    Some((forge, repo))
}

/// The forge and repository a repository's own URL key names, which a
/// note about the repository attaches under, as a commit's does: a
/// GitHub or Forgejo `owner/repo`, a sourcehut `~owner/repo`, a GitLab
/// project at any depth of groups. `.git` at the end does not count.
fn repo_url(key: &str) -> Option<(Forge, &str)> {
    let (host, path) = key.split_once('/')?;
    let path = path.strip_suffix(".git").unwrap_or(path);
    if path.is_empty() || path.contains("/-/") || path.contains('?') {
        return None;
    }
    let depth = path.split('/').count();
    let (forge, fits) = if host.contains("github") {
        (Forge::Github, depth == 2)
    } else if host == "git.sr.ht" {
        (Forge::Sourcehut, depth == 2 && path.starts_with('~'))
    } else if host.contains("gitlab") || host == "salsa.debian.org" {
        (Forge::Gitlab, depth >= 2)
    } else {
        (Forge::Forgejo, depth == 2)
    };
    fits.then_some((forge, path))
}

/// The repository a list item is about, when it opens with one in
/// backticks (``- `owner/repo`: 12``).
fn item_repo(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("- `")?;
    let (repo, after) = rest.split_once('`')?;
    after.starts_with(':').then_some(repo)
}

/// The URLs of the Markdown links on `line`, without a link title,
/// with balanced parentheses kept inside the URL.
fn link_urls(line: &str) -> Vec<&str> {
    let mut urls = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find("](") {
        let after = &rest[start + 2..];
        let mut depth = 0usize;
        let mut end = None;
        for (i, c) in after.char_indices() {
            match c {
                '(' => depth += 1,
                ')' if depth == 0 => {
                    end = Some(i);
                    break;
                }
                ')' => depth -= 1,
                _ => {}
            }
        }
        let Some(end) = end else { break };
        let target = &after[..end];
        urls.push(target.split_whitespace().next().unwrap_or(""));
        rest = &after[end + 1..];
    }
    urls
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// `note`'s text as a quote at `indent`, with who wrote it.
fn quoted(note: &Note, indent: usize) -> String {
    let pad = " ".repeat(indent);
    let mut out = String::new();
    for line in note.text.lines() {
        if line.trim().is_empty() {
            out.push_str(&format!("{pad}>\n"));
        } else {
            out.push_str(&format!("{pad}> {}\n", line.trim_end()));
        }
    }
    if let Some(by) = &note.by {
        out.push_str(&format!("{pad}> (written by {by})\n"));
    }
    out
}

/// `markdown` with each note quoted under the first list item linking
/// its URL, or for a commit, the first line about its repository in
/// its forge's commit counts (a report counts commits per repository
/// rather than listing them), and the indices of the notes no item
/// took.
pub fn attach(markdown: &str, notes: &[Stored]) -> (String, Vec<usize>) {
    let keys: Vec<Option<String>> = notes
        .iter()
        .map(|s| s.note.url.as_deref().map(url_key))
        .collect();
    let commits: Vec<Option<(Forge, String)>> = keys
        .iter()
        .map(|k| {
            k.as_deref()
                .and_then(|k| commit_repo(k).or_else(|| repo_url(k)))
                .map(|(f, r)| (f, r.to_lowercase()))
        })
        .collect();
    let lines: Vec<&str> = markdown.split_inclusive('\n').collect();
    let mut placed = vec![false; notes.len()];
    // (line the quote goes before, indent, note), in the order found.
    let mut quotes: Vec<(usize, usize, usize)> = Vec::new();
    let mut forge = None;
    let mut in_commits = false;
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("### ") {
            forge = Forge::from_heading(line);
            in_commits = false;
        } else if line.starts_with("#### ") {
            in_commits = line.starts_with("#### Commits by repo")
                || line.starts_with("#### Commits by project");
        } else if line.starts_with("## ") {
            forge = None;
            in_commits = false;
        }
        if !line.trim_start().starts_with("- ") {
            continue;
        }
        let linked: Vec<String> = link_urls(line).into_iter().map(url_key).collect();
        let repo = item_repo(line)
            .filter(|_| in_commits)
            .map(str::to_lowercase);
        let here: Vec<usize> = (0..notes.len())
            .filter(|&n| {
                if placed[n] {
                    return false;
                }
                let by_link = keys[n]
                    .as_ref()
                    .is_some_and(|k| linked.iter().any(|l| l == k));
                let by_repo = match (&commits[n], &repo, forge) {
                    (Some((f, r)), Some(line_repo), Some(here)) => *f == here && r == line_repo,
                    _ => false,
                };
                by_link || by_repo
            })
            .collect();
        if here.is_empty() {
            continue;
        }
        // After the item's own continuation lines and sub-items.
        let indent = indent_of(line);
        let mut end = i + 1;
        while end < lines.len() && !lines[end].trim().is_empty() && indent_of(lines[end]) > indent {
            end += 1;
        }
        for n in here {
            placed[n] = true;
            quotes.push((end, indent, n));
        }
    }
    // A sub-item's quote and its parent's can end at the same line:
    // the deeper one first, so each stays under its own item.
    quotes.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    let mut out = String::new();
    let mut q = quotes.iter().peekable();
    for (i, line) in lines.iter().enumerate() {
        while let Some(&&(at, indent, n)) = q.peek() {
            if at != i {
                break;
            }
            out.push_str(&quoted(&notes[n].note, indent + 2));
            q.next();
        }
        out.push_str(line);
    }
    for &(_, indent, n) in q {
        out.push_str(&quoted(&notes[n].note, indent + 2));
    }
    let unplaced = (0..notes.len()).filter(|&n| !placed[n]).collect();
    (out, unplaced)
}

/// The `embargo:` header value for `--embargo`: `until <date>` for a
/// date (YYYY-MM-DD), `until disclosed` for `disclosed`, with or
/// without the leading `until`.
pub fn embargo_value(given: &str) -> Result<String, String> {
    let given = given.trim();
    let rest = given.strip_prefix("until ").unwrap_or(given).trim();
    if rest.eq_ignore_ascii_case("disclosed") {
        return Ok("until disclosed".into());
    }
    NaiveDate::parse_from_str(rest, "%Y-%m-%d")
        .map(|d| format!("until {}", d.format("%Y-%m-%d")))
        .map_err(|_| {
            format!("--embargo {given:?}: give the day it ends (YYYY-MM-DD) or `disclosed`")
        })
}

/// Whether `note`'s embargo has lifted by `today`: its `embargo:`
/// header says `lifted`, or `until` a day that has come.
pub fn lifted(note: &Note, today: NaiveDate) -> bool {
    note.headers.iter().any(|(k, v)| {
        if !k.eq_ignore_ascii_case("embargo") {
            return false;
        }
        let mut words = v.split_whitespace();
        match words.next() {
            Some(w) if w.eq_ignore_ascii_case("lifted") => true,
            Some(w) if w.eq_ignore_ascii_case("until") => words
                .next()
                .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                .is_some_and(|d| d <= today),
            _ => false,
        }
    })
}

/// Move `stored`, whose embargo has lifted, to the same year directory
/// under `to`, its header saying `lifted <today>` unless it already
/// says when, so a shared report may carry it. The note is written
/// before the old file is removed, never over an existing one, and is
/// left only where it was when either step fails.
pub fn release(stored: &Stored, to: &Path, today: NaiveDate) -> Result<PathBuf, String> {
    use std::io::Write;
    let mut note = stored.note.clone();
    for (k, v) in &mut note.headers {
        let said = v
            .split_whitespace()
            .next()
            .is_some_and(|w| w.eq_ignore_ascii_case("lifted"));
        if k.eq_ignore_ascii_case("embargo") && !said {
            *v = format!("lifted {}", today.format("%Y-%m-%d"));
        }
    }
    let (Some(name), Some(year)) = (
        stored.path.file_name(),
        stored.path.parent().and_then(Path::file_name),
    ) else {
        return Err(format!("{}: not a note file", stored.path.display()));
    };
    let year_dir = to.join(year);
    std::fs::create_dir_all(&year_dir)
        .map_err(|e| format!("cannot create {}: {e}", year_dir.display()))?;
    // Under the same name, or the next free one (`-2`, `-3`) when a
    // public note was written on the same host in the same minute.
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut n = 1;
    let (dest, mut f) = loop {
        let dest = match n {
            1 => year_dir.join(name),
            n => year_dir.join(format!("{stem}-{n}.md")),
        };
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)
        {
            Ok(f) => break (dest, f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
            Err(e) => return Err(format!("cannot write {}: {e}", dest.display())),
        }
    };
    if let Err(e) = f
        .write_all(render(&note).as_bytes())
        .and_then(|()| f.sync_all())
    {
        let _ = std::fs::remove_file(&dest);
        return Err(format!("cannot write {}: {e}", dest.display()));
    }
    // Never in both directories: if the old file stays, so does only it.
    if let Err(e) = std::fs::remove_file(&stored.path) {
        let _ = std::fs::remove_file(&dest);
        return Err(format!(
            "cannot remove {}: {e}; left it where it was",
            stored.path.display()
        ));
    }
    Ok(dest)
}

/// Whether `note` is under an embargo not lifted by `today`: an
/// `embargo:` header (any case) that does not say `lifted`, nor
/// `until` a day that has come.
pub fn embargoed(note: &Note, today: NaiveDate) -> bool {
    note.headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("embargo"))
        && !lifted(note, today)
}

/// What a hidden note says in place of its text.
pub const HIDDEN_TEXT: &str = "(embargoed: link and text hidden; --show-embargoed shows them)";

/// `stored` with its text withheld, and its URL too once it is
/// [`shown`]; it is still placed under its item by the URL, which no
/// printed output carries.
pub fn hide(stored: Stored) -> Stored {
    Stored {
        hidden: true,
        note: Note {
            text: HIDDEN_TEXT.into(),
            ..stored.note
        },
        ..stored
    }
}

/// `notes` with each note still embargoed by `today` [`hide`]n: the
/// project its URL names, or what its text says, could give the issue
/// away on a screen or in a pasted log.
pub fn hide_embargoed(notes: Vec<Stored>, today: NaiveDate) -> Vec<Stored> {
    notes
        .into_iter()
        .map(|s| match embargoed(&s.note, today) {
            true => hide(s),
            false => s,
        })
        .collect()
}

/// `stored` as it is printed: a hidden note without its URL.
pub fn shown(stored: &Stored) -> Stored {
    let mut s = stored.clone();
    if s.hidden {
        s.note.url = None;
    }
    s
}

/// The notes a report to share may carry, and how many it leaves out:
/// only those that go under an item `listed` (the report as it will
/// be read) shows, so a note is never more private than the line it
/// sits under, and never an embargoed one.
pub fn shareable(notes: Vec<Stored>, listed: &str, today: NaiveDate) -> (Vec<Stored>, usize) {
    let (_, unlisted) = attach(listed, &notes);
    let mut keep = vec![true; notes.len()];
    for i in unlisted {
        keep[i] = false;
    }
    let total = notes.len();
    let kept: Vec<Stored> = notes
        .into_iter()
        .zip(keep)
        .filter(|(s, keep)| *keep && !embargoed(&s.note, today))
        .map(|(s, _)| s)
        .collect();
    let left_out = total - kept.len();
    (kept, left_out)
}

/// The notes no item in the report took, closing it: grouped by the
/// host their URL points at, those without one last, with a line on
/// why a note ends up here. `detail` is the report's: below 2, some
/// items are counted rather than listed.
pub fn format_section(notes: &[&Stored], detail: u8) -> String {
    if notes.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "## Notes on items not in this report\n\n\
         These notes are about items the report does not list: work in a \
         source it does not query, in a project outside its domains, or of \
         a kind it does not show.",
    );
    if detail < 2 {
        out.push_str(
            " At this detail some items are counted rather than listed, so \
             their notes are here too; `--detailed --detailed` lists them.",
        );
    }
    out.push_str("\n\n");
    let mut by_host: std::collections::BTreeMap<String, Vec<&Stored>> = Default::default();
    let mut other: Vec<&Stored> = Vec::new();
    let mut hidden: Vec<&Stored> = Vec::new();
    for s in notes {
        match s.note.url.as_deref().and_then(url_host) {
            _ if s.hidden => hidden.push(s),
            Some(host) => by_host.entry(host).or_default().push(s),
            None => other.push(s),
        }
    }
    for (host, notes) in by_host {
        out.push_str(&format!("### {host}\n\n"));
        for s in notes {
            let url = s.note.url.as_deref().unwrap_or_default();
            out.push_str(&format!("- {} <{url}>\n", s.note.date));
            out.push_str(&quoted(&s.note, 2));
        }
        out.push('\n');
    }
    for (heading, notes) in [("Without a link", other), ("Embargoed", hidden)] {
        if notes.is_empty() {
            continue;
        }
        out.push_str(&format!("### {heading}\n\n"));
        for s in notes {
            out.push_str(&format!("- {}\n", s.note.date));
            out.push_str(&quoted(&s.note, 2));
        }
        out.push('\n');
    }
    out
}

/// `dir`, from the config key or flag `key`, with `~/` expanded
/// against `home`; absolute otherwise.
fn expand_dir(key: &str, dir: &str, home: Option<&Path>) -> Result<PathBuf, String> {
    if dir == "~" || dir.starts_with("~/") {
        let home = home.ok_or_else(|| {
            format!("{key} {dir} starts with ~, but there is no home directory ($HOME is unset)")
        })?;
        return Ok(home.join(dir.trim_start_matches('~').trim_start_matches('/')));
    }
    check_dir(dir)
        .map(|()| PathBuf::from(dir))
        .map_err(|e| format!("{key} {e}"))
}

/// The directories' paths, joined by commas.
fn list_paths(dirs: &[NoteDir]) -> String {
    dirs.iter()
        .map(|d| d.path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The host `url` points at, as [`url_key`] reads it (so `rhbz#N`
/// is Bugzilla's), without a user or port.
fn url_host(url: &str) -> Option<String> {
    let key = url_key(url);
    let host = key.split(['/', '?']).next()?;
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    (!host.is_empty() && host.contains('.')).then(|| host.to_string())
}

/// Whether `answer` to a `[Y/n]` question is yes.
pub fn answered_yes(answer: &str) -> bool {
    matches!(answer.trim().to_lowercase().as_str(), "" | "y" | "yes")
}

/// Check that `--since` is not after `--until`.
pub fn check_range(filter: &Filter) -> Result<(), String> {
    match (filter.since, filter.until) {
        (Some(since), Some(until)) if since > until => {
            Err(format!("--since {since} is after --until {until}"))
        }
        _ => Ok(()),
    }
}

fn headers_as_object<S: serde::Serializer>(
    headers: &[(String, String)],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_map(headers.iter().map(|(k, v)| (k, v)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn note(url: Option<&str>, date: &str, text: &str) -> Note {
        Note {
            url: url.map(String::from),
            date: day(date),
            by: None,
            headers: Vec::new(),
            text: text.into(),
        }
    }

    #[test]
    fn a_note_reads_back_as_written() {
        let n = Note {
            url: Some("https://github.com/o/r/pull/1".into()),
            date: day("2026-11-18"),
            by: Some("Claude Code:claude-opus-5-5".into()),
            headers: vec![("embargo".into(), "until disclosed".into())],
            text: "Reviewed to unblock foo.\n\nSecond paragraph.".into(),
        };
        let text = render(&n);
        assert_eq!(
            text,
            "url: https://github.com/o/r/pull/1\n\
             date: 2026-11-18\n\
             by: Claude Code:claude-opus-5-5\n\
             embargo: until disclosed\n\
             \n\
             Reviewed to unblock foo.\n\
             \n\
             Second paragraph.\n"
        );
        assert!(text.lines().all(|l| l == l.trim_end()));
        assert_eq!(parse(&text).unwrap(), n);
    }

    #[test]
    fn a_note_without_url_or_author_has_only_a_date() {
        let n = note(None, "2026-11-18", "Mentoring session.");
        assert_eq!(render(&n), "date: 2026-11-18\n\nMentoring session.\n");
        assert_eq!(parse(&render(&n)).unwrap(), n);
    }

    #[test]
    fn trailing_spaces_are_not_written() {
        let n = note(None, "2026-11-18", "one   \ntwo\t");
        assert_eq!(render(&n), "date: 2026-11-18\n\none\ntwo\n");
    }

    #[test]
    fn malformed_notes_are_rejected() {
        assert!(parse("url: x\n\ntext").unwrap_err().contains("no date"));
        assert!(
            parse("date: 2026-13-01\n\ntext")
                .unwrap_err()
                .contains("date")
        );
        assert!(
            parse("date: 2026-11-18\n\n  \n")
                .unwrap_err()
                .contains("no text")
        );
        assert!(parse("date: 2026-11-18").unwrap_err().contains("no text"));
        assert!(parse("just text\n").unwrap_err().contains("header"));
        assert!(parse("two words: x\n").unwrap_err().contains("header"));
        assert!(parse(": x\n").unwrap_err().contains("header"));
    }

    #[test]
    fn header_values_fit_on_one_line() {
        assert_eq!(
            header_value("url", " https://x/1\n").unwrap(),
            "https://x/1"
        );
        assert!(header_value("by", "a\n\nb").unwrap_err().contains("--by"));
        assert!(header_value("url", "a\rb").is_err());
        assert!(header_value("url", " ").is_err());
    }

    #[test]
    fn answers_and_ranges() {
        assert!(answered_yes("\n"));
        assert!(answered_yes("Y\n"));
        assert!(answered_yes("yes"));
        assert!(!answered_yes("n"));
        assert!(!answered_yes("nope"));
        let f = |since, until| Filter {
            since: Some(day(since)),
            until: Some(day(until)),
            url: None,
        };
        assert!(check_range(&f("2026-11-01", "2026-11-30")).is_ok());
        assert!(check_range(&f("2026-11-18", "2026-11-18")).is_ok());
        assert!(
            check_range(&f("2026-12-01", "2026-11-01"))
                .unwrap_err()
                .contains("after")
        );
        assert!(check_range(&Filter::default()).is_ok());
    }

    #[test]
    fn urls_for_the_same_item_share_a_key() {
        let key = "github.com/o/r/pull/12";
        for url in [
            "https://github.com/o/r/pull/12",
            "http://GitHub.com/o/r/pull/12/",
            "https://github.com/o/r/pull/12#issuecomment-1",
            "https://github.com/o/r/pull/12?notification_referrer_id=x",
            "https://api.github.com/repos/o/r/pulls/12",
            " https://github.com/o/r/pull/12\n",
        ] {
            assert_eq!(url_key(url), key, "{url}");
        }
        assert_eq!(
            url_key("https://api.github.com/repos/o/r/issues/3"),
            "github.com/o/r/issues/3"
        );
        // GitLab's two pages for an issue; a group's work item keeps its.
        assert_eq!(
            url_key("https://gitlab.com/CentOS/infra/tracker/-/work_items/2018"),
            url_key("https://gitlab.com/centos/infra/tracker/-/issues/2018/")
        );
        assert_eq!(
            url_key("https://gitlab.com/groups/CentOS/-/work_items/12"),
            "gitlab.com/groups/centos/-/work_items/12"
        );
        let bug = "bugzilla.redhat.com/show_bug.cgi?id=2575659";
        for url in [
            "https://bugzilla.redhat.com/show_bug.cgi?id=2575659",
            "https://bugzilla.redhat.com/show_bug.cgi?id=2575659#c3",
            "https://bugzilla.redhat.com/show_bug.cgi?format=multiple&id=2575659",
            "https://bugzilla.redhat.com/2575659",
            "rhbz#2575659",
        ] {
            assert_eq!(url_key(url), bug, "{url}");
        }
        assert_eq!(
            url_key("https://bugzilla.opensuse.org/1234"),
            "bugzilla.opensuse.org/show_bug.cgi?id=1234"
        );
        assert_eq!(url_key("rhbz#"), "rhbz");
        assert_eq!(
            url_key("https://bodhi.fedoraproject.org/updates/FEDORA-2026-abc"),
            "bodhi.fedoraproject.org/updates/FEDORA-2026-abc"
        );
        assert_eq!(url_key("https://example.org"), "example.org");
        for url in [
            "https://bugs.debian.org/1018193",
            "https://bugs.debian.org/cgi-bin/bugreport.cgi?bug=1018193",
            "https://bugs.debian.org/cgi-bin/bugreport.cgi?msg=5;bug=1018193",
            "https://bugs.debian.org/cgi-bin/bugreport.cgi?att=0&bug=1018193#10",
        ] {
            assert_eq!(url_key(url), "bugs.debian.org/1018193", "{url}");
        }
        assert_eq!(
            url_key("https://bugs.debian.org/cgi-bin/bugreport.cgi?bug=x"),
            "bugs.debian.org/cgi-bin/bugreport.cgi?bug=x"
        );
        for form in ["Rhbz#2575659", "RHBZ:2575659", "rhbz 2575659"] {
            assert_eq!(url_key(form), bug, "{form}");
        }
        assert_eq!(
            url_key("https://api.github.com/repos/O/R/commits/ABC"),
            "github.com/o/r/commit/abc"
        );
        assert_eq!(
            url_key("https://github.com/Packit/Packit/pull/2794"),
            url_key("https://github.com/packit/packit/pull/2794")
        );
        // Elsewhere case and the query can name the item.
        assert_ne!(
            url_key("https://koji.fedoraproject.org/koji/buildinfo?buildID=1"),
            url_key("https://koji.fedoraproject.org/koji/buildinfo?buildID=2")
        );
        assert_eq!(
            url_key("https://koji.fedoraproject.org/koji/buildinfo?buildID=1&utm_source=x"),
            "koji.fedoraproject.org/koji/buildinfo?buildID=1"
        );
        assert_ne!(url_key("https://e.org/A"), url_key("https://e.org/a"));
        assert_eq!(
            url_key("https://api.github.com/user"),
            "api.github.com/user"
        );
    }

    /// Notes shaped like the first ones written by hand (2026-11-16
    /// to 2026-11-18): a PR, a ticket on a forge the report does not
    /// cover, commits, and one with no URL.
    fn real_notes() -> Vec<Stored> {
        let stored = |url: Option<&str>, text: &str| Stored {
            path: PathBuf::from("n.md"),
            private: false,
            hidden: false,
            note: Note {
                url: url.map(String::from),
                date: day("2026-11-18"),
                by: Some("Claude Code:claude-opus-5-5".into()),
                headers: Vec::new(),
                text: text.into(),
            },
        };
        vec![
            stored(
                Some("https://github.com/packit/packit/pull/2794"),
                "Found while checking links\nfor the primer.",
            ),
            stored(
                Some("https://forge.fedoraproject.org/security/tickets/issues/23"),
                "Started a backlog report.",
            ),
            stored(
                Some("https://github.com/slopfest/sandogasa/commit/8184d641"),
                "Added so a CBS build is checked.",
            ),
            stored(
                Some("https://gitlab.com/CentOS/Hyperscale/rpms/perf/-/commit/abc"),
                "Rebased perf.",
            ),
            stored(None, "Mentoring session."),
        ]
    }

    #[test]
    fn notes_go_under_their_items_and_the_rest_to_the_end() {
        let md = "\
### GitHub

#### Opened

- [o/r#1](https://github.com/o/r/pull/1) other
- [packit/packit#2794](https://github.com/packit/packit/pull/2794) Update links
  (merged)
- [packit/packit.dev#1189](https://github.com/packit/packit.dev/pull/1189) Copr

#### Merged

- [packit/packit#2794](https://github.com/packit/packit/pull/2794) Update links

#### Commits by repo

- `slopfest/sandogasa`: 33

#### Tags pushed

- `slopfest/sandogasa`: [v0.28.2](https://github.com/slopfest/sandogasa/tree/v0.28.2)

### GitLab

#### Commits by project

- `CentOS/Hyperscale/rpms/perf`: 2 authored / 3 pushed
";
        let notes = real_notes();
        let (out, rest) = attach(md, &notes);
        assert_eq!(
            out,
            "\
### GitHub

#### Opened

- [o/r#1](https://github.com/o/r/pull/1) other
- [packit/packit#2794](https://github.com/packit/packit/pull/2794) Update links
  (merged)
  > Found while checking links
  > for the primer.
  > (written by Claude Code:claude-opus-5-5)
- [packit/packit.dev#1189](https://github.com/packit/packit.dev/pull/1189) Copr

#### Merged

- [packit/packit#2794](https://github.com/packit/packit/pull/2794) Update links

#### Commits by repo

- `slopfest/sandogasa`: 33
  > Added so a CBS build is checked.
  > (written by Claude Code:claude-opus-5-5)

#### Tags pushed

- `slopfest/sandogasa`: [v0.28.2](https://github.com/slopfest/sandogasa/tree/v0.28.2)

### GitLab

#### Commits by project

- `CentOS/Hyperscale/rpms/perf`: 2 authored / 3 pushed
  > Rebased perf.
  > (written by Claude Code:claude-opus-5-5)
"
        );
        assert_eq!(rest, [1, 4]);
        let rest: Vec<&Stored> = rest.iter().map(|&i| &notes[i]).collect();
        let section = format_section(&rest, 2);
        assert!(!section.contains("At this detail"), "{section}");
        assert!(format_section(&rest, 1).contains("At this detail some items"));
        assert!(
            section.starts_with("## Notes on items not in this report\n\nThese notes are about"),
            "{section}"
        );
        assert!(
            section.ends_with(
                "### forge.fedoraproject.org\n\n\
                 - 2026-11-18 <https://forge.fedoraproject.org/security/tickets/issues/23>\n  \
                 > Started a backlog report.\n  \
                 > (written by Claude Code:claude-opus-5-5)\n\
                 \n### Without a link\n\n\
                 - 2026-11-18\n  \
                 > Mentoring session.\n  \
                 > (written by Claude Code:claude-opus-5-5)\n\n"
            ),
            "{section}"
        );
        assert_eq!(
            url_host("https://Bugs.Debian.org/1018193"),
            Some("bugs.debian.org".into())
        );
        assert_eq!(url_host("rhbz#2575659"), Some("bugzilla.redhat.com".into()));
        assert_eq!(
            url_host("https://me@git.example.org:8080/x"),
            Some("git.example.org".into())
        );
        assert_eq!(url_host("https://"), None);
        assert_eq!(format_section(&[], 2), "");
    }

    #[test]
    fn a_commit_note_goes_to_its_own_forge_commit_counts() {
        // --detailed once: per-repository counts precede the commits.
        let md = "\
### GitHub

#### Opened

- `o/r`: 1

#### Commits by repo

- `O/R`: 3

### GitLab

#### Commits by project

- `o/r`: 2 authored / 2 pushed
";
        let mut notes = real_notes();
        notes.truncate(1);
        notes[0].note.url = Some("https://gitlab.com/o/r/-/commit/abc".into());
        notes[0].note.by = None;
        notes[0].note.text = "GitLab one.".into();
        let mut github = notes[0].clone();
        github.note.url = Some("https://github.com/o/r/commit/def".into());
        github.note.text = "GitHub one.".into();
        notes.push(github);
        let (out, rest) = attach(md, &notes);
        assert!(rest.is_empty());
        assert!(out.contains("- `o/r`: 1\n\n"), "{out}");
        assert!(out.contains("- `O/R`: 3\n  > GitHub one.\n"), "{out}");
        assert!(
            out.contains("- `o/r`: 2 authored / 2 pushed\n  > GitLab one.\n"),
            "{out}"
        );
        // Outside a forge section, a repository line takes no commit.
        let (_, rest) = attach("## By repository\n\n- `o/r`: 3\n", &notes);
        assert_eq!(rest, [0, 1]);
        // A note on the repository itself goes to the same line.
        let mut repo = notes[1].clone();
        repo.note.url = Some("https://github.com/O/R".into());
        repo.note.text = "The repository.".into();
        let (out, rest) = attach(md, &[repo]);
        assert!(rest.is_empty());
        assert!(out.contains("- `O/R`: 3\n  > The repository.\n"), "{out}");
    }

    #[test]
    fn a_sub_item_keeps_its_own_note() {
        let md = "- [p](https://e.org/p)\n  - [c](https://e.org/c)\n- [q](https://e.org/q)\n";
        let mut parent = real_notes().remove(0);
        parent.note.by = None;
        parent.note.url = Some("https://e.org/p".into());
        parent.note.text = "parent".into();
        let mut child = parent.clone();
        child.note.url = Some("https://e.org/c".into());
        child.note.text = "child".into();
        let mut last = parent.clone();
        last.note.url = Some("https://e.org/q".into());
        last.note.text = "last".into();
        let (out, rest) = attach(md, &[parent, child, last]);
        assert!(rest.is_empty());
        assert_eq!(
            out,
            "- [p](https://e.org/p)\n  - [c](https://e.org/c)\n    > child\n  > parent\n\
             - [q](https://e.org/q)\n  > last\n"
        );
    }

    #[test]
    fn a_shared_report_carries_only_notes_on_listed_items() {
        let listed = "### GitHub\n\n#### Opened\n\n\
            - [packit/packit#2794](https://github.com/packit/packit/pull/2794) x\n\n\
            #### Commits by repo\n\n- `slopfest/sandogasa`: 33\n";
        let mut notes = real_notes();
        let mut embargoed_pr = notes[0].clone();
        embargoed_pr.note.headers = vec![("embargo".into(), "until disclosed".into())];
        let mut lifted = notes[0].clone();
        lifted.note.headers = vec![("embargo".into(), "lifted 2026-11-20".into())];
        lifted.note.text = "lifted".into();
        notes.push(embargoed_pr);
        notes.push(lifted);
        let texts = |v: &[Stored]| v.iter().map(|s| s.note.text.clone()).collect::<Vec<_>>();

        // The forge ticket, a commit to an unlisted repository, the
        // URL-less note and the embargoed one stay out.
        let (shared, left_out) = shareable(notes, listed, day("2026-11-20"));
        assert_eq!(
            texts(&shared),
            [
                "Found while checking links\nfor the primer.",
                "Added so a CBS build is checked.",
                "lifted"
            ]
        );
        assert_eq!(left_out, 4);
    }

    #[test]
    fn an_embargo_is_read_in_any_case() {
        let with = |k: &str, v: &str| {
            let mut n = note(None, "2026-11-18", "x");
            n.headers = vec![(k.into(), v.into())];
            n
        };
        assert!(embargoed(
            &with("Embargo", "until disclosed"),
            day("2026-11-20")
        ));
        assert!(embargoed(
            &with("EMBARGO", "until 2026-12-01"),
            day("2026-11-20")
        ));
        assert!(embargoed(
            &with("embargo", "liftedness unknown"),
            day("2026-11-20")
        ));
        assert!(embargoed(&with("embargo", ""), day("2026-11-20")));
        assert!(!embargoed(
            &with("embargo", "Lifted 2026-12-01"),
            day("2026-11-20")
        ));
        assert!(!embargoed(&with("embargo", "lifted"), day("2026-11-20")));
        // Its day has come.
        assert!(!embargoed(
            &with("embargo", "until 2026-11-20"),
            day("2026-11-20")
        ));
        assert!(!embargoed(
            &with("other", "until disclosed"),
            day("2026-11-20")
        ));
        assert!(!embargoed(
            &note(None, "2026-11-18", "x"),
            day("2026-11-20")
        ));
    }

    #[test]
    fn a_paragraph_break_stays_in_the_quote() {
        let n = note(None, "2026-11-18", "One.\n\nTwo.");
        assert_eq!(quoted(&n, 0), "> One.\n>\n> Two.\n");
    }

    #[test]
    fn items_and_commits_name_their_repository() {
        assert_eq!(item_repo("- `o/r`: 3"), Some("o/r"));
        assert_eq!(item_repo("  - `o/r`: [v1](u)"), Some("o/r"));
        assert_eq!(item_repo("- `o/r` (FAS)"), None);
        assert_eq!(item_repo("- [x](u)"), None);
        assert_eq!(
            commit_repo("github.com/o/r/commit/abc"),
            Some((Forge::Github, "o/r"))
        );
        assert_eq!(
            commit_repo("gitlab.com/a/b/c/-/commit/abc"),
            Some((Forge::Gitlab, "a/b/c"))
        );
        assert_eq!(
            commit_repo("salsa.debian.org/a/b/-/commit/abc"),
            Some((Forge::Gitlab, "a/b"))
        );
        assert_eq!(
            commit_repo("git.sr.ht/~u/r/commit/abc"),
            Some((Forge::Sourcehut, "~u/r"))
        );
        assert_eq!(
            commit_repo("codeberg.org/o/r/commit/abc"),
            Some((Forge::Forgejo, "o/r"))
        );
        assert_eq!(commit_repo("github.com/o/r/pull/1"), None);
        assert_eq!(
            repo_url("github.com/slopfest/upstream-primer"),
            Some((Forge::Github, "slopfest/upstream-primer"))
        );
        assert_eq!(
            repo_url("gitlab.com/centos/hyperscale/rpms/perf.git"),
            Some((Forge::Gitlab, "centos/hyperscale/rpms/perf"))
        );
        assert_eq!(repo_url("git.sr.ht/~u/r"), Some((Forge::Sourcehut, "~u/r")));
        assert_eq!(repo_url("codeberg.org/o/r"), Some((Forge::Forgejo, "o/r")));
        // An owner, an item, a page within a project: not a repository.
        assert_eq!(repo_url("github.com/slopfest"), None);
        assert_eq!(repo_url("github.com/o/r/pull/1"), None);
        assert_eq!(repo_url("gitlab.com/a/b/-/issues/1"), None);
        assert_eq!(repo_url("bugs.debian.org/1018193"), None);
        assert_eq!(repo_url("git.sr.ht/u/r"), None);
        assert_eq!(commit_repo("nohost"), None);
        assert_eq!(link_urls("[a](u1) and [b](u2) [c](broken"), ["u1", "u2"]);
        assert_eq!(
            link_urls("[w](https://e.org/wiki/Foo_(bar)) [t](u \"title\")"),
            ["https://e.org/wiki/Foo_(bar)", "u"]
        );
    }

    #[test]
    fn the_host_name_is_the_short_one() {
        assert_eq!(short_host("box.example.org\n").as_deref(), Some("box"));
        assert_eq!(short_host("box").as_deref(), Some("box"));
        assert_eq!(short_host("\n"), None);
        assert_eq!(short_host("a/b"), None);
    }

    #[test]
    fn this_machine_has_a_host_name() {
        // Read from /proc or /etc only, never the network.
        if std::path::Path::new("/proc/sys/kernel/hostname").exists() {
            assert!(!host().unwrap().is_empty());
        }
    }

    #[test]
    fn the_notes_directory_comes_from_the_config_else_the_data_dir() {
        let home = Some(PathBuf::from("/home/u"));
        let data = Some(PathBuf::from("/home/u/.local/share"));
        assert_eq!(
            notes_dir(Some("~/Nextcloud/notes"), home.clone(), data.clone()).unwrap(),
            PathBuf::from("/home/u/Nextcloud/notes")
        );
        assert_eq!(
            notes_dir(Some("~"), home.clone(), None).unwrap(),
            PathBuf::from("/home/u")
        );
        assert_eq!(
            notes_dir(Some("/srv/notes"), None, None).unwrap(),
            PathBuf::from("/srv/notes")
        );
        assert_eq!(
            notes_dir(None, home, data).unwrap(),
            PathBuf::from("/home/u/.local/share/sandogasa-report/notes")
        );
        assert!(
            notes_dir(Some("~/x"), None, None)
                .unwrap_err()
                .contains("HOME")
        );
        assert!(
            notes_dir(None, None, None)
                .unwrap_err()
                .contains("XDG_DATA_HOME")
        );
        for relative in ["notes", "~user/notes", "$HOME/notes"] {
            let err = notes_dir(Some(relative), Some(PathBuf::from("/h")), None).unwrap_err();
            assert!(err.contains("neither absolute"), "{relative}: {err}");
        }
    }

    #[test]
    fn notes_written_in_the_same_minute_get_their_own_files() {
        let dir = tempfile::tempdir().unwrap();
        let now = at("2026-11-18 20:38");
        let n = note(None, "2026-11-18", "first");
        let a = save(dir.path(), &n, now, "box").unwrap();
        let b = save(dir.path(), &note(None, "2026-11-18", "second"), now, "box").unwrap();
        let c = save(dir.path(), &n, now, "box").unwrap();
        let year = dir.path().join("2026");
        assert_eq!(a, year.join("2026-11-18T2038-box.md"));
        assert_eq!(b, year.join("2026-11-18T2038-box-2.md"));
        assert_eq!(c, year.join("2026-11-18T2038-box-3.md"));
        assert_eq!(std::fs::read_to_string(&a).unwrap(), render(&n));
        assert_eq!(
            std::fs::read_to_string(&b).unwrap(),
            "date: 2026-11-18\n\nsecond\n"
        );
    }

    #[test]
    fn a_directory_that_cannot_be_created_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, "").unwrap();
        let err = save(
            &file,
            &note(None, "2026-11-18", "x"),
            at("2026-11-18 10:00"),
            "b",
        )
        .unwrap_err();
        assert!(err.contains("cannot create"), "{err}");
    }

    #[test]
    fn the_text_comes_from_the_words_or_standard_input() {
        let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let mut empty = std::io::empty();
        assert_eq!(
            text_from(&words(&["Reviewed", "to", "unblock"]), &mut empty).unwrap(),
            "Reviewed to unblock"
        );
        let mut input = "Long\n\nbrain dump.\n".as_bytes();
        assert_eq!(
            text_from(&words(&["-"]), &mut input).unwrap(),
            "Long\n\nbrain dump."
        );
        assert!(
            text_from(&words(&["-"]), &mut std::io::empty())
                .unwrap_err()
                .contains("empty")
        );
        assert!(
            text_from(&words(&[" "]), &mut empty)
                .unwrap_err()
                .contains("empty")
        );
    }

    #[test]
    fn a_note_is_written_only_once_confirmed() {
        let dir = tempfile::tempdir().unwrap();
        let n = note(Some("https://x/1"), "2026-11-18", "Why.");
        let now = at("2026-11-18 20:38");
        let mut shown = String::new();
        let declined = record(dir.path(), &n, now, "box", &mut |p| {
            shown = p.to_string();
            Ok(false)
        })
        .unwrap();
        assert_eq!(declined, None);
        let path = dir.path().join("2026").join("2026-11-18T2038-box.md");
        assert_eq!(shown, format!("{}\nTo {}\n", render(&n), path.display()));
        assert!(!path.exists());
        let written = record(dir.path(), &n, now, "box", &mut |_| Ok(true)).unwrap();
        assert_eq!(written, Some(path.clone()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), render(&n));
        let err = record(dir.path(), &n, now, "box", &mut |_| {
            Err("no terminal".into())
        });
        assert_eq!(err, Err("no terminal".into()));
    }

    #[test]
    fn notes_are_listed_by_date_with_bad_files_skipped() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_all(&dir.path().join("none")), (vec![], vec![]));
        let p = dir.path();
        save(
            p,
            &note(Some("https://x/2"), "2026-11-19", "later"),
            at("2026-11-17 09:00"),
            "a",
        )
        .unwrap();
        save(
            p,
            &note(None, "2025-12-01", "earlier"),
            at("2025-12-01 09:00"),
            "a",
        )
        .unwrap();
        save(
            p,
            &note(Some("https://x/1"), "2026-11-18", "middle"),
            at("2026-11-18 09:00"),
            "b",
        )
        .unwrap();
        std::fs::write(p.join("2026").join("broken.md"), "nonsense\n").unwrap();
        std::fs::write(p.join("2026").join("readme.txt"), "not a note").unwrap();
        std::fs::write(p.join("stray.md"), "date: 2026-01-01\n\nnot in a year\n").unwrap();
        let (found, warnings) = load_all(p);
        let texts: Vec<&str> = found.iter().map(|s| s.note.text.as_str()).collect();
        assert_eq!(texts, ["earlier", "middle", "later"]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("broken.md"), "{warnings:?}");

        let only = |f: Filter| -> Vec<String> {
            select(found.clone(), &f)
                .into_iter()
                .map(|s| s.note.text)
                .collect()
        };
        assert_eq!(
            only(Filter {
                since: Some(day("2026-01-01")),
                ..Default::default()
            }),
            ["middle", "later"]
        );
        assert_eq!(
            only(Filter {
                until: Some(day("2026-11-18")),
                ..Default::default()
            }),
            ["earlier", "middle"]
        );
        assert_eq!(
            only(Filter {
                url: Some("https://x/2".into()),
                ..Default::default()
            }),
            ["later"]
        );
    }

    #[test]
    fn the_listing_shows_date_url_text_and_author() {
        let mut a = note(Some("https://x/1"), "2026-11-18", "Why.\n\nMore.");
        a.by = Some("Claude Code:claude-opus-5-5".into());
        let b = note(None, "2026-11-19", "Meeting.");
        let stored = |note| Stored {
            path: PathBuf::from("p"),
            private: false,
            hidden: false,
            note,
        };
        assert_eq!(
            format_list(&[stored(a), stored(b)]),
            "2026-11-18 https://x/1\n  Why.\n\n  More.\n  (by Claude Code:claude-opus-5-5)\n\
             \n2026-11-19\n  Meeting.\n"
        );
    }

    #[test]
    fn json_carries_the_path_and_every_field() {
        let s = Stored {
            path: PathBuf::from("/n/2026/x.md"),
            private: false,
            hidden: false,
            note: note(Some("https://x/1"), "2026-11-18", "Why."),
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["path"], "/n/2026/x.md");
        assert_eq!(v["url"], "https://x/1");
        assert_eq!(v["date"], "2026-11-18");
        assert_eq!(v["text"], "Why.");
        assert!(v["by"].is_null());
        let mut n = note(None, "2026-11-18", "x");
        n.headers = vec![("embargo".into(), "until disclosed".into())];
        let v = serde_json::to_value(&n).unwrap();
        assert_eq!(v["headers"]["embargo"], "until disclosed");
    }
    #[test]
    fn notes_are_read_from_the_public_then_the_private_directories() {
        let home = Some(PathBuf::from("/home/u"));
        let data = Some(PathBuf::from("/home/u/.local/share"));
        let dir = |p: &str, private| NoteDir {
            path: PathBuf::from(p),
            private,
        };
        assert_eq!(
            note_dirs(
                Some("~/Nextcloud/notes"),
                &["/srv/team"],
                &["~/src/embargoed/notes"],
                home.clone(),
                data.clone()
            )
            .unwrap(),
            [
                dir("/home/u/Nextcloud/notes", false),
                dir("/srv/team", false),
                dir("/home/u/src/embargoed/notes", true),
            ]
        );
        // No public one set: the data directory's, still first.
        assert_eq!(
            note_dirs(None, &[], &["/p"], home.clone(), data.clone()).unwrap(),
            [
                dir("/home/u/.local/share/sandogasa-report/notes", false),
                dir("/p", true)
            ]
        );
        let err =
            note_dirs(Some("~/n"), &[], &["/home/u/n"], home.clone(), data.clone()).unwrap_err();
        assert!(err.contains("listed twice"), "{err}");
        // A private directory inside a public one, or holding one.
        for (public, private) in [("~/sync", "~/sync/embargoed"), ("~/sync/x", "~/sync")] {
            let err =
                note_dirs(Some(public), &[], &[private], home.clone(), data.clone()).unwrap_err();
            assert!(err.contains("one inside the other"), "{err}");
        }
        // Each entry's own key in its error.
        let err = note_dirs(Some("notes"), &[], &[], home.clone(), data.clone()).unwrap_err();
        assert!(err.starts_with("[notes] dir "), "{err}");
        let err = note_dirs(None, &["notes"], &[], home.clone(), data.clone()).unwrap_err();
        assert!(err.starts_with("[notes] dirs "), "{err}");
        let err = note_dirs(None, &[], &["private"], home, data).unwrap_err();
        assert!(
            err.contains("[notes] private") && err.contains("neither absolute"),
            "{err}"
        );
    }

    #[test]
    fn an_embargoed_note_is_written_only_to_a_private_directory() {
        let home = Path::new("/home/u");
        let dirs = [
            NoteDir {
                path: PathBuf::from("/home/u/synced"),
                private: false,
            },
            NoteDir {
                path: PathBuf::from("/home/u/local"),
                private: true,
            },
        ];
        let path = |r: Result<&NoteDir, String>| r.unwrap().path.clone();
        assert_eq!(
            path(write_dir(&dirs, None, false, Some(home))),
            dirs[0].path
        );
        assert_eq!(path(write_dir(&dirs, None, true, Some(home))), dirs[1].path);
        assert_eq!(
            path(write_dir(&dirs, Some("~/local"), false, Some(home))),
            dirs[1].path
        );
        let err = write_dir(&dirs, Some("~/synced"), true, Some(home)).unwrap_err();
        assert!(err.contains("not a private directory"), "{err}");
        let err = write_dir(&dirs, Some("/elsewhere"), false, Some(home)).unwrap_err();
        assert!(err.contains("/home/u/synced, /home/u/local"), "{err}");
        let err = write_dir(&dirs[..1], None, true, Some(home)).unwrap_err();
        assert!(err.contains("lists none"), "{err}");
        let err = write_dir(&dirs[1..], None, false, Some(home)).unwrap_err();
        assert!(err.contains("no public directory"), "{err}");
    }

    #[test]
    fn notes_from_several_directories_merge_by_date() {
        let public = tempfile::tempdir().unwrap();
        let private = tempfile::tempdir().unwrap();
        save(
            public.path(),
            &note(None, "2026-11-19", "later"),
            at("2026-11-19 10:00"),
            "box",
        )
        .unwrap();
        save(
            private.path(),
            &note(None, "2026-11-18", "earlier"),
            at("2026-11-18 10:00"),
            "box",
        )
        .unwrap();
        let dirs = [
            NoteDir {
                path: public.path().into(),
                private: false,
            },
            NoteDir {
                path: private.path().into(),
                private: true,
            },
        ];
        let (found, warnings) = load_dirs(&dirs);
        assert!(warnings.is_empty(), "{warnings:?}");
        let seen: Vec<_> = found
            .iter()
            .map(|s| (s.note.text.as_str(), s.private))
            .collect();
        assert_eq!(seen, [("earlier", true), ("later", false)]);
        assert!(format_list(&found).starts_with("2026-11-18 (private)\n"));
        assert_eq!(serde_json::to_value(&found[0]).unwrap()["private"], true);
    }

    #[test]
    fn an_embargo_is_given_as_a_day_or_disclosed() {
        assert_eq!(embargo_value("2026-12-01").unwrap(), "until 2026-12-01");
        assert_eq!(
            embargo_value("until 2026-12-01").unwrap(),
            "until 2026-12-01"
        );
        assert_eq!(embargo_value("disclosed").unwrap(), "until disclosed");
        assert_eq!(embargo_value("until Disclosed").unwrap(), "until disclosed");
        for bad in ["", "soon", "2026-13-01", "until"] {
            assert!(embargo_value(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_embargo_lifts_on_its_day_or_when_marked() {
        let today = day("2026-12-01");
        let with = |v: &str| Note {
            headers: vec![("Embargo".into(), v.into())],
            ..note(None, "2026-11-18", "x")
        };
        assert!(lifted(&with("until 2026-12-01"), today));
        assert!(lifted(&with("until 2026-11-30"), today));
        assert!(lifted(&with("lifted 2026-11-25"), today));
        assert!(!lifted(&with("until 2026-12-02"), today));
        assert!(!lifted(&with("until disclosed"), today));
        assert!(!lifted(&note(None, "2026-11-18", "x"), today));
    }

    #[test]
    fn a_released_note_moves_and_says_when_its_embargo_lifted() {
        let private = tempfile::tempdir().unwrap();
        let public = tempfile::tempdir().unwrap();
        let mut n = note(Some("https://x/1"), "2026-11-18", "Fixed the CVE.");
        n.headers = vec![("embargo".into(), "until 2026-12-01".into())];
        let path = save(private.path(), &n, at("2026-11-18 10:32"), "box").unwrap();
        let stored = Stored {
            path: path.clone(),
            private: true,
            hidden: false,
            note: n,
        };
        let dest = release(&stored, public.path(), day("2026-12-02")).unwrap();
        assert_eq!(
            dest,
            public.path().join("2026").join("2026-11-18T1032-box.md")
        );
        assert!(!path.exists());
        let moved = parse(&std::fs::read_to_string(&dest).unwrap()).unwrap();
        assert_eq!(
            moved.headers,
            [("embargo".to_string(), "lifted 2026-12-02".to_string())]
        );
        assert!(!embargoed(&moved, day("2026-12-02")));

        // Never over an existing note: the next free name, its own
        // header kept.
        let mut again = stored.clone();
        again.note.headers = vec![("embargo".into(), "lifted 2026-11-30".into())];
        std::fs::write(&path, render(&again.note)).unwrap();
        let dest = release(&again, public.path(), day("2026-12-02")).unwrap();
        assert_eq!(
            dest,
            public.path().join("2026").join("2026-11-18T1032-box-2.md")
        );
        let moved = parse(&std::fs::read_to_string(&dest).unwrap()).unwrap();
        assert_eq!(moved.headers[0].1, "lifted 2026-11-30");

        // An old file that is gone already: nothing left behind.
        let err = release(&again, public.path(), day("2026-12-02")).unwrap_err();
        assert!(err.contains("left it where it was"), "{err}");
        let left: Vec<_> = std::fs::read_dir(public.path().join("2026"))
            .unwrap()
            .collect();
        assert_eq!(left.len(), 2);
    }

    #[test]
    fn an_embargoed_note_is_printed_without_its_link_or_text() {
        let today = day("2026-11-20");
        let mut held = note(
            Some("https://github.com/o/secret/pull/1"),
            "2026-11-18",
            "The fix.",
        );
        held.headers = vec![("embargo".into(), "until disclosed".into())];
        let mut past = held.clone();
        past.headers = vec![("embargo".into(), "until 2026-11-19".into())];
        let stored = |note| Stored {
            path: PathBuf::from("p"),
            private: true,
            hidden: false,
            note,
        };
        let notes = hide_embargoed(vec![stored(held), stored(past)], today);
        assert!(notes[0].hidden && !notes[1].hidden);
        assert_eq!(notes[0].note.text, HIDDEN_TEXT);
        // Still placed under its item by the URL.
        let listed = "- [o/secret#1](https://github.com/o/secret/pull/1) x\n";
        let (md, rest) = attach(listed, &notes[..1]);
        assert!(rest.is_empty() && md.contains(HIDDEN_TEXT), "{md}");
        assert!(!md.contains("The fix."), "{md}");
        // Printed without it, in text and JSON.
        let printed: Vec<Stored> = notes.iter().map(shown).collect();
        let text = format_list(&printed[..1]);
        assert!(
            !text.contains("secret") && !text.contains("The fix."),
            "{text}"
        );
        assert!(
            text.contains("embargo") || text.contains(HIDDEN_TEXT),
            "{text}"
        );
        let json = serde_json::to_value(&printed[0]).unwrap();
        assert_eq!(json["hidden"], true);
        assert!(json.get("url").is_none_or(|u| u.is_null()), "{json}");
        assert!(!json.to_string().contains("secret"), "{json}");
        // Nor in the section of notes no item took.
        let section = format_section(&[&printed[0]], 2);
        assert!(
            !section.contains("secret") && section.contains(HIDDEN_TEXT),
            "{section}"
        );
        // One whose day has come is printed in full.
        assert!(format_list(&printed[1..]).contains("The fix."));
    }

    fn noted(url: &str, private: bool) -> Stored {
        Stored {
            path: PathBuf::from("n.md"),
            private,
            hidden: false,
            note: note(Some(url), "2026-11-18", "Why."),
        }
    }

    #[test]
    fn a_moved_prefix_rewrites_whole_segments_only() {
        let moved = BTreeMap::from([
            (
                "github.com/old/sandogasa".to_string(),
                "github.com/new/sandogasa".to_string(),
            ),
            // A scheme and a trailing slash in the table do not count.
            (
                "https://github.com/old/".to_string(),
                "github.com/elsewhere".to_string(),
            ),
        ]);
        assert_eq!(
            moved_url("https://github.com/Old/Sandogasa/pull/3", &moved).as_deref(),
            Some("https://github.com/new/sandogasa/pull/3")
        );
        assert_eq!(
            moved_url("https://github.com/old/sandogasa", &moved).as_deref(),
            Some("https://github.com/new/sandogasa")
        );
        assert_eq!(
            moved_url("http://github.com/old/sandogasa?tab=x", &moved).as_deref(),
            Some("http://github.com/new/sandogasa?tab=x")
        );
        // The longest prefix wins; a shorter one still covers the rest.
        assert_eq!(
            moved_url("https://github.com/old/hansei", &moved).as_deref(),
            Some("https://github.com/elsewhere/hansei")
        );
        // Not a whole segment.
        let only = BTreeMap::from([(
            "github.com/old/sandogasa".to_string(),
            "github.com/new/sandogasa".to_string(),
        )]);
        assert_eq!(
            moved_url("https://github.com/old/sandogasa-foo", &only),
            None
        );
        assert_eq!(moved_url("rhbz#123", &only), None);
        assert_eq!(moved_url("https://gitlab.com/old/sandogasa", &only), None);
        // A bare host/path gets https.
        assert_eq!(
            moved_url("github.com/old/sandogasa/issues/1", &only).as_deref(),
            Some("https://github.com/new/sandogasa/issues/1")
        );
        let mut notes = vec![
            noted("https://github.com/old/sandogasa/pull/1", false),
            noted("https://github.com/other/repo", false),
        ];
        notes.push(Stored {
            note: note(None, "2026-11-18", "No URL."),
            ..noted("x", false)
        });
        assert_eq!(apply_moves(&mut notes, &only), 1);
        assert_eq!(
            notes[0].note.url.as_deref(),
            Some("https://github.com/new/sandogasa/pull/1")
        );
    }

    #[test]
    fn unattached_github_notes_follow_their_repository_to_its_new_owner() {
        let report = "### GitHub\n\n#### Commits by repo\n\n- `new/hansei`: 6\n\
                      - [PR](https://github.com/kept/repo/pull/2)\n";
        let mut notes = vec![
            noted("https://github.com/old/hansei/commit/8ab1b2e", false),
            noted("https://github.com/kept/repo/pull/2", false),
            noted("https://github.com/Old/Gone", false),
            // Never sent anywhere: from a private directory.
            noted("https://github.com/secret/fix/pull/9", true),
            noted("https://gitlab.com/old/hansei", false),
            // Nor an embargoed note outside one.
            Stored {
                note: Note {
                    headers: vec![("embargo".into(), "until disclosed".into())],
                    ..note(
                        Some("https://github.com/acme/hidden/pull/3"),
                        "2026-11-18",
                        "Why.",
                    )
                },
                ..noted("x", false)
            },
            // An API URL follows its repository too, and stays one.
            noted("https://api.github.com/repos/old/hansei/pulls/4", false),
        ];
        let mut asked = Vec::new();
        let moves = follow_github_moves(
            &mut notes,
            report,
            day("2026-11-22"),
            |repos: &[String]| {
                asked = repos.to_vec();
                Ok(BTreeMap::from([
                    ("old/hansei".to_string(), Some("new/hansei".to_string())),
                    ("old/gone".to_string(), None),
                ]))
            },
        )
        .unwrap();
        // Asked once, for the unattached public ones only.
        assert_eq!(asked, ["old/gone", "old/hansei"]);
        assert_eq!(
            moves,
            [(
                "github.com/old/hansei".to_string(),
                "github.com/new/hansei".to_string()
            )]
        );
        assert_eq!(
            notes[0].note.url.as_deref(),
            Some("https://github.com/new/hansei/commit/8ab1b2e")
        );
        assert_eq!(
            notes[6].note.url.as_deref(),
            Some("https://api.github.com/repos/new/hansei/pulls/4")
        );
        let (_, rest) = attach(report, &notes);
        // Its pull request is not in this report, so it stays unattached.
        assert_eq!(rest, [2, 3, 4, 5, 6]);

        // Nothing unattached on github.com: GitHub is not asked.
        let mut placed = vec![noted("https://github.com/kept/repo/pull/2", false)];
        let none = follow_github_moves(&mut placed, report, day("2026-11-22"), |_: &[String]| {
            panic!("asked without need")
        })
        .unwrap();
        assert!(none.is_empty());
        // A failed lookup is reported and changes nothing.
        let mut notes = vec![noted("https://github.com/old/hansei", false)];
        let err = follow_github_moves(&mut notes, report, day("2026-11-22"), |_: &[String]| {
            Err("no token".into())
        });
        assert_eq!(err.unwrap_err(), "no token");
        assert_eq!(
            notes[0].note.url.as_deref(),
            Some("https://github.com/old/hansei")
        );
    }

    #[test]
    fn found_moves_are_added_to_the_config_and_nothing_else_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# mine\n[notes]\ndir = \"~/notes\"\n\n[notes.moved]\n\
             \"github.com/a/b\" = \"github.com/c/b\"\n\n[defaults]\nx = 1\n",
        )
        .unwrap();
        let cf = sandogasa_config::ConfigFile::from_path(path.clone());
        save_moves(
            &cf,
            &[(
                "github.com/old/hansei".to_string(),
                "github.com/new/hansei".to_string(),
            )],
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let cfg: crate::config::ReportConfig = toml::from_str(&text).unwrap();
        assert_eq!(
            cfg.notes.moved["github.com/old/hansei"],
            "github.com/new/hansei"
        );
        assert_eq!(cfg.notes.moved["github.com/a/b"], "github.com/c/b");
        assert_eq!(cfg.notes.dir.as_deref(), Some("~/notes"));
        assert!(text.starts_with("# mine\n"), "{text}");
        assert!(text.contains("[defaults]\nx = 1"), "{text}");
        // A file not there yet is created.
        let fresh = sandogasa_config::ConfigFile::from_path(dir.path().join("new/config.toml"));
        save_moves(
            &fresh,
            &[("github.com/x/y".into(), "github.com/z/y".into())],
        )
        .unwrap();
        assert!(
            std::fs::read_to_string(fresh.path())
                .unwrap()
                .contains("\"github.com/x/y\" = \"github.com/z/y\"")
        );
    }

    #[test]
    fn a_rewrite_changes_only_the_url_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("n.md");
        let text = "date: 2026-11-18\r\nurl: https://github.com/old/r/pull/1\r\nby: me\r\n\r\n\
                    url: https://github.com/old/r in the text stays\r\n";
        std::fs::write(&path, text).unwrap();
        rewrite_url(&path, "https://github.com/new/r/pull/1").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            text.replacen("github.com/old/r/pull/1", "github.com/new/r/pull/1", 1)
        );
        assert!(!dir.path().join("n.md.tmp").exists());
        // A url: line in the text is not a header.
        let bare = dir.path().join("bare.md");
        std::fs::write(&bare, "date: 2026-11-18\n\nurl: x\n").unwrap();
        let err = rewrite_url(&bare, "y").unwrap_err();
        assert!(err.contains("no url: header"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&bare).unwrap(),
            "date: 2026-11-18\n\nurl: x\n"
        );
        assert!(rewrite_url(&dir.path().join("gone.md"), "y").is_err());
    }

    #[test]
    fn rewrites_are_planned_from_the_moves_and_listed_without_embargoed_urls() {
        let moved = BTreeMap::from([(
            "github.com/old/r".to_string(),
            "github.com/new/r".to_string(),
        )]);
        let held = Stored {
            note: Note {
                headers: vec![("embargo".into(), "until disclosed".into())],
                ..note(
                    Some("https://github.com/old/r/pull/9"),
                    "2026-11-18",
                    "Why.",
                )
            },
            ..noted("x", true)
        };
        let notes = vec![
            noted("https://github.com/old/r/pull/1", false),
            noted("https://github.com/other/r", false),
            held,
        ];
        let plan = planned_rewrites(&notes, &moved);
        assert_eq!(
            plan,
            [
                (0, "https://github.com/new/r/pull/1".to_string()),
                (2, "https://github.com/new/r/pull/9".to_string())
            ]
        );
        let list = format_rewrites(&notes, &plan, day("2026-11-22"), false);
        assert_eq!(
            list,
            "2026-11-18 n.md\n  https://github.com/old/r/pull/1\n  \
             -> https://github.com/new/r/pull/1\n\
             1 embargoed note(s) would change too; --show-embargoed lists them.\n"
        );
        let shown = format_rewrites(&notes, &plan, day("2026-11-22"), true);
        assert!(shown.contains("pull/9"), "{shown}");
        assert!(!shown.contains("embargoed note(s)"), "{shown}");
    }

    #[test]
    fn github_moves_skip_the_lookup_when_nothing_is_on_github() {
        let notes = vec![noted("https://gitlab.com/o/r", false)];
        let none = github_moves(&notes, &[0], day("2026-11-22"), |_: &[String]| {
            panic!("asked without need")
        })
        .unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn a_rewrite_finds_the_url_header_as_parse_does() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("n.md");
        // A spaced key, a second url (parse keeps the last), and a
        // separator line of spaces before a url: in the text.
        let text = "url : https://x/first\ndate: 2026-11-18\nurl: https://x/last\n   \nurl: body\n";
        std::fs::write(&path, text).unwrap();
        rewrite_url(&path, "https://x/new").unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            out,
            "url : https://x/first\ndate: 2026-11-18\nurl: https://x/new\n   \nurl: body\n"
        );
        assert_eq!(parse(&out).unwrap().url.as_deref(), Some("https://x/new"));
    }

    #[cfg(unix)]
    #[test]
    fn a_rewrite_keeps_the_mode_and_a_symlink_and_leaves_nothing_behind() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.md");
        std::fs::write(&target, "url: https://x/old\ndate: 2026-11-18\n\nWhy.\n").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.path().join("link.md");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        rewrite_url(&link, "https://x/new").unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            std::fs::read_to_string(&target)
                .unwrap()
                .contains("https://x/new")
        );
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
    }

    #[test]
    fn moves_chain_skip_no_ops_and_cover_a_git_suffix() {
        let moved = BTreeMap::from([
            ("github.com/a/r".to_string(), "github.com/b/r".to_string()),
            ("github.com/b/r".to_string(), "github.com/c/r".to_string()),
            (
                "github.com/Same/r".to_string(),
                "github.com/same/r".to_string(),
            ),
            (
                "github.com/x/loop".to_string(),
                "github.com/y/loop".to_string(),
            ),
            (
                "github.com/y/loop".to_string(),
                "github.com/x/loop".to_string(),
            ),
        ]);
        assert_eq!(
            resolved_url("https://github.com/a/r/pull/1", &moved).as_deref(),
            Some("https://github.com/c/r/pull/1")
        );
        assert_eq!(
            moved_url("https://github.com/a/r.git", &moved).as_deref(),
            Some("https://github.com/b/r.git")
        );
        // A loop ends.
        assert!(resolved_url("https://github.com/x/loop", &moved).is_some());
        let notes = vec![
            noted("https://github.com/same/r/pull/2", false),
            noted("https://github.com/a/r", false),
        ];
        assert_eq!(
            planned_rewrites(&notes, &moved),
            [(1, "https://github.com/c/r".to_string())]
        );
    }
}
