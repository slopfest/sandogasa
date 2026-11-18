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

use std::path::{Path, PathBuf};

use chrono::{NaiveDate, NaiveDateTime};
use serde::Serialize;

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
    #[serde(flatten)]
    pub note: Note,
}

/// A note as `report --notes --json` gives it: the stored note and
/// the key its URL is matched by.
#[derive(Debug, Clone, Serialize)]
pub struct ReportNote<'a> {
    #[serde(flatten)]
    pub stored: &'a Stored,
    /// [`url_key`] of the note's URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// What `notes` lists.
#[derive(Debug, Default, Clone)]
pub struct Filter {
    pub since: Option<NaiveDate>,
    pub until: Option<NaiveDate>,
    pub url: Option<String>,
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
        Some(dir) if dir == "~" || dir.starts_with("~/") => {
            let home = home.ok_or(
                "[notes] dir starts with ~, but there is no home directory ($HOME is unset)",
            )?;
            Ok(home.join(dir.trim_start_matches('~').trim_start_matches('/')))
        }
        Some(dir) => check_dir(dir)
            .map(|()| PathBuf::from(dir))
            .map_err(|e| format!("[notes] dir {e}")),
        None => data_dir
            .map(|d| d.join("sandogasa-report").join("notes"))
            .ok_or_else(|| {
                "no notes directory: set [notes] dir in the config, or set \
                 $XDG_DATA_HOME or $HOME"
                    .into()
            }),
    }
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
            Ok(note) => found.push(Stored { path, note }),
            Err(e) => warnings.push(format!("skipping {}: {e}", path.display())),
        }
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

/// Notes as `notes` prints them: the date and URL, then the text
/// indented, with who wrote it.
pub fn format_list(notes: &[Stored]) -> String {
    let mut out = String::new();
    for (i, s) in notes.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let n = &s.note;
        match &n.url {
            Some(url) => out.push_str(&format!("{} {url}\n", n.date)),
            None => out.push_str(&format!("{}\n", n.date)),
        }
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
            "### Forgejo" => Some(Forge::Forgejo),
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
                .and_then(commit_repo)
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

/// Whether `note` is under an embargo not lifted yet: an `embargo:`
/// header (any case) whose value does not start with the word
/// `lifted`.
pub fn embargoed(note: &Note) -> bool {
    note.headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("embargo")
            && !v
                .split_whitespace()
                .next()
                .is_some_and(|w| w.eq_ignore_ascii_case("lifted"))
    })
}

/// The notes a report to share may carry, and how many it leaves out:
/// only those that go under an item `listed` (the report as it will
/// be read) shows, so a note is never more private than the line it
/// sits under, and never an embargoed one.
pub fn shareable(notes: Vec<Stored>, listed: &str) -> (Vec<Stored>, usize) {
    let (_, unlisted) = attach(listed, &notes);
    let mut keep = vec![true; notes.len()];
    for i in unlisted {
        keep[i] = false;
    }
    let total = notes.len();
    let kept: Vec<Stored> = notes
        .into_iter()
        .zip(keep)
        .filter(|(s, keep)| *keep && !embargoed(&s.note))
        .map(|(s, _)| s)
        .collect();
    let left_out = total - kept.len();
    (kept, left_out)
}

/// The report's closing section for `notes` no item showed: those
/// about something else, by URL, then those with none.
pub fn format_section(notes: &[&Stored]) -> String {
    if notes.is_empty() {
        return String::new();
    }
    let mut out = String::from("## Notes\n\n");
    let (linked, other): (Vec<&Stored>, Vec<&Stored>) =
        notes.iter().partition(|s| s.note.url.is_some());
    for s in linked {
        let url = s.note.url.as_deref().unwrap_or_default();
        out.push_str(&format!("- {} <{url}>\n", s.note.date));
        out.push_str(&quoted(&s.note, 2));
    }
    if !other.is_empty() {
        out.push_str("\n### Other notes\n\n");
        for s in other {
            out.push_str(&format!("- {}\n", s.note.date));
            out.push_str(&quoted(&s.note, 2));
        }
    }
    out.push('\n');
    out
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
        assert_eq!(
            format_section(&rest),
            "## Notes\n\n\
             - 2026-11-18 <https://forge.fedoraproject.org/security/tickets/issues/23>\n  \
             > Started a backlog report.\n  \
             > (written by Claude Code:claude-opus-5-5)\n\
             \n### Other notes\n\n\
             - 2026-11-18\n  \
             > Mentoring session.\n  \
             > (written by Claude Code:claude-opus-5-5)\n\n"
        );
        assert_eq!(format_section(&[]), "");
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
        let (shared, left_out) = shareable(notes, listed);
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
        assert!(embargoed(&with("Embargo", "until disclosed")));
        assert!(embargoed(&with("EMBARGO", "until 2026-12-01")));
        assert!(embargoed(&with("embargo", "liftedness unknown")));
        assert!(embargoed(&with("embargo", "")));
        assert!(!embargoed(&with("embargo", "Lifted 2026-12-01")));
        assert!(!embargoed(&with("embargo", "lifted")));
        assert!(!embargoed(&with("other", "until disclosed")));
        assert!(!embargoed(&note(None, "2026-11-18", "x")));
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
}
