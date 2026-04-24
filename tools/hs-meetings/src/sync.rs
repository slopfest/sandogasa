// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `sync` subcommand — fetch meetings from meetbot and merge into
//! a tool-managed meetings list markdown file (typically included
//! into `meetings.md` via pymdownx.snippets).

use std::collections::HashSet;
use std::process::ExitCode;

use chrono::{Datelike, NaiveDate};
use sandogasa_meetbot::{Meetbot, Meeting, dedup_by_longest_log};

const DEFAULT_TOPIC: &str = "centos-hyperscale-sig";

/// SIG meetings from 2023 and earlier predate the meetbot archive
/// and often carry hand-curated `[agenda](...)` links in the docs;
/// they're out of scope for this tool. Meetbot results before this
/// year are dropped so pre-existing sections stay untouched.
const MANAGED_FROM_YEAR: i32 = 2024;

#[derive(clap::Args)]
pub struct SyncArgs {
    /// Path to the tool-managed meetings list file.
    #[arg(short, long, value_name = "PATH")]
    pub file: String,

    /// Meetbot search topic.
    #[arg(short, long, default_value = DEFAULT_TOPIC)]
    pub topic: String,

    /// Start date filter (inclusive, YYYY-MM-DD).
    #[arg(long, group = "date_range")]
    pub since: Option<NaiveDate>,

    /// End date filter (inclusive, YYYY-MM-DD, default: today).
    #[arg(long, requires = "since")]
    pub until: Option<NaiveDate>,

    /// Calendar period filter — e.g. `2026`, `2026Q1`, `2026H1`.
    #[arg(long, group = "date_range")]
    pub period: Option<String>,

    /// Print planned changes without writing.
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(args: &SyncArgs) -> ExitCode {
    let range = match sandogasa_cli::date::resolve_date_range(
        args.since,
        args.until,
        args.period.as_deref(),
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let existing = match std::fs::read_to_string(&args.file) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            eprintln!("error: reading {}: {}", args.file, e);
            return ExitCode::FAILURE;
        }
    };

    let mut doc = match parse_document(&existing) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: parsing {}: {}", args.file, e);
            return ExitCode::FAILURE;
        }
    };

    let client = Meetbot::new();
    let meetings = match client.search(&args.topic) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: meetbot: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Collapse same-day `!startmeeting` retries (and
    // cross-channel overlaps) by log length. The SIG only ever
    // has one real meeting per day, so the longest log is taken
    // as the canonical one.
    let meetings = dedup_by_longest_log(&client, meetings, |winner, dropped| {
        let d = winner.datetime.date();
        eprintln!(
            "warning: {} fragments on {d}, keeping longest log:",
            dropped.len() + 1
        );
        eprintln!("  kept:    {}", winner.logs_url);
        for m in dropped {
            eprintln!("  dropped: {}", m.logs_url);
        }
    });

    let existing_dates: HashSet<NaiveDate> = doc
        .sections
        .iter()
        .flat_map(|s| s.entries.iter().map(|e| e.date))
        .collect();

    let mut added: Vec<Meeting> = meetings
        .into_iter()
        .filter(|m| {
            let d = m.datetime.date();
            d.year() >= MANAGED_FROM_YEAR
                && d >= range.0
                && d <= range.1
                && !existing_dates.contains(&d)
        })
        .collect();
    added.sort_by_key(|m| m.datetime.date());

    if added.is_empty() {
        println!("up to date ({} existing entries)", existing_dates.len());
        return ExitCode::SUCCESS;
    }

    for m in &added {
        doc.insert(m);
    }

    if args.dry_run {
        println!("would add {} entries:", added.len());
        for m in &added {
            println!("  {} {}", m.datetime.date(), m.summary_url);
        }
    } else {
        let rendered = doc.render();
        if let Err(e) = std::fs::write(&args.file, &rendered) {
            eprintln!("error: writing {}: {}", args.file, e);
            return ExitCode::FAILURE;
        }
        println!("added {} entries to {}", added.len(), args.file);
    }

    ExitCode::SUCCESS
}

