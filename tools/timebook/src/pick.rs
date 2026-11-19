// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Which meeting a command names: one of its entries, by id or page, or
//! its calendar, `--meeting` picking one of the calendar's meetings by
//! title when it holds several.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use sandogasa_fedocal::Meeting;

/// A meeting as named on the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Named {
    /// One of its entries.
    Entry(u64),
    /// Its calendar.
    Calendar(String),
}

/// What `text` names: an entry's id (`11067`) or page (a link, or
/// anything with `/meeting/`), or else a calendar
/// (`centos-sig-hyperscale`).
pub fn named(text: &str) -> Result<Named, String> {
    let text = text.trim();
    if text.is_empty() {
        Err("name the meeting's calendar, or an entry's id or page".into())
    } else if text.starts_with("http")
        || text.contains("/meeting/")
        || text.chars().all(|c| c.is_ascii_digit())
    {
        crate::edit::entry_id(text).map(Named::Entry)
    } else {
        Ok(Named::Calendar(text.to_string()))
    }
}

/// The title of the meeting among `meetings` in `calendar` that
/// `meeting` picks: part of its title, in any case, a whole title
/// winning over a part; any meeting when not given. An error listing the
/// calendar's meetings `when` (the period `meetings` cover, as a phrase)
/// when none or several fit.
pub fn title(
    meetings: &[Meeting],
    calendar: &str,
    meeting: Option<&str>,
    when: &str,
) -> Result<String, String> {
    // Per title (in lowercase), as written and when it meets.
    let mut titles: BTreeMap<String, (String, BTreeSet<String>)> = BTreeMap::new();
    for m in meetings.iter().filter(|m| m.calendar_name == calendar) {
        let entry = titles
            .entry(m.meeting_name.trim().to_lowercase())
            .or_insert_with(|| (m.meeting_name.trim().to_string(), BTreeSet::new()));
        if let Some(at) = m.starts_at() {
            entry.1.insert(at.format("%a %H:%M UTC").to_string());
        }
    }
    let list = |keys: &[&String]| {
        keys.iter()
            .map(|k| {
                let (title, times) = &titles[*k];
                let times: Vec<&str> = times.iter().map(String::as_str).collect();
                format!("\"{title}\" ({})", times.join(", "))
            })
            .collect::<Vec<_>>()
            .join("; ")
    };
    let all: Vec<&String> = titles.keys().collect();
    let fits: Vec<&String> = match meeting.map(|m| m.trim().to_lowercase()) {
        None => all.clone(),
        Some(want) if titles.contains_key(&want) => vec![titles.get_key_value(&want).unwrap().0],
        Some(want) => all.iter().copied().filter(|k| k.contains(&want)).collect(),
    };
    match fits.as_slice() {
        [one] => Ok(titles[*one].0.clone()),
        [] if all.is_empty() => Err(format!("calendar `{calendar}` has no meetings {when}")),
        [] => Err(format!(
            "no meeting in `{calendar}` {when} is titled like \"{}\"; it has {}",
            meeting.unwrap_or_default(),
            list(&all)
        )),
        several => Err(format!(
            "`{calendar}` has several meetings {when}: {}; pick one with --meeting",
            list(several)
        )),
    }
}

/// The entry holding the next occurrence, from `now`, of the meeting
/// `meeting` picks in `calendar` (as [`title`] picks it); an error when
/// its coming occurrences are in several entries, which a change to one
/// would leave as they are.
pub fn next_entry(
    meetings: &[Meeting],
    calendar: &str,
    meeting: Option<&str>,
    now: DateTime<Utc>,
    when: &str,
) -> Result<u64, String> {
    let coming: Vec<Meeting> = meetings
        .iter()
        .filter(|m| m.starts_at().is_some_and(|t| t >= now))
        .cloned()
        .collect();
    let title = title(&coming, calendar, meeting, when)?;
    let mut entries: Vec<&Meeting> = coming
        .iter()
        .filter(|m| {
            m.calendar_name == calendar && m.meeting_name.trim().eq_ignore_ascii_case(&title)
        })
        .collect();
    entries.sort_by_key(|m| m.starts_at());
    let ids: Vec<u64> = entries.iter().fold(Vec::new(), |mut ids, m| {
        if !ids.contains(&m.meeting_id) {
            ids.push(m.meeting_id);
        }
        ids
    });
    match ids.as_slice() {
        [id] => Ok(*id),
        [] => Err(format!("no coming occurrence of \"{title}\"")),
        several => {
            let ids: Vec<String> = several.iter().map(|i| i.to_string()).collect();
            Err(format!(
                "\"{title}\" is booked {when} as entries {}; change each by its id",
                ids.join(", ")
            ))
        }
    }
}