struct Document {
    header: String,
    sections: Vec<Section>,
}

struct Section {
    year: i32,
    entries: Vec<Entry>,
}

struct Entry {
    date: NaiveDate,
    text: String,
}

impl Document {
    /// Insert a meeting, creating a year section if needed. Sections
    /// are kept newest-first; entries within a section are kept
    /// newest-first by date.
    fn insert(&mut self, m: &Meeting) {
        let date = m.datetime.date();
        let year = date.year();
        let entry = Entry {
            date,
            text: render_entry(m),
        };

        if let Some(section) = self.sections.iter_mut().find(|s| s.year == year) {
            let pos = section
                .entries
                .iter()
                .position(|e| e.date < date)
                .unwrap_or(section.entries.len());
            section.entries.insert(pos, entry);
        } else {
            let new_section = Section {
                year,
                entries: vec![entry],
            };
            let pos = self
                .sections
                .iter()
                .position(|s| s.year < year)
                .unwrap_or(self.sections.len());
            self.sections.insert(pos, new_section);
        }
    }

    /// Render with `### YYYY` year headings. The tool-managed
    /// meetings-list file is included underneath the `## Meeting
    /// minutes` heading in the docs, so year sections must be
    /// a deeper level to nest correctly in the site's sidebar.
    /// Any pre-existing `## YYYY` or similar headings in the file
    /// are normalized to `###` on write.
    fn render(&self) -> String {
        let mut out = self.header.clone();
        for section in &self.sections {
            out.push_str(&format!("### {}\n\n", section.year));
            for entry in &section.entries {
                out.push_str(&entry.text);
            }
            out.push('\n');
        }
        let mut trimmed = out.trim_end().to_string();
        trimmed.push('\n');
        trimmed
    }
}

fn render_entry(m: &Meeting) -> String {
    let date = m.datetime.date();
    let month_day = date.format("%b %d").to_string();
    format!(
        "* {month_day}: [summary]({}),\n          [logs]({})\n",
        m.summary_url, m.logs_url
    )
}

fn parse_document(text: &str) -> Result<Document, String> {
    let mut header_lines: Vec<&str> = Vec::new();
    let mut sections: Vec<Section> = Vec::new();
    let mut current: Option<(i32, Vec<&str>)> = None;

    for line in text.lines() {
        if let Some(year) = parse_year_heading(line) {
            if let Some((y, body)) = current.take() {
                sections.push(parse_section(y, body)?);
            }
            current = Some((year, Vec::new()));
        } else if let Some((_, body)) = current.as_mut() {
            body.push(line);
        } else {
            header_lines.push(line);
        }
    }
    if let Some((y, body)) = current {
        sections.push(parse_section(y, body)?);
    }

    let header = if header_lines.is_empty() {
        String::new()
    } else {
        let mut h = header_lines.join("\n");
        h.push('\n');
        // Guarantee a blank line between header and first section.
        if !h.ends_with("\n\n") {
            h.push('\n');
        }
        h
    };

    Ok(Document { header, sections })
}

fn parse_section(year: i32, body: Vec<&str>) -> Result<Section, String> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut current_lines: Vec<&str> = Vec::new();

    for line in body {
        if line.starts_with("* ") {
            if !current_lines.is_empty() {
                entries.push(build_entry(std::mem::take(&mut current_lines))?);
            }
            current_lines.push(line);
        } else if current_lines.is_empty() {
            // Before first entry: skip blanks.
        } else if line.trim().is_empty() {
            entries.push(build_entry(std::mem::take(&mut current_lines))?);
        } else if line.starts_with(' ') || line.starts_with('\t') {
            current_lines.push(line);
        } else {
            return Err(format!("unexpected line in {year} section: {line:?}"));
        }
    }
    if !current_lines.is_empty() {
        entries.push(build_entry(current_lines)?);
    }

    Ok(Section { year, entries })
}

fn build_entry(lines: Vec<&str>) -> Result<Entry, String> {
    let mut text = lines.join("\n");
    text.push('\n');
    let date =
        extract_date(&text).ok_or_else(|| format!("no YYYY-MM-DD date in entry: {text:?}"))?;
    Ok(Entry { date, text })
}

fn parse_year_heading(line: &str) -> Option<i32> {
    let rest = line
        .strip_prefix("## ")
        .or_else(|| line.strip_prefix("### "))?;
    let rest = rest.trim();
    if rest.len() == 4 {
        rest.parse::<i32>().ok()
    } else {
        None
    }
}

fn extract_date(text: &str) -> Option<NaiveDate> {
    // chrono's `%d` accepts a space-padded single-digit day, so we
    // require the slice to start with an ASCII digit.
    let bytes = text.as_bytes();
    if bytes.len() < 10 {
        return None;
    }
    for i in 0..=bytes.len() - 10 {
        if !bytes[i].is_ascii_digit() {
            continue;
        }
        if let Ok(s) = std::str::from_utf8(&bytes[i..i + 10])
            && let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        {
            return Some(d);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;

    fn meeting(ts: &str) -> Meeting {
        Meeting {
            channel: "c".into(),
            datetime: NaiveDateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%S").unwrap(),
            topic: "centos-hyperscale-sig".into(),
            summary_url: format!("https://meetbot.example/s/{ts}"),
            logs_url: format!("https://meetbot.example/l/{ts}"),
        }
    }

    #[test]
    fn year_heading_h2() {
        assert_eq!(parse_year_heading("## 2024"), Some(2024));
    }

    #[test]
    fn year_heading_h3() {
        assert_eq!(parse_year_heading("### 2021"), Some(2021));
    }

    #[test]
    fn year_heading_rejects_text() {
        assert_eq!(parse_year_heading("## Meeting minutes"), None);
        assert_eq!(parse_year_heading("## 12345"), None);
    }

    #[test]
    fn extract_date_finds_first_match() {
        let d = extract_date("foo 2024-03-15 bar 2024-04-01");
        assert_eq!(d, Some(NaiveDate::from_ymd_opt(2024, 3, 15).unwrap()));
    }

    #[test]
    fn extract_date_none_when_missing() {
        assert_eq!(extract_date("no dates here"), None);
    }

    #[test]
    fn parse_two_sections() {
        let text = "## 2024\n\n\
* May 22: agenda,\n          [summary](https://x/2024-05-22-16.06.html),\n          [logs](https://x/2024-05-22-16.06.log.html)\n\n\
## 2023\n\n\
* Dec 20: agenda,\n          [summary](https://y/2023-12-20-16.00.html),\n          [logs](https://y/2023-12-20-16.00.log.html)\n";
        let doc = parse_document(text).unwrap();
        assert_eq!(doc.header, "");
        assert_eq!(doc.sections.len(), 2);
        assert_eq!(doc.sections[0].year, 2024);
        assert_eq!(doc.sections[0].entries.len(), 1);
        assert_eq!(
            doc.sections[0].entries[0].date,
            NaiveDate::from_ymd_opt(2024, 5, 22).unwrap()
        );
        assert_eq!(doc.sections[1].year, 2023);
    }

    #[test]
    fn parse_header_preserved() {
        let text = "<!-- auto-generated -->\n\n## 2024\n\n\
* May 22: agenda,\n          [summary](https://x/2024-05-22-16.06.html),\n          [logs](https://x/2024-05-22-16.06.log.html)\n";
        let doc = parse_document(text).unwrap();
        assert!(doc.header.starts_with("<!-- auto-generated -->"));
    }

    #[test]
    fn insert_into_existing_year() {
        let text = "## 2024\n\n\
* May 22: agenda,\n          [summary](https://x/2024-05-22-16.06.html),\n          [logs](https://x/2024-05-22-16.06.log.html)\n";
        let mut doc = parse_document(text).unwrap();
        doc.insert(&meeting("2024-06-05T16:00:00"));
        assert_eq!(doc.sections[0].entries.len(), 2);
        assert_eq!(
            doc.sections[0].entries[0].date,
            NaiveDate::from_ymd_opt(2024, 6, 5).unwrap()
        );
        assert_eq!(
            doc.sections[0].entries[1].date,
            NaiveDate::from_ymd_opt(2024, 5, 22).unwrap()
        );
    }

    #[test]
    fn insert_creates_newer_year_section_first() {
        let text = "## 2024\n\n\
* May 22: agenda,\n          [summary](https://x/2024-05-22-16.06.html),\n          [logs](https://x/2024-05-22-16.06.log.html)\n";
        let mut doc = parse_document(text).unwrap();
        doc.insert(&meeting("2025-01-15T16:00:00"));
        assert_eq!(doc.sections[0].year, 2025);
        assert_eq!(doc.sections[1].year, 2024);
    }

    #[test]
    fn insert_creates_older_year_section_last() {
        let text = "## 2024\n\n\
* May 22: agenda,\n          [summary](https://x/2024-05-22-16.06.html),\n          [logs](https://x/2024-05-22-16.06.log.html)\n";
        let mut doc = parse_document(text).unwrap();
        doc.insert(&meeting("2022-01-15T16:00:00"));
        assert_eq!(doc.sections[0].year, 2024);
        assert_eq!(doc.sections[1].year, 2022);
    }

    #[test]
    fn render_entry_format() {
        let m = meeting("2024-05-22T16:06:00");
        let s = render_entry(&m);
        assert!(s.starts_with("* May 22: [summary]("));
        assert!(s.contains("[summary](https://meetbot.example/s/2024-05-22T16:06:00),\n"));
        assert!(s.contains("[logs](https://meetbot.example/l/2024-05-22T16:06:00)\n"));
        assert!(s.ends_with(")\n"));
        assert!(!s.contains("agenda"));
    }

    #[test]
    fn render_normalizes_heading_level_to_h3() {
        // Existing `## YYYY` headings are rewritten to `### YYYY`
        // so the meetings list nests correctly under the docs'
        // `## Meeting minutes` parent heading.
        let input = "## 2024\n\n\
* May 22: agenda,\n          [summary](https://x/2024-05-22-16.06.html),\n          [logs](https://x/2024-05-22-16.06.log.html)\n\n\
## 2023\n\n\
* Dec 20: agenda,\n          [summary](https://y/2023-12-20-16.00.html),\n          [logs](https://y/2023-12-20-16.00.log.html)\n";
        let expected = input
            .replace("## 2024", "### 2024")
            .replace("## 2023", "### 2023");
        let doc = parse_document(input).unwrap();
        let out = doc.render();
        assert_eq!(out, expected);
    }

    #[test]
    fn insert_preserves_legacy_entries_verbatim() {
        // Legacy entries (pre-MANAGED_FROM_YEAR) with hand-curated
        // `[agenda](...)` links survive a roundtrip untouched.
        let text = "## 2023\n\n\
* Jan 18: [agenda](https://hackmd.io/KLIda-WkRSidyKtfGZHKvg),\n          [summary](https://y/2023-01-18-16.00.html),\n          [logs](https://y/2023-01-18-16.00.log.html)\n";
        let doc = parse_document(text).unwrap();
        let out = doc.render();
        assert!(out.contains("[agenda](https://hackmd.io/KLIda-WkRSidyKtfGZHKvg)"));
    }

    #[test]
    fn parse_empty_document() {
        let doc = parse_document("").unwrap();
        assert_eq!(doc.header, "");
        assert_eq!(doc.sections.len(), 0);
    }

    #[test]
    fn insert_into_empty_document() {
        let mut doc = parse_document("").unwrap();
        doc.insert(&meeting("2026-04-22T15:08:00"));
        let out = doc.render();
        assert!(out.contains("### 2026\n"));
        assert!(out.contains("* Apr 22: [summary]("));
        assert!(!out.contains("agenda"));
    }
}