/// `text` as one shell word: as is when nothing in it is special, else
/// in single quotes.
pub fn shell_word(text: &str) -> String {
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@+,=".contains(c));
    if plain {
        text.to_string()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meeting(id: u64, cal: &str, name: &str, date: &str, time: &str) -> Meeting {
        serde_json::from_value(serde_json::json!({
            "meeting_id": id, "calendar_name": cal, "meeting_name": name,
            "meeting_date": date, "meeting_date_end": date,
            "meeting_time_start": time, "meeting_time_stop": time,
            "meeting_timezone": "UTC"
        }))
        .unwrap()
    }

    fn sigs() -> Vec<Meeting> {
        vec![
            meeting(1, "SIGs", "AI/ML SIG", "2026-11-05", "17:30:00"),
            meeting(2, "SIGs", "riscv sig", "2026-11-04", "16:00:00"),
            meeting(
                3,
                "SIGs",
                "RISC-V SIG Office Hours",
                "2026-11-06",
                "14:00:00",
            ),
            meeting(4, "SIGs", "AI/ML SIG", "2026-11-12", "17:30:00"),
            meeting(9, "hs", "Hyperscale SIG", "2026-11-04", "16:00:00"),
        ]
    }

    #[test]
    fn a_meeting_is_named_by_entry_or_calendar() {
        assert_eq!(named("11067"), Ok(Named::Entry(11067)));
        assert_eq!(
            named("https://calendar.fedoraproject.org/meeting/11067/"),
            Ok(Named::Entry(11067))
        );
        assert_eq!(
            named(" centos-sig-hyperscale "),
            Ok(Named::Calendar("centos-sig-hyperscale".into()))
        );
        assert!(named(" ").is_err());
        assert!(named("https://example.org/x/").is_err());
        assert!(named("../meeting/x/").is_err());
        assert_eq!(named("a/b"), Ok(Named::Calendar("a/b".into())));
    }

    #[test]
    fn a_calendar_with_one_meeting_needs_no_title() {
        assert_eq!(
            title(&sigs(), "hs", None, "then").unwrap(),
            "Hyperscale SIG"
        );
        let err = title(&sigs(), "SIGs", None, "then").unwrap_err();
        assert!(
            err.contains("several meetings then: \"AI/ML SIG\" (Thu 17:30 UTC)")
                && err.ends_with("pick one with --meeting"),
            "{err}"
        );
        let err = title(&sigs(), "nowhere", None, "then").unwrap_err();
        assert_eq!(err, "calendar `nowhere` has no meetings then");
    }

    #[test]
    fn meeting_picks_by_part_of_the_title_and_a_whole_one_wins() {
        assert_eq!(
            title(&sigs(), "SIGs", Some("ai/ml"), "then").unwrap(),
            "AI/ML SIG"
        );
        // "sig" alone fits all three.
        assert_eq!(
            title(&sigs(), "SIGs", Some("RISCV SIG"), "then").unwrap(),
            "riscv sig"
        );
        assert!(title(&sigs(), "SIGs", Some("sig"), "then").is_err());
        let err = title(&sigs(), "SIGs", Some("eln"), "then").unwrap_err();
        assert!(
            err.starts_with(
                "no meeting in `SIGs` then is titled like \"eln\"; it has \"AI/ML SIG\""
            ),
            "{err}"
        );
    }

    #[test]
    fn the_next_entry_is_the_coming_occurrence() {
        let now = DateTime::parse_from_rfc3339("2026-11-06T00:00:00Z")
            .unwrap()
            .to_utc();
        assert_eq!(
            next_entry(&sigs(), "SIGs", Some("ai/ml"), now, "then"),
            Ok(4)
        );
        assert_eq!(
            next_entry(&sigs(), "SIGs", Some("office"), now, "then"),
            Ok(3)
        );
        // The riscv SIG met on the 4th, and has not come since.
        let err = next_entry(&sigs(), "SIGs", Some("riscv sig"), now, "then").unwrap_err();
        assert!(err.contains("titled like \"riscv sig\""), "{err}");
        assert!(next_entry(&sigs(), "hs", None, now, "then").is_err());
        // A cancelled occurrence splits a series: a change to the next
        // entry would miss the later one.
        let mut split = sigs();
        split.push(meeting(5, "SIGs", "AI/ML SIG", "2026-11-26", "17:30:00"));
        let err = next_entry(&split, "SIGs", Some("ai/ml"), now, "then").unwrap_err();
        assert_eq!(
            err,
            "\"AI/ML SIG\" is booked then as entries 4, 5; change each by its id"
        );
    }

    #[test]
    fn shell_words_are_quoted_when_needed() {
        assert_eq!(shell_word("centos-sig-hyperscale"), "centos-sig-hyperscale");
        assert_eq!(shell_word("AI/ML SIG"), "'AI/ML SIG'");
        assert_eq!(shell_word("Bob's $call"), r"'Bob'\''s $call'");
        assert_eq!(shell_word(""), "''");
    }
}
